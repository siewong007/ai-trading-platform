#![allow(dead_code)] // consumed by upcoming CLI/executor wiring
//! Glue: bus -> cache market-data pipeline with gap healing from sqlite.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::bus::Bus;
use crate::cache::Cache;
use crate::db::Db;
use crate::types::Candle;

/// Subscribe `kline.*` on the bus and upsert every payload into the cache.
/// Payload contract: serde JSON of [`Candle`] (see types.rs).
pub fn spawn_bus_to_cache(bus: Arc<Bus>, cache: Arc<Cache>) -> JoinHandle<()> {
    let mut rx = bus.subscribe("kline.*");
    tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            match serde_json::from_str::<Candle>(&payload) {
                Ok(c) => {
                    let symbol = payload_symbol(&payload);
                    cache.upsert(&c, &symbol, "1h");
                }
                Err(e) => tracing::warn!("cache pipeline dropped bad payload: {e}"),
            }
        }
    })
}

fn payload_symbol(payload: &str) -> String {
    // topic travels separately; embed symbol in payload when publishing so the
    // cache key is unambiguous. Fallback: parse attempt of a wrapper object.
    #[derive(serde::Deserialize)]
    struct WithSymbol {
        #[serde(default)]
        symbol: Option<String>,
        #[serde(flatten)]
        candle: Candle,
    }
    serde_json::from_str::<WithSymbol>(payload)
        .ok()
        .and_then(|w| w.symbol)
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

/// Ensure the cache's series for `(symbol, timeframe)` has no hole up to
/// `expected_last_open_ms`: backfills missing candles from sqlite.
pub async fn heal_gaps(
    cache: &Cache,
    db: &Db,
    symbol: &str,
    timeframe: &str,
    tf_ms: i64,
    expected_last_open_ms: i64,
) -> anyhow::Result<usize> {
    let last = cache.series(symbol, timeframe).last().map(|c| c.open_time);
    let Some(last_ts) = last else {
        return cache.hydrate_from_db(db, symbol, timeframe).await;
    };
    if expected_last_open_ms - last_ts <= tf_ms {
        return Ok(0); // continuous
    }
    let healed = cache.hydrate_from_db(db, symbol, timeframe).await?;
    tracing::info!(
        "{symbol}: gap {last_ts}..{expected_last_open_ms} healed from db ({healed} rows)"
    );
    tokio::time::sleep(Duration::from_millis(0)).await;
    Ok(healed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(t: i64) -> Candle {
        Candle {
            open_time: t,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
        }
    }

    #[tokio::test]
    async fn published_candles_land_in_cache() {
        let bus = Arc::new(Bus::new());
        let cache = Arc::new(Cache::new());
        spawn_bus_to_cache(bus.clone(), cache.clone());
        let payload = r#"{"symbol":"BTCUSDT","open_time":1000,"open":1.0,"high":2.0,"low":0.5,"close":1.5,"volume":10.0}"#;
        bus.publish("kline.BTCUSDT", payload);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(cache.len("BTCUSDT", "1h"), 1);
        assert_eq!(cache.series("BTCUSDT", "1h")[0].open_time, 1000);
    }

    #[tokio::test]
    async fn heal_gaps_backfills_from_db_when_behind() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let mk = |t: i64| Candle {
            open_time: t,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 3.0,
        };
        db.upsert_klines("TESTUSDT", "1h", &[mk(1000), mk(2000), mk(3000)])
            .await
            .unwrap();
        let cache = Cache::new();
        cache.upsert(&mk(1000), "TESTUSDT", "1h"); // behind by TWO candles
        let n = heal_gaps(&cache, &db, "TESTUSDT", "1h", 1000, 3000)
            .await
            .unwrap();
        assert_eq!(n, 3);
        assert_eq!(cache.len("TESTUSDT", "1h"), 3);
        // continuous case (cache holds the latest closed candle): no-op
        let n2 = heal_gaps(&cache, &db, "TESTUSDT", "1h", 1000, 3000)
            .await
            .unwrap();
        assert_eq!(n2, 0);
    }
}

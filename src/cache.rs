use std::collections::HashMap;
use std::sync::Mutex;

use crate::db::Db;
use crate::types::Candle;

/// Max candles retained per (symbol, timeframe) series; oldest evicted beyond this.
const MAX_SERIES_LEN: usize = 5000;

/// Latest-candle market-data cache keyed by (symbol, timeframe), fed by live
/// bus subscriptions and hydrated from sqlite. Series stay time-sorted by
/// `open_time`; same-timestamp upserts replace in place.
pub struct Cache {
    inner: Mutex<HashMap<(String, String), Vec<Candle>>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Insert or replace `candle`, keeping the series sorted by `open_time`.
    /// Evicts the oldest candle once the series exceeds the cap.
    pub fn upsert(&self, candle: &Candle, symbol: &str, timeframe: &str) {
        let mut map = self.inner.lock().unwrap();
        let series = map
            .entry((symbol.to_string(), timeframe.to_string()))
            .or_default();
        match series.binary_search_by_key(&candle.open_time, |c| c.open_time) {
            Ok(idx) => series[idx] = *candle,
            Err(idx) => series.insert(idx, *candle),
        }
        if series.len() > MAX_SERIES_LEN {
            let excess = series.len() - MAX_SERIES_LEN;
            series.drain(..excess);
        }
    }

    /// Cloned copy of the stored series, time-sorted ascending.
    pub fn series(&self, symbol: &str, timeframe: &str) -> Vec<Candle> {
        self.inner
            .lock()
            .unwrap()
            .get(&(symbol.to_string(), timeframe.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    /// Number of cached candles for `(symbol, timeframe)`.
    pub fn len(&self, symbol: &str, timeframe: &str) -> usize {
        self.inner
            .lock()
            .unwrap()
            .get(&(symbol.to_string(), timeframe.to_string()))
            .map_or(0, Vec::len)
    }

    /// Backfill from sqlite (rows arrive time-sorted). Returns the row count.
    pub async fn hydrate_from_db(
        &self,
        db: &Db,
        symbol: &str,
        timeframe: &str,
    ) -> anyhow::Result<usize> {
        let candles = db.load_klines(symbol, timeframe).await?;
        let count = candles.len();
        for c in &candles {
            self.upsert(c, symbol, timeframe);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(t: i64, close: f64) -> Candle {
        Candle {
            open_time: t,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 10.0,
        }
    }

    #[test]
    fn out_of_order_upserts_stay_sorted() {
        let cache = Cache::new();
        cache.upsert(&mk(3000, 103.0), "BTCUSDT", "1h");
        cache.upsert(&mk(1000, 101.0), "BTCUSDT", "1h");
        cache.upsert(&mk(2000, 102.0), "BTCUSDT", "1h");
        let ts: Vec<i64> = cache.series("BTCUSDT", "1h").iter().map(|c| c.open_time).collect();
        assert_eq!(ts, vec![1000, 2000, 3000]);
        assert_eq!(cache.len("BTCUSDT", "1h"), 3);
    }

    #[test]
    fn duplicate_open_time_replaced_not_appended() {
        let cache = Cache::new();
        cache.upsert(&mk(1000, 101.0), "BTCUSDT", "1h");
        cache.upsert(&mk(1000, 999.0), "BTCUSDT", "1h");
        let series = cache.series("BTCUSDT", "1h");
        assert_eq!(series.len(), 1);
        assert!((series[0].close - 999.0).abs() < 1e-9);
    }

    #[test]
    fn cap_evicts_oldest_at_5000() {
        let cache = Cache::new();
        for t in 0..(MAX_SERIES_LEN as i64 + 10) {
            cache.upsert(&mk(t, t as f64), "BTCUSDT", "1m");
        }
        assert_eq!(cache.len("BTCUSDT", "1m"), MAX_SERIES_LEN);
        let series = cache.series("BTCUSDT", "1m");
        assert_eq!(series[0].open_time, 10); // 0..10 evicted
        assert_eq!(series.last().unwrap().open_time, MAX_SERIES_LEN as i64 + 9);
        assert!(series.windows(2).all(|w| w[0].open_time < w[1].open_time));
    }

    #[tokio::test]
    async fn hydrate_returns_row_count_and_matches_db_order() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let candles: Vec<Candle> = (0..5).rev().map(|i| mk(i * 1000, 100.0)).collect();
        db.upsert_klines("BTCUSDT", "1h", &candles).await.unwrap();

        let cache = Cache::new();
        assert_eq!(cache.len("BTCUSDT", "1h"), 0);
        let n = cache.hydrate_from_db(&db, "BTCUSDT", "1h").await.unwrap();
        assert_eq!(n, 5);

        let series = cache.series("BTCUSDT", "1h");
        assert_eq!(series.len(), 5);
        assert!(series.windows(2).all(|w| w[0].open_time < w[1].open_time));

        // Unknown series hydrates zero rows and stays absent.
        assert_eq!(
            cache.hydrate_from_db(&db, "ETHUSDT", "1h").await.unwrap(),
            0
        );
        assert_eq!(cache.len("ETHUSDT", "1h"), 0);
    }
}

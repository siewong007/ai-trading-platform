#![allow(dead_code)] // consumed by upcoming CLI/executor wiring
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::bus::Bus;
use crate::types::Candle;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

static PARSE_ERRORS: AtomicU64 = AtomicU64::new(0);

/// Total kline frames rejected as malformed since process start.
pub fn parse_errors() -> u64 {
    PARSE_ERRORS.load(Ordering::Relaxed)
}

/// One subscribed market: `{symbol}@kline_{timeframe}`.
#[derive(Debug, Clone)]
pub struct KlineStream {
    pub symbol: String,
    pub timeframe: String,
}

/// Combined-stream kline frame -> `(SYMBOL, Candle)` for CLOSED candles only
/// (`k.x == true`). Open candles yield `None` silently; any malformed frame
/// yields `None`, logs a warning, and bumps [`parse_errors`]. Never panics.
pub fn parse_kline_frame(frame: &str) -> Option<(String, Candle)> {
    match decode(frame) {
        Ok(parsed) => parsed,
        Err(e) => {
            PARSE_ERRORS.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("malformed kline frame dropped: {e}");
            None
        }
    }
}

fn decode(frame: &str) -> anyhow::Result<Option<(String, Candle)>> {
    let v: serde_json::Value = serde_json::from_str(frame)?;
    let k = v
        .pointer("/data/k")
        .context("frame has no data.k object")?;
    if k.get("x").and_then(serde_json::Value::as_bool) != Some(true) {
        return Ok(None); // candle not closed yet
    }
    let num = |field: &str| -> anyhow::Result<f64> {
        k.get(field)
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("missing k.{field}"))?
            .parse()
            .with_context(|| format!("bad k.{field}: not a number"))
    };
    let candle = Candle {
        open_time: k
            .get("t")
            .and_then(serde_json::Value::as_i64)
            .context("missing k.t")?,
        open: num("o")?,
        high: num("h")?,
        low: num("l")?,
        close: num("c")?,
        volume: num("v")?,
    };
    let symbol = v
        .pointer("/data/s")
        .and_then(serde_json::Value::as_str)
        .map(str::to_uppercase)
        .or_else(|| {
            v.get("stream")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.split('@').next())
                .map(str::to_uppercase)
        })
        .context("frame has no symbol")?;
    Ok(Some((symbol, candle)))
}

/// Connect to `{base_ws}/stream?streams=a@kline_tf/b@kline_tf`, publish every
/// closed candle to `kline.SYMBOL` on the bus, and reconnect forever with
/// bounded backoff (1s, ×2, cap 30s; reset after a healthy session).
/// Transport errors and malformed frames are logged, never fatal, never panic.
pub async fn run_kline_stream(
    base_ws: &str,
    streams: Vec<KlineStream>,
    bus: Arc<Bus>,
) -> anyhow::Result<()> {
    anyhow::ensure!(!streams.is_empty(), "no streams requested");
    let url = format!(
        "{}/stream?streams={}",
        base_ws.trim_end_matches('/'),
        streams
            .iter()
            .map(|s| format!("{}@kline_{}", s.symbol.to_lowercase(), s.timeframe))
            .collect::<Vec<_>>()
            .join("/")
    );
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match session(&url, &bus).await {
            Ok(healthy) if healthy => backoff = INITIAL_BACKOFF,
            Ok(_) => {}
            Err(e) => tracing::warn!("kline feed disconnected: {e:#}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    }
}

/// One connection lifetime; returns whether any frame arrived on it.
async fn session(url: &str, bus: &Bus) -> anyhow::Result<bool> {
    let (ws, _) = connect_async(url).await?;
    let (_, mut read) = ws.split();
    let mut got_frames = false;
    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(text) => {
                got_frames = true;
                if let Some((symbol, candle)) = parse_kline_frame(text.as_str()) {
                    bus.publish(
                        &format!("kline.{symbol}"),
                        &serde_json::json!({
                            "symbol": symbol,
                            "open_time": candle.open_time,
                            "open": candle.open,
                            "high": candle.high,
                            "low": candle.low,
                            "close": candle.close,
                            "volume": candle.volume,
                        })
                        .to_string(),
                    );
                }
            }
            Message::Close(_) => break,
            _ => {} // ping/pong/binary/raw frames are irrelevant here
        }
    }
    Ok(got_frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOSED_FRAME: &str = r#"{"stream":"btcusdt@kline_1h","data":{"e":"kline","E":1700000000000,"s":"BTCUSDT","k":{"t":1699996800000,"T":1700000399999,"s":"BTCUSDT","i":"1h","f":100,"L":1600,"o":"34000.10000000","c":"34100.50000000","h":"34200.00000000","l":"33950.25000000","v":"1250.50000000","n":1500,"x":true,"q":"42651250.00000000","V":"700.00000000","Q":"23910000.00000000","B":"0"}}}"#;

    #[test]
    fn parses_closed_combined_frame_into_candle_and_symbol() {
        let before = parse_errors();
        let (symbol, candle) = parse_kline_frame(CLOSED_FRAME).expect("closed candle parsed");
        assert_eq!(symbol, "BTCUSDT");
        assert_eq!(
            candle,
            Candle {
                open_time: 1_699_996_800_000,
                open: 34_000.10,
                high: 34_200.00,
                low: 33_950.25,
                close: 34_100.50,
                volume: 1_250.5,
            }
        );
        assert_eq!(parse_errors(), before);
    }

    #[test]
    fn open_candle_is_ignored_without_counting_an_error() {
        let open_frame = CLOSED_FRAME.replace("\"x\":true", "\"x\":false");
        let before = parse_errors();
        assert_eq!(parse_kline_frame(&open_frame), None);
        assert_eq!(parse_errors(), before);
    }

    #[test]
    fn garbage_frames_never_panic_and_bump_counter() {
        let garbage = [
            "not json at all",
            r#"{"stream":"btcusdt@kline_1h"}"#,
            r#"{"stream":"btcusdt@kline_1h","data":{"k":{"t":"NaN","o":"1","c":"1","h":"1","l":"1","v":"1","x":true}}}"#,
        ];
        for frame in garbage {
            let before = parse_errors();
            assert_eq!(parse_kline_frame(frame), None);
            assert_eq!(parse_errors(), before + 1, "frame: {frame}");
        }
    }

    #[tokio::test]
    #[ignore = "live network test — manual run only; waits up to ~2 min for one closed 1m candle"]
    async fn live_feed_publishes_one_closed_candle_to_bus() {
        let bus = Arc::new(Bus::new());
        let mut rx = bus.subscribe("kline.BTCUSDT");
        let task = tokio::spawn(run_kline_stream(
            "wss://stream.binance.com:9443",
            vec![KlineStream {
                symbol: "BTCUSDT".into(),
                timeframe: "1m".into(),
            }],
            bus.clone(),
        ));
        let payload = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("timed out waiting for a closed candle")
            .expect("bus channel closed unexpectedly");
        assert!(payload.contains("\"symbol\":\"BTCUSDT\""), "{payload}");
        assert!(payload.contains("\"open_time\""), "{payload}");
        task.abort();
    }
}

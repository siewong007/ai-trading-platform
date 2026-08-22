# Phase 3a: Core Infrastructure — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Foundational event infrastructure — in-process message bus, market-data cache, Binance WebSocket kline feed, and extended order TIF/execInstruction support — so multi-venue/event-driven evolution has a base.

**Architecture:** `bus.rs`: synchronous typed topic pub/sub (String topics, `serde_json::Value` payloads; subscribers = tokio mpsc channels). `cache.rs`: latest-candle-per-symbol cache fed by bus subscription + sqlite backfill. `ws.rs`: Binance public kline stream client (tokio-tungstenite, rustls), auto-reconnect w/ exponential backoff, publishes parsed candles to the bus; gap-heals from sqlite. `signed.rs` gains `TimeInForce` + `ExecInstruction` enums mapped onto Binance params (GTC/IOC/FOK; post-only=LIMIT_MAKER).

**Tech Stack:** existing stack + `tokio-tungstenite = { version = "0.28", features = ["rustls-tls-webpki-roots"] }`, `futures-util = "0.3"`.

## Global Constraints

- No blocking calls inside async tasks (bus send uses try_send/async aware)
- WS reconnect must be bounded-backoff (start 1s, ×2, cap 30s) and never panic on malformed frames
- All new types Send+Sync; no unsafe
- Keys/secrets never logged (unchanged)
- Every task ends green (`cargo test`) and committed
- Existing CLI behavior unchanged (fetch/backtest/search/export/trade keep working)

---

### Task 1: Message bus

**Files:** Create `src/bus.rs`; Modify `src/main.rs` (`mod bus;`); Test: inline `#[cfg(test)]`

**Interfaces (produces):**
```rust
pub struct Bus { /* inner Mutex<HashMap<String, Vec<mpsc::Sender<String>>>> */ }
impl Bus {
    pub fn new() -> Self
    pub fn subscribe(&self, topic_pattern: &str) -> mpsc::Receiver<String> // pattern: exact topic or "prefix.*"
    pub fn publish(&self, topic: &str, payload: &str)                    // fan-out, drops on closed/full channels (never blocks)
    pub fn subscriber_count(&self, topic: &str) -> usize
}
```
Topics are plain strings; `kline.BTCUSDT` style. Wildcard `*` suffix matches any suffix.

- [ ] Step 1: failing tests — exact-topic delivery; prefix-wildcard delivery; no subscribers = silent drop; closed-receiver cleanup (subscriber_count shrinks after receiver dropped); slow receiver with full channel (capacity 64) drops oldest? NO — drops NEW message and continues (documented).
- [ ] Step 2: implement; Step 3: `cargo test bus` green; Step 4: commit `Task 1: message bus`

### Task 2: Market-data cache

**Files:** Create `src/cache.rs`; Modify `src/main.rs` (`mod cache;`)

**Interfaces:**
```rust
pub struct Cache { /* Mutex<HashMap<(String symbol, String tf), Vec<Candle>>> capped at 5000 candles/symbol */ }
impl Cache {
    pub fn new() -> Self
    pub fn upsert(&self, candle: &Candle, symbol: &str, timeframe: &str)   // sorted insert by open_time; replaces same ts
    pub fn series(&self, symbol: &str, timeframe: &str) -> Vec<Candle>     // clone, time-sorted
    pub fn len(&self, symbol: &str, timeframe: &str) -> usize
    pub async fn hydrate_from_db(&self, db: &Db, symbol: &str, timeframe: &str) -> anyhow::Result<usize>
}
```
- [ ] Step 1: failing tests — out-of-order upsert stays sorted; duplicate open_time replaced not appended; cap at 5000 evicts oldest; hydrate returns row count and matches db order.
- [ ] Step 2: implement; Step 3: green; Step 4: commit `Task 2: market-data cache`

### Task 3: WebSocket kline feed

**Files:** Create `src/ws.rs`; Modify `src/main.rs` (`mod ws;`); Cargo.toml deps above

**Interfaces:**
```rust
pub struct KlineStream { pub symbol: String, pub timeframe: String }
pub async fn run_kline_stream(base_ws: &str, streams: Vec<KlineStream>, bus: std::sync::Arc<Bus>) -> anyhow::Result<()>
// URL form: wss://stream.binance.com:9443/stream?streams=btcusdt@kline_1h/ethusdt@kline_1h
// Combined-stream frames: {"stream":"btcusdt@kline_1h","data":{"e":"kline","k":{...,"t":ms,"o":"..","c":".."}}}
// On CLOSED candle (k.x == true): parse into Candle{open_time:t, o,h,l,c,v} and bus.publish("kline.SYMBOL", json)
```
Reconnect loop with backoff 1s×2 cap 30s; malformed frame => warn+continue; NEVER panic. Parse failures counted in an AtomicU64 exposed by `pub fn parse_errors() -> u64`.

- [ ] Step 1: failing tests — combined-frame parser extracts closed candle correctly (real sample JSON literal in test); ignores k.x==false; garbage frame does not panic and bumps counter. Parser extracted as `pub fn parse_kline_frame(frame: &str) -> Option<(String, Candle)>` so tests run offline.
- [ ] Step 2: implement parser + connect loop; Step 3: green + `#[ignore]` live test hitting real Binance WS for one closed candle (manual); Step 4: commit `Task 3: websocket kline feed`

### Task 4: TIF + exec instructions

**Files:** Modify `src/signed.rs`; Test: inline

**Interfaces:**
```rust
pub enum TimeInForce { Gtc, Ioc, Fok }          // Display -> "GTC"|"IOC"|"FOK"
pub enum ExecInstruction { Default, PostOnly }   // PostOnly => order type LIMIT_MAKER (Binance spot)
pub struct OrderSpec { pub qty: f64, pub limit_price: f64, pub tif: TimeInForce, pub instruction: ExecInstruction }
pub async fn place_limit(&self, symbol:&str, spec:&OrderSpec, client_id:&str) -> anyhow::Result<PlacedOrder>
// place_limit_buy retained as thin wrapper calling place_limit(Gtc, Default) for compatibility
```
- [ ] Step 1: failing wiremock tests — IOC posts timeInForce=IOC; FOK likewise; PostOnly emits type=LIMIT_MAKER and omits timeInForce (Binance rejects TIF on LIMIT_MAKER); default wrapper unchanged behavior (existing tests still pass).
- [ ] Step 2: implement; Step 3: green; Step 4: commit `Task 4: time-in-force + exec instructions`

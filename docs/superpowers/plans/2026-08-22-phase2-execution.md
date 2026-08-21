# Phase 2: Execution Layer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the execution layer per spec §6–§8 — signed REST client, risk manager, executor state machine with reconciliation, `trade` CLI (testnet-default) — plus smoke suite and a shipped v0.2.0 release.

**Architecture:** New modules `signed.rs` (HMAC signing + private endpoints), `risk.rs` (pure risk rails), `executor.rs` (state machine + reconciliation). Fill detection via REST polling of open orders + myTrades every ~30s (documented deviation from spec §6's WS stream: signals are hourly; polling is sufficient and dependency-free — WS deferred to v1.1). Kill switch = `flatten` CLI subcommand (dashboard/Telegram-command deferred). Telegram alerts optional via env vars.

**Tech Stack:** existing tokio/reqwest(rustls)/serde/sqlx/clap/tracing/wiremock + new `hmac`, `sha2`, `hex`.

## Global Constraints (from spec rev 2)

- Fixed-dollar mode at launch: $2 risk/trade, max 1 open position, notional cap ≤ 50% equity, skip if notional < $15
- Daily halt: −2% day or 2 consecutive stop-outs, whichever first
- Daily −3% equity loss → flatten all, halt until manual reset
- Stale data (> 2 missed candles) → refuse to act
- Lot-size / step-size / min-notional compliance enforced by executor pre-submit
- Entry: LIMIT idempotent client ID → fill detected → place OCO (TP limit + stop-loss leg)
- Flatten-all ordering: cancel all open orders → confirm cancellations → market-reduce positions → verify balances. Never market-sell while an OCO can trigger mid-sequence.
- Secrets in `.env` (gitignored); keys never logged, never stored in DB
- Testnet (`https://testnet.binance.vision`) is the default trading base; live requires explicit flag + typed confirmation
- The pre-registered gate verdict (currently NO-GO) must be printed at every `trade` launch
- Every task ends green (`cargo test`) and committed

---

### Task 1: Signed request layer

**Files:** Create `src/signed.rs`; Modify `Cargo.toml` (add `hmac = "0.12"`, `sha2 = "0.10"`, `hex = "0.4"`); Modify `src/main.rs` (`mod signed;`)

**Interfaces:**
- Consumes: nothing new
- Produces:
```rust
pub struct Keys { pub api_key: String, pub secret: String } // never Debug/Display-derived
impl Keys { pub fn from_env() -> anyhow::Result<Keys> } // reads BINANCE_API_KEY, BINANCE_API_SECRET; error names the missing var, never its value
pub struct SignedClient { /* wraps reqwest::Client + base + keys */ }
impl SignedClient {
    pub fn new(base: &str, keys: Keys) -> anyhow::Result<Self>
    pub async fn signed_get(&self, path: &str, params: &[(&str, &str)]) -> anyhow::Result<String>
    pub async fn signed_post(&self, path: &str, params: &[(&str, &str)]) -> anyhow::Result<String>
    pub async fn signed_delete(&self, path: &str, params: &[(&str, &str)]) -> anyhow::Result<String>
}
pub fn sign_query(secret: &str, query: &str) -> String // hex HMAC-SHA256 — exposed pub for hand-vector tests
```

- [ ] **Step 1: Failing test** — hand-computed HMAC vector (RFC 4231 test case 2 key="Jefe", data="what do ya want for nothing?" → `5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843`) plus: params are sorted-free (joined in given order), timestamp+recvWindow auto-appended, signature appended last, error body surfaces Binance code (-1022 invalid signature, -1021 timestamp) as structured message.
- [ ] **Step 2:** `cargo test signed` → FAIL (module missing)
- [ ] **Step 3: Implement** — build query string `a=b&c=d&timestamp=<ms>&recvWindow=5000`, sign, append `&signature=...`; send with header `X-MBX-APIKEY`; on non-2xx read JSON `{code, msg}` and bail with `binance error {code}: {msg}`.
- [ ] **Step 4:** `cargo test` green
- [ ] **Step 5:** Commit `Task 1: signed request layer with HMAC-SHA256`

### Task 2: Account reads + exchange filters

**Files:** Extend `src/signed.rs`

**Interfaces:**
- Produces:
```rust
pub struct Balance { pub asset: String, pub free: f64, pub locked: f64 }
pub struct SymbolFilters { pub step_size: f64, pub min_qty: f64, pub min_notional: f64 }
pub struct OpenOrder { pub order_id: i64, pub client_order_id: String, pub symbol: String, pub side: String, pub otype: String, pub price: f64, pub orig_qty: f64, pub executed_qty: f64, pub status: String }
pub struct MyTrade { pub id: i64, pub order_id: i64, pub price: f64, pub qty: f64, pub commission: f64, pub time: i64 }
impl SignedClient {
    pub async fn balances(&self) -> anyhow::Result<Vec<Balance>>            // GET /api/v3/account
    pub async fn open_orders(&self, symbol: &str) -> anyhow::Result<Vec<OpenOrder>> // GET /api/v3/openOrders?symbol=
    pub async fn my_trades(&self, symbol: &str, limit: u32) -> anyhow::Result<Vec<MyTrade>> // GET /api/v3/myTrades
    pub async fn symbol_filters(base_url: &str, symbol: &str) -> anyhow::Result<SymbolFilters> // public GET /api/v3/exchangeInfo (no signature)
}
```
- [ ] **Step 1: Failing wiremock tests** — account JSON with walletDescription wrapper tolerated (testnet shape) and plain shape; openOrders array parse incl. zero-fill NEW order; exchangeInfo filters extraction (`LOT_SIZE.stepSize/minQty`, `NOTIONAL.minNotional`, falling back to legacy `MIN_NOTIONAL.minNotional`).
- [ ] **Step 2:** FAIL; **Step 3:** implement parsers (serde with `#[serde(default)]`, price/qty strings via `str::parse` like types.rs); **Step 4:** green; **Step 5:** Commit `Task 2: account reads + symbol filters`

### Task 3: Order placement (entry, OCO, cancel, market-reduce)

**Files:** Extend `src/signed.rs`

**Interfaces:**
- Produces:
```rust
pub struct PlacedOrder { pub order_id: i64, pub client_order_id: String, pub status: String, pub executed_qty: f64 }
impl SignedClient {
    pub async fn place_limit_buy(&self, symbol: &str, qty: f64, price: f64, client_id: &str) -> anyhow::Result<PlacedOrder> // POST /api/v3/order type=LIMIT timeInForce=GTC newClientOrderId
    pub async fn place_oco_sell(&self, symbol: &str, qty: f64, tp_price: f64, stop_price: f64, list_client_id: &str) -> anyhow::Result<String> // POST /api/v3/order/oco: aboveType=LIMIT_MAKER abovePrice=tp, belowType=STOP_LOSS_LIMIT belowStopPrice=stop belowPrice=round(stop*0.995) — returns orderListId
    pub async fn cancel_all_orders(&self, symbol: &str) -> anyhow::Result<()> // DELETE /api/v3/openOrders?symbol= ; treats "order list is empty"/-2011-ish empty responses as Ok
    pub async fn market_sell(&self, symbol: &str, qty: f64, client_id: &str) -> anyhow::Result<PlacedOrder>
    pub async fn get_order(&self, symbol: &str, client_order_id: &str) -> anyhow::Result<OpenOrder> // GET /api/v3/order?origClientOrderId=
}
pub fn round_qty_to_step(qty: f64, step_size: f64) -> f64 // floor to step, 8-decimal safe: ((qty/step).floor()*step*1e8).round()/1e8
```
- [ ] **Step 1: Failing wiremock tests** — limit buy posts correct params + client ID passthrough; OCO param set exact (above/below legs, prices formatted without exponent — use `format!("{:.8}", x)` trimmed of trailing zeros via helper `fmt_price`); cancel-all maps empty-list response to Ok; market sell parses fill; `round_qty_to_step(0.123456789, 0.0001) == 0.1234`, `round_qty_to_step(0.05, 0.001)==0.050`.
- [ ] **Step 2:** FAIL; **Step 3:** implement; **Step 4:** green; **Step 5:** Commit `Task 3: order placement — entry, OCO, cancel-all, market reduce`

### Task 4: Risk manager

**Files:** Create `src/risk.rs`; Modify `src/db.rs` (day-state helpers on config_state)

**Interfaces:**
- Consumes: `SymbolFilters`, `TradePlan`, `BacktestSection` (sizing fields)
- Produces:
```rust
pub struct DayState { pub day_key: String, pub day_start_equity: f64, pub consecutive_stopouts: u32, pub halted: bool, pub halt_reason: Option<String> }
pub enum RiskDecision { Place { qty: f64, entry_limit: f64 }, Skip(String), Halt(String) }
pub fn load_day_state(db: &Db, cfg_hash: &str) -> impl Future<Result<DayState>>   // db helpers get/set config_state
pub fn store_day_state(db: &Db, cfg_hash: &str, s: &DayState) -> impl Future<Result<()>>
pub fn roll_day_if_new(s: &mut DayState, now_utc_date: &str, current_equity: f64)
pub fn risk_pass(
    plan: &TradePlan, equity: f64, has_open_position: bool,
    bt: &BacktestSection, filters: &SymbolFilters, day: &DayState,
    last_trade_was_stopout: bool,
) -> RiskDecision
// rails in this order: halted→Halt; day loss ≤ -2% or (consecutive_stopouts>=2)→Halt("daily halt");
// -3% intraday → Halt("flatten required"); has_open_position→Skip("position open");
// qty = risk/(entry-stop) floored to step; notional=qty*entry > bt.max_notional_pct_equity*equity → shrink to cap;
// notional < bt.min_notional_usd → Skip("below min notional"); qty<=0 or stop>=entry → Skip("degenerate plan")
pub fn register_result(day: &mut DayState, pnl: f64, was_stopout: bool) // updates consecutive_stopouts, sets halted at thresholds
```
- [ ] **Step 1: Failing unit tests, hand-computed literals** — $200 equity, plan entry=100 stop=98 → qty=1.0 notional=100 > cap 100? equal → passes at exactly-cap; entry=101 stop=99 risk $2 → qty=1.0 notional=101>100 → shrunk qty=0.990099… assert `(qty*entry - 100).abs() < 1e-6`; wide stop → notional<$15 → Skip; day_start_equity 200, equity 196.1 (−1.95%) → ok; 195.9 (−2.05%) → Halt; two stopouts → Halt; −3% → Halt flatten reason; step rounding 0.123456789@step 0.001 → 0.123.
- [ ] **Step 2:** FAIL; **Step 3:** implement pure functions + db get/set wrappers; **Step 4:** green; **Step 5:** Commit `Task 4: risk manager — all spec §4 rails`

### Task 5: Executor state machine + reconciliation + flatten

**Files:** Create `src/executor.rs`; Modify `src/db.rs` (positions table accessors if missing: upsert position state keyed by config_hash+symbol)

**Interfaces:**
- Consumes: Tasks 1–4, `generate_signals`, `load_series` pattern from main.rs
- Produces:
```rust
pub struct Executor { pub sc: SignedClient, pub db: Db, pub strat: StrategySection, pub bt: BacktestSection, pub hash: String, pub dry_run: bool }
pub enum CycleOutcome { NoSignal, PlacedEntry { client_id: String }, AwaitingFill, PlacedOco { list_id: String }, PositionLive, Flat }
impl Executor {
    pub async fn reconcile(&self) -> anyhow::Result<String> // startup: fetch open_orders+balances; cancel orphan entries (entry client ids w/o OCO and stale); persist expected state; returns human summary
    pub async fn run_cycle(&self, now_ms: i64) -> anyhow::Result<CycleOutcome> // one pass of the §6 flow
    pub async fn flatten_all(&self) -> anyhow::Result<String> // spec ordering: cancel_all → confirm via open_orders empty → market_sell remaining base balance above dust → verify balances; NEVER market-sell while OCO live (cancel first, await confirm)
}
fn last_closed_candle_time(candles: &[Candle]) -> i64
fn is_stale(last_open_time: i64, now_ms: i64, timeframe_ms: i64) -> bool // > 2 missed candles → true
fn entry_client_id(hash: &str, candle_open_time: i64) -> String // format!("tp-{}-{}", &hash[..8], candle_open_time)
```
Cycle logic: load candles → staleness check (refuse, log) → signal only on last CLOSED candle (index len−2 semantics already used by backtester; reuse) → if entry order exists (client id known): poll status → FILLED ⇒ place_oco_sell → record; if OCO exists: check fills via my_trades/open orders → when flat, register_result(day_state, pnl from trades vs entry, was_stopout = exit ≤ stop*1.001) → persist.
- [ ] **Step 1: Failing integration tests (wiremock scripted)** — (a) fresh signal places LIMIT with deterministic client id; (b) filled entry → OCO placed with exact legs; (c) restart mid-entry reconciles (sees open entry, does NOT re-place); (d) flatten cancels then sells only after cancellations confirmed (assert request ORDER in wiremock expectations); (e) stale candles → CycleOutcome skipped with refusal log, no HTTP order calls made.
- [ ] **Step 2:** FAIL; **Step 3:** implement; **Step 4:** full `cargo test` green; **Step 5:** Commit `Task 5: executor state machine, reconciliation, flatten-all`

### Task 6: `trade` CLI wiring + safety gates

**Files:** Modify `src/main.rs` (subcommands `Trade`, `Flatten`); Modify `.env.example`

**Interfaces:**
- Consumes: Tasks 1–5
- Produces:
```rust
// trade --config <path> [--testnet|--live] [--dry-run] [--once]
// flatten --config <path> [--testnet|--live]
// base selection: --testnet => https://testnet.binance.vision (DEFAULT when neither flag given);
// --live => https://api.binance.com AND requires typed confirmation: prompt prints gate verdict then
// requires the literal word GO on stdin; anything else aborts.
// At launch ALWAYS print: latest stored search OVERALL verdict from backtest_runs (or "NO GATE RESULT ON RECORD")
// plus banner: "PRE-REGISTERED GATE VERDICT: NO-GO — running against spec §5 advice" when verdict is fail.
// --once runs a single cycle (used by smoke tests); default loops hourly aligned to candle close + 30s poll loop.
// Heartbeat: tracing::info each cycle "heartbeat alive cycle=N pos=X"; if TELEGRAM_BOT_TOKEN+TELEGRAM_CHAT_ID set,
// POST sendMessage (plain reqwest, no new dep) on start/halt/flatten/errors; failures to send are logged, never fatal.
```
- [ ] **Step 1: Failing tests** — clap parses all flags; live-without-GO aborts (feed stdin "no"); gate banner function output contains "NO-GO" when last verdict fail and "NO GATE RESULT" when table empty; base URL selection logic pure-fn tested.
- [ ] **Step 2:** FAIL; **Step 3:** implement; **Step 4:** green; **Step 5:** update `.env.example` (BINANCE_API_KEY=, BINANCE_API_SECRET=, TELEGRAM_BOT_TOKEN=, TELEGRAM_CHAT_ID= commented placeholders); Commit `Task 6: trade/flatten CLI, testnet default, live confirmation, gate banner`

### Task 7: Smoke suite + docs

**Files:** Create `scripts/smoke_local.sh`, `docs/RUNBOOK.md`; Modify `README.md` if present else create

- [ ] **Step 1:** `scripts/smoke_local.sh`: builds, runs full `cargo test`, then `trade --once --dry-run --testnet` against wiremock-less local env expecting clean refusal without keys (asserts exit code + log line), then `export` to temp CSV. Exit non-zero on any failure.
- [ ] **Step 2:** `docs/RUNBOOK.md`: key creation checklist (spot-only, withdrawals disabled, IP whitelist — verbatim spec §7), .env setup, testnet registration flow, start/reconcile/flatten/kill procedures, heartbeat silence meaning, launchd sample plist (KeepAlive, path placeholders).
- [ ] **Step 3:** Run script locally → green; Commit `Task 7: smoke suite + runbook`

### Task 8: Ship v0.2.0

- [ ] **Step 1:** `cargo fmt && cargo clippy -- -D warnings` clean; `cargo test` green
- [ ] **Step 2:** bump version to 0.2.0 in Cargo.toml; commit `v0.2.0`
- [ ] **Step 3:** `cargo build --release`; tag `v0.2.0`; push main + tag
- [ ] **Step 4:** `gh release create v0.2.0` with notes: contents summary, NO-GO status prominent, testnet-first instructions, macOS arm64 binary attached

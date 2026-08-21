# Phase 1: Research Foundation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the backtester + data layer so we can run the pre-registered strategy gate from spec §5 and get an honest go/no-go answer before any execution code exists.

**Architecture:** Single Rust binary, clap CLI with subcommands (`fetch`, `backtest`, `search`; `trade`/`serve`/`export` arrive in Phase 2). SQLite via sqlx runtime queries for kline caching and result storage. Public REST client only in this phase — no keys needed.

**Tech Stack:** tokio, reqwest (rustls), serde, sqlx(sqlite), clap(derive), tracing, wiremock (dev).

## Global Constraints (from spec rev 2)

- Fee model: taker 0.1% per side; slippage 0.05% per side — hard-coded constants shared by all backtests
- Pre-registered gate (never relax after seeing results): OOS profit factor ≥ 1.30, profitable OOS on ≥ 3 of 5 pairs, OOS max drawdown < 20%, ≥ 20 OOS trades/pair, ≤ 20 total configurations tested
- Data: ≥ 18 months of 1h klines per pair; optimize oldest 70%, validate newest 30%
- Fixed-dollar risk mode at launch: $2 risk/trade, max 1 open position, notional cap ≤ 50% equity, skip if notional < $15
- Strategies are TOML configs interpreted by code — no recompile per variant
- Secrets never committed; `.env` gitignored
- Every task ends green (`cargo test`) and committed

---

### Task 1: Scaffold crate + CLI skeleton

**Files:** Create `Cargo.toml`, `src/main.rs`, `.gitignore`, `.env.example`

- [ ] `cargo init`, add deps, build green, commit.

### Task 2: Core domain types

**Files:** Create `src/types.rs`; Test: inline `#[cfg(test)]`

- [ ] `Candle { open_time: i64, open/high/low/close/volume: f64 }`, parse from Binance kline JSON array-of-strings format. Test with real API sample row.

### Task 3: Indicators (EMA, RSI Wilder, ATR Wilder)

**Files:** Create `src/indicators.rs` + unit tests

- [ ] Hand-computed small-case tests, exact to f64 tolerance.

### Task 4: Strategy config + signal engine

**Files:** Create `src/strategy.rs`, `config/strategy_ema_rsi.toml`

- [ ] TOML → `StrategyConfig`; long-only: trend filter (close>EMA200 ∧ EMA50>EMA200) + RSI cross-up through threshold → Signal{entry=close, stop=entry−atr_mult·ATR, target=entry+rr·(entry−stop)}. Tests: synthetic uptrend yields signals, downtrend none.

### Task 5: Storage schema + klines cache

**Files:** Create `src/db.rs`, embedded migrations

- [ ] Tables: klines(PK symbol,interval,open_time), trades, orders, positions, equity_curve, signals_log, config_state. Roundtrip tests via temp file DB.

### Task 6: Exchange public REST client

**Files:** Create `src/exchange.rs`

- [ ] `fetch_klines(symbol, interval, start_ms)` with pagination to present, weight-aware retry on 429/418. wiremock tests for parsing + pagination; `#[ignore]` live test.

### Task 7: Backtest engine

**Files:** Create `src/backtest.rs`

- [ ] Fill model: enter next-bar open ×(1+slip); stop checked before target when both hit same candle; fees both sides. Fixed-$2 risk sizing with notional caps. Deterministic synthetic test verifying exact PnL math.

### Task 8: Metrics + gate evaluation

**Files:** Create `src/metrics.rs`, CLI wiring for `backtest`

- [ ] PF, win rate, maxDD from equity curve; `GateReport` implementing §5 thresholds verbatim; prints PASS/FAIL per pair + overall.

### Task 9: Search runner with variant budget

**Files:** Extend CLI with `search`; budget counter persisted in `config_state`

- [ ] Grid of 12 pre-declared variants × 5 pairs; refuses 21st distinct config hash; persists every run's OOS metrics; final ranked report.

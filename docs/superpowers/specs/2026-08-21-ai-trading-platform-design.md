# AI Trading Platform — Design Spec (rev 2)

**Date:** 2026-08-21 (rev 2 same day, post-review)
**Status:** Revised after external review; pending user re-approval
**Review acknowledgment:** Rev 1's risk model was arithmetically incoherent at the stated capital range, the AI overlay was negative-EV at this tier, and the parts determining profitability got less design attention than infrastructure. This revision fixes those. Full critique preserved in git history (rev 1 commit `f2688fe`).

## 1. Goal

A personal crypto swing-trading platform trading real money on Binance, running 24/7 on the user's Mac, operated with AI-assistant support.

**Framing:** The first live deployment ($100–300) is *tuition*, not capital. The honest base rate for retail algo trading is that most systems lose to fees over a full cycle. The design goal is bounded downside while we find out whether there is any edge — not promised income.

## 2. Decisions Made

| Decision | Choice |
|---|---|
| Market | Binance spot, liquid USDT pairs (test set of 5 for validation; trade subset that passes) |
| Style | Swing — decisions on 1h candle closes |
| Capital | $100–300 at go-live (tuition sizing); risk model scales coherently above $500 |
| AI overlay | **Deferred out of v1.** Revisit at ≥$3k capital with counterfactual logging (§9) |
| Language | Rust (user choice, confirmed twice). Mitigation for slow research iteration: strategies are TOML configs interpreted by the binary — variants need no recompile; backtest/live share exact code paths |
| Hosting | Mac 24/7, sleep disabled, launchd auto-restart, heartbeat monitoring |
| Interface | Telegram alerts from day one; web dashboard ships before go-live |

## 3. Architecture

Single Rust binary, Tokio async runtime, SQLite (`sqlx`), Axum web server, direct Binance REST/WebSocket client (`reqwest` + `tokio-tungstenite`, HMAC-SHA256 signing).

```
┌──────────────────────────────────────────────────────┐
│                   TRADING ENGINE                     │
│                                                      │
│  Market Data ──▶ Strategy Engine ──▶ Risk Manager    │
│  (REST+WS)       (TOML-defined      (veto + sizing)  │
│                   strategies)             │          │
│                                           ▼          │
│                            Executor ────────────────▶│ Binance
│                                │                     │
│                             SQLite DB               │
└───────────────┬──────────────────────────────────────┘
                ▼
    Axum Dashboard (:8080, localhost+token)
    Telegram Alerts + Hourly Heartbeat
```

### Modules (v1)

1. **exchange/** — Signed REST (orders, balances, klines) + WebSocket kline and user-data streams. Rate-limit tracking, retry with exponential backoff. Read-only public endpoints usable before API keys exist.
2. **strategy/** — Trait-based engine; concrete strategies declared in TOML files (indicator params, entry/exit rules from a small rule vocabulary). Launch hypothesis: EMA trend filter + RSI pullback entries, ATR stops. Explicitly treated as an unproven starting hypothesis — it is among the most-backtested retail templates and carries no assumed edge.
3. **risk/** — Hard guardrails, non-overridable at runtime (§4).
4. **executor/** — Order lifecycle state machine with idempotent client order IDs; correct OCO sequencing and flatten ordering (§6); restart reconciliation before any trading resumes.
5. **storage/** — SQLite tables: trades, orders, positions, equity_curve, signals_log, config_state. Trade rows carry everything required for tax cost-basis export; `export csv` CLI subcommand produces per-fill records (time, pair, side, qty, price, fee) suitable for tax software/import.
6. **alerts/** — Telegram: fills, rejections, risk events, daily summary, hourly heartbeat (alert if silent > 90 min).
7. **backtester/** — First-class module, not an afterthought (§5).
8. **dashboard/** — Axum, bound to 127.0.0.1, bearer-token auth: equity curve, positions, history, P&L, kill switch (cancel-all → flatten → halt).

## 4. Risk Model (reconciled with capital)

Two modes, selected automatically by account equity:

**Fixed-dollar mode (equity < $500) — launch mode**
- Risk per trade: **$2 fixed** (config)
- Max **1 open position**
- Notional cap: computed position ≤ 50% of equity
- Skip trade if computed notional < $15 (stays clear of Binance ~$5–10 min-notional with buffer)
- Daily halt: **−2% day or 2 consecutive stop-outs, whichever first**

**Percent-risk mode (equity ≥ $500) — unlocks later**
- Risk per trade: 1% of equity
- Max **2 concurrent positions**; total open risk ≤ 2% of equity
- Combined open notional ≤ 80% of equity (sizer takes the binding constraint)

Both modes:
- Daily −3% equity loss → flatten all, halt until manual reset
- Stale data (> 2 missed candles) → refuse to act
- Lot-size / step-size / min-notional compliance enforced by executor pre-submit

Arithmetic check (the rev-1 defect): $200 equity → $2 risk ÷ ~2% stop ≈ $100 notional = 50% of account, one position, placeable, survivable. $600 equity → $6 risk ≈ $300 notional × 2 = $600 > 80% cap → sizer shrinks to cap. Coherent at every level.

## 5. Research Process & Pre-Registered Go/No-Go Gate

Development order puts the backtester and strategy search **before** any execution or UI work.

**Backtester requirements:**
- Replays historical 1h klines through the identical strategy/risk/executor code paths used live
- Cost model: taker fees 0.1% per side (0.2% round trip; 0.15% with BNB discount) + slippage 0.05% per side
- Data: ≥ 18 months of 1h klines per pair, fetched fresh from Binance REST
- Split: optimize on oldest 70%, validate out-of-sample on newest 30%; OOS window untouched until final scoring

**Pre-registered gate — written before any backtest is run:**

Go live only if ALL hold on out-of-sample data after full costs:
1. Profit factor ≥ 1.30
2. Profitable OOS on ≥ 3 of 5 tested pairs
3. OOS max drawdown < 20%
4. ≥ 20 OOS trades per pair (sample size floor)
5. Total distinct configurations tested across the whole search ≤ 20 (multiple-comparison control)

**Fail any → no live deployment.** Iterate with a new OOS window or abandon the variant. The gate exists because a 1h strategy backtested without realistic costs reliably looks profitable and isn't; risk rails bound loss rate but create no edge.

## 6. Execution Mechanics

Corrected from rev 1 (Binance spot OCO cannot contain an entry leg):

```
signal → risk pass → LIMIT entry (idempotent client ID)
  → fill detected via user-data WS stream
    → place OCO: take-profit limit + stop-loss leg
```

- Stop-loss legs prefer stop-market semantics where available; **explicit honesty note: exchange-side stops bound losses *usually* — gaps can slip through a stop-limit. Downside is bounded in expectation, not guaranteed tick-for-tick.**
- Flatten-all ordering: cancel all open orders → confirm cancellations → market-reduce positions → verify balances against expected state. Never market-sell while an OCO can trigger mid-sequence.
- Kill switch (dashboard + Telegram command) runs the flatten sequence then halts the engine.

## 7. Security

- Binance API key: **spot trading enabled, withdrawals disabled, IP-whitelisted** — highest-value control, applied at key creation before first use
- Secrets in `.env` (gitignored); keys never logged, never stored in DB
- Dashboard: 127.0.0.1 bind + bearer token; never exposed to LAN/internet in v1

## 8. Reliability

- Restart → full reconciliation against open orders + balances before resuming
- WS disconnect → reconnect + REST backfill of missed candles
- Unrecoverable errors → Telegram alert + safe halt (existing positions keep exchange-side TP/SL)
- launchd job auto-restarts the binary; hourly Telegram heartbeat; silence > 90 min alerts the user
- Stages: unit tests → **Binance spot testnet** (`testnet.binance.vision`) end-to-end suite → shadow mode → live

## 9. Deferred to v2 (gated)

- **AI news/sentiment overlay:** only when capital ≥ $3k AND built with counterfactual logging from day one (every cycle logs decision-with-veto and decision-without so its P&L impact is measurable). Rev-1 finding stands: a per-day-cost LLM loop on a $100–300 account is negative-EV regardless of signal quality.
- Multi-position percent mode beyond 2 slots, additional strategies, futures/margin (still likely never).

## 10. Rollout Plan

| Phase | Content | Exit criteria |
|---|---|---|
| 1 (wks 1–2) | Exchange client (public endpoints), storage, backtester, strategy search | Gate in §5 passed or project paused honestly |
| 2 | Executor, risk manager, testnet suite green | Testnet round-trips reliably for ≥ 3 days |
| 3 | Shadow mode live-connected, Telegram-only monitoring | 3–7 days clean behavior, fills match expectations |
| 4 | Live-small: $100–300 real money; dashboard completed | — |

## 11. Explicit Non-Goals (v1)

- No LLM in the trade path
- No futures/margin/leverage — spot only
- No HFT — candle-close cadence only
- No guaranteed returns — bounded, survivable downside is the design goal
- No multi-exchange support

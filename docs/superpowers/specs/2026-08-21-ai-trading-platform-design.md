# AI Trading Platform — Design Spec

**Date:** 2026-08-21
**Status:** Approved by user (pending final review)

## 1. Goal

A personal AI-assisted crypto swing-trading platform that trades real money on Binance, runs 24/7 on the user's Mac, and is operated with full AI (assistant) support. Income is a goal, not a guarantee: the system is designed around strict risk controls so losing days are bounded.

## 2. Decisions Made

| Decision | Choice |
|---|---|
| Market | Binance crypto, spot only, 6–10 liquid pairs (BTC, ETH, SOL, …) |
| Style | Swing trading — decisions on 1h/4h candle closes |
| Capital | $100–$1,000 starting range; begin at low end |
| Execution | Real money from day one, behind hard risk rails |
| AI role | Rules-based core generates signals; LLM overlay may only veto or downsize |
| Language | Rust (user's explicit choice despite speed being non-critical) |
| Hosting | User's Mac, 24/7 (sleep disabled) |
| Interface | Web dashboard + Telegram alerts |

## 3. Architecture

Single Rust binary using Tokio. SQLite storage. Axum web server. One project folder, one command to run (`cargo run --release`), one `.env` for secrets.

```
┌─────────────────────────────────────────────────┐
│                  TRADING ENGINE                 │
│                                                 │
│  Market Data ──▶ Strategy Engine ──▶ Risk Mgr   │
│  (REST+WS)       (signals)         (veto/sizing)│
│                                        │        │
│                                        ▼        │
│                                   Executor ────▶│ Binance
│                                        │        │
│                                   SQLite DB      │
└──────────────┬──────────────────────────────────┘
               ▼
   Axum Dashboard (:8080) + Telegram Alerts
```

### Modules

1. **exchange/** — Binance client. Signed REST (orders, balances, klines) via `reqwest`; live candles via WebSocket (`tokio-tungstenite`). HMAC-SHA256 request signing, rate-limit tracking, retry with exponential backoff.
2. **strategy/** — Trait-based pluggable engine. Launch strategy: EMA trend filter + RSI pullback entries, ATR-based stop-loss and take-profit. Emits raw signals on candle close.
3. **risk/** — Hard guardrails (non-overridable):
   - Risk per trade ≤ 1% of equity
   - Max 3 concurrent open positions
   - Daily loss limit −3% → flatten all positions, halt until manual reset
   - Binance min-notional and lot-size compliance
   - Refusal to act on stale data (> 2 missed candles)
4. **ai_overlay/** — Scheduled (every 30 min) news/sentiment fetch for watched assets → LLM scores bias per asset on −1.0…+1.0 with one-line rationale. LLM provider is configurable (any OpenAI-compatible endpoint). Policy:
   - bias < −0.5 → block new longs in that asset
   - −0.5 ≤ bias < 0 → halve position size
   - positive bias never creates a trade
   - Cost target: ~$0.10–0.40/day
5. **executor/** — Order lifecycle state machine: submit → confirm fill → reconcile with exchange. Idempotent client order IDs. Crash-safe recovery: on restart, reconcile local state against open orders/balances before resuming.
6. **storage/** — SQLite via `sqlx`. Tables: trades, positions, equity_curve, signals_log, ai_decisions, config_state.
7. **dashboard/** — Axum on :8080 serving single-page UI: equity curve chart, open positions, trade history, P&L, AI decision feed, kill-switch button (flatten all + halt).
8. **alerts/** — Telegram bot messages: every order/fill, daily summary, all risk events.

## 4. Decision Loop

On each 1h candle close:

```
WS kline closes ─▶ update indicators ─▶ strategy emits raw signals
                                             │
   ai_overlay bias scores ──────────────────▶ risk manager
                                             │  ├─ size = equity × 1% ÷ ATR stop distance
                                             │  ├─ veto if daily loss / max pos / stale data
                                             │  ▼
                                        executor places OCO
                                     (entry + TP + SL on Binance)
                                             │
                                  SQLite log → Telegram → dashboard
```

## 5. Error Handling

- WS disconnect → auto-reconnect, backfill missing candles from REST
- Any restart → full reconciliation against Binance open orders + balances before trading resumes
- Unrecoverable exchange errors → Telegram alert + safe halt (existing positions keep exchange-side SL/TP)
- All secrets in `.env` (gitignored); no keys ever logged or stored in DB

## 6. Testing & Rollout Plan

1. **Unit tests** — indicators, position sizing math, state machine transitions
2. **Backtester** — replays historical klines through the same strategy code; report win rate, profit factor, max drawdown per pair before go-live
3. **Shadow mode** — bot fully connected, logs hypothetical fills instead of ordering; recommended first 3–7 days of operation; toggled by one config value
4. **Live-small** — start with $100–300

## 7. Explicit Non-Goals (v1)

- No futures/margin/leverage — spot only
- No HFT — candle-close cadence only
- No guaranteed returns — bounded downside is the design goal
- No multi-exchange support — Binance only

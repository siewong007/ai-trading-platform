# Operations — autonomous deployment (macOS)

How this platform runs itself on the operator's machine. Complements
[RUNBOOK.md](RUNBOOK.md) (engine procedures) — read that first.

## Runtime split

| Location | Role |
|---|---|
| `<repo>` (external SSD) | source of truth: code, configs, docs, research DB (`data/trading.db`) |
| `~/trading-engine/` | live runtime: binary reads `.env`, its own `config/`, own `data/trading.db` (klines + orders + trades) |

**Why the split:** macOS TCC silently blocks launchd agents from reading
removable volumes — a LaunchAgent pointed at the external SSD hangs before
its first file access. Everything a service touches must live on the
internal disk. After any config change: copy TOML into
`~/trading-engine/config/` and restart the executor.

## Services (launchd, gui domain)

| Label | Cadence | Job |
|---|---|---|
| com.tradingplatform.trade | continuous (KeepAlive) | testnet executor loop; reconciles on every start |
| com.tradingplatform.fetch | 30 min | refresh klines in **engine** DB — REQUIRED: the executor only *reads* candles; without freshness `is_stale` refuses to act |
| com.tradingplatform.dashboard | 60 s | render `~/trading-engine/dashboard.html` (self-contained HTML, auto-refreshing tab) |
| com.tradingplatform.tgbot | long-poll (KeepAlive) | Telegram reporter + entry/fill alert watcher (~60 s latency) |
| com.tradingplatform.research | 6 h | measurement suite on both families → `research_history.log`; Telegram push only on change (verdict / p-value fingerprint) |

Control: `launchctl bootstrap|bootout gui/$(id -u)/<label>`.
Logs: `/tmp/tp_<name>.{log,err}`.

## Telegram bot

Created via @BotFather; token + chat id live ONLY in `~/trading-engine/.env`
(chmod 600). Register the chat menu once via `setMyCommands`
(report / pnl / research).

- Pushes: trade start + reconciliation summary, halts (with reason),
  flatten events, cycle errors, 🟢 entries, ✅/❌ closed fills (with running
  balance), 🔬 research changes.
- Pulls: `/report`, `/pnl`, `/research`.
- Read-only by design: no trade verbs exist in chat; `flatten` stays CLI-only.

## Research cadence

The 6 h loop runs free paths only (no variant-budget charge, no unlock):
`backtest --folds 4`, `sensitivity`, `permutetest --trials 100` per family,
against the deep cache (`--lookback-days 2200` ≈ 6 y). History accumulates
in `research_history.log` for longitudinal regime tracking. Window-regime
notes and pre-registration rules: [OOS_STUDY.md](OOS_STUDY.md).

## Hygiene

- `.env` files are chmod 600, gitignored, never logged by the platform.
- Testnet keys pasted through chat should be regenerated via BotFather /
  testnet.binance.vision when convenient.
- Logs grow unbounded (~0.5 MB/day); rotate `/tmp/tp_*.log` occasionally.

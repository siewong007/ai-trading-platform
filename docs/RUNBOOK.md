# Runbook — trading_platform v0.2 execution layer

Operational procedures for running the `trade` engine (spec §6–§8). Read this
fully once before the first testnet run.

## 0. Status warning

The pre-registered gate verdict is **NO-GO**. `trade --live` exits non-zero
until a stored overall PASS exists (`GO` cannot override). `flatten --live`
remains the kill switch (still requires typed `GO`). Testnet is the sanctioned
engine stage. Variant budget: 12/20 used; remaining 8 are reserved — do not
run `search --unlock-new-study` on this OOS window.

An agent/orchestrator may run `scripts/lab.sh` (`fetch` + `backtest` only) to
speed measurement. It must not call `trade`, `flatten`, or
`search --unlock-new-study`. There is no LLM in the trade path (spec §11).

## 1. API key creation checklist (verbatim spec §7)

> Binance API key: **spot trading enabled, withdrawals disabled,
> IP-whitelisted** — highest-value control, applied at key creation before
> first use

- [ ] Spot trading enabled
- [ ] Withdrawals disabled
- [ ] IP whitelist set to the host that runs the binary
- Secrets stay in `.env` (gitignored); keys are never logged, never stored in DB

## 2. `.env` setup

```sh
cp .env.example .env
chmod 600 .env   # then edit:
# BINANCE_API_KEY=...
# BINANCE_API_SECRET=...
# optional Telegram alerts (both required to enable):
# TELEGRAM_BOT_TOKEN=...
# TELEGRAM_CHAT_ID=...
```

`.env` is gitignored — verify with `git status` before committing anything.

## 3. Testnet registration flow

1. Register at <https://testnet.binance.vision> (GitHub login works).
2. Generate testnet API key/secret (test funds are pre-loaded).
3. Put them in `.env` as above — they are separate from production keys.
4. Sanity-check: `cargo run -- trade --once --dry-run --testnet` should print
   the gate banner and a reconciliation summary without placing orders.

## 4. Start / reconcile / flatten / kill

- **Start:** `cargo run --release -- trade --config config/strategy_ema_rsi.toml`
  (defaults to testnet). Startup ALWAYS runs reconciliation first: open orders
  + balances fetched, orphan entries cancelled, expected state persisted; the
  summary is logged before the first cycle.
- **Reconcile:** automatic at every start; restart the process after any crash
  — never hand-edit DB state.
- **Flatten (kill switch):** `cargo run --release -- flatten`
  Ordering enforced by code: cancel all open orders → confirm empty →
  market-reduce remaining base → verify balances. Never market-sells while an
  OCO could trigger mid-sequence.
- **Kill:** Ctrl-C or `launchctl stop`. After any kill, restart triggers full
  reconciliation. If day-state shows halted, the engine stays down until
  manual reset of `config_state` (`day_state_*` keys) — understand WHY it
  halted (daily −2%, 2 stop-outs, −3% flatten rule) before resetting.

## 5. Heartbeat silence meaning

Each cycle logs `heartbeat alive cycle=N pos=X` (~every 30 s poll loop;
signals hourly). With Telegram configured, alerts fire on start/halt/flatten/
errors. Spec §8: **silence > 90 min means the engine is dead** — check
`launchctl` status, logs, network; then restart (reconciliation makes restarts
safe).

## 6. launchd sample plist

Save as `~/Library/LaunchAgents/com.tradingplatform.trade.plist`; replace the
three `PATH_PLACEHOLDER`s:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.tradingplatform.trade</string>
  <key>ProgramArguments</key>
  <array>
    <string>PATH_PLACEHOLDER/trading_platform</string>
    <string>trade</string>
    <string>--config</string>
    <string>PATH_PLACEHOLDER/config/strategy_ema_rsi.toml</string>
  </array>
  <key>WorkingDirectory</key><string>PATH_PLACEHOLDER</string>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>/tmp/tp_trade.log</string>
  <key>StandardErrorPath</key><string>/tmp/tp_trade.err.log</string>
</dict>
</plist>
```

Load: `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.tradingplatform.trade.plist`
Stop: `launchctl bootout gui/$(id -u)/com.tradingplatform.trade`

KeepAlive=true auto-restarts crashes; each restart reconciles first, so
exchange-side TP/SL keeps positions safe while the process is down.

## 7. Smoke suite

`scripts/smoke_local.sh` — builds, runs the full test suite, proves `trade`
refuses cleanly without keys, and exercises CSV export. Run it after pulling
or before any operational change.

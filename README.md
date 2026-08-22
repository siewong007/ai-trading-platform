# trading_platform

Personal AI-free algorithmic spot-trading platform for Binance (EMA/RSI
strategy with ATR stops), built around an honest pre-registered backtest gate.

**Current gate verdict: NO-GO — research only.** Live is refused in code until a
stored overall PASS. 8 of 20 lifetime variant slots are reserved; `search`
replays the known 12 for free and will not spend new hashes unless
`--unlock-new-study` + typed `NEW-OOS` (new OOS window, documented first).
Testnet-first: `trade` defaults to `https://testnet.binance.vision`.

> **Research-only freeze (2026-08-22):** live trading is **refused in code**
> until a stored overall gate PASS exists — `GO` cannot override FAIL or a
> missing verdict. The remaining 8 variant slots (12/20 used) are reserved for
> a documented new out-of-sample study; `search` refuses new config hashes
> unless invoked with `--unlock-new-study` and the literal stdin word `NEW-OOS`.

## Commands

| Command | Purpose |
|---|---|
| `fetch` | cache ~18 months of 1h klines per configured pair |
| `backtest` | one config: IS/OOS report + gate verdict |
| `search [--unlock-new-study]` | replay the known grid; new hashes locked |
| `export --out FILE.csv` | trades table → CSV (tax records) |
| `trade [--once] [--dry-run] [--testnet\|--live]` | executor loop (testnet default; `--live` requires stored PASS then `GO`) |
| `flatten` | kill switch: cancel all → confirm → market-reduce → verify |

## Quickstart

```sh
cp .env.example .env      # add keys (spot-only, withdrawals OFF, IP-whitelisted)
cargo build --release
scripts/smoke_local.sh    # build + tests + keyless-refusal + export checks
cargo run --release -- fetch
cargo run --release -- search
cargo run --release -- trade --once --dry-run   # single testnet cycle
```

Risk rails (fixed-dollar mode): $2/trade risk, max 1 position, ≤50% equity
notional cap, skip <$15 notional, daily halt at −2% or 2 consecutive
stop-outs, −3% day → flatten-all + halt until manual reset, stale-data
refusal, exchange lot/notional compliance enforced pre-submit.

Secrets live only in `.env` (gitignored); keys are never logged or stored in
the DB. Operations manual: [docs/RUNBOOK.md](docs/RUNBOOK.md).

# trading_platform

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Personal algorithmic **spot-trading lab** for [Binance](https://www.binance.com):
an EMA/RSI pullback strategy with ATR stops, built around an honest
pre-registered backtest gate.

The crate name is `trading_platform`. There is **no LLM in the trade path**.
The first live deployment is tuition-sized research, not promised income.

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

## Status

| Item | State |
|---|---|
| Venue | Binance spot only (no futures, margin, or second exchanges) |
| Strategy under study | EMA trend filter + RSI pullback, ATR stops (`config/strategy_ema_rsi.toml`) |
| Pairs | BTCUSDT, ETHUSDT, SOLUSDT, BNBUSDT, XRPUSDT |
| Gate | FAIL / NO-GO on the current OOS window |
| Variant budget | 12 / 20 used; 8 reserved |
| Live | Refused until stored overall PASS |
| Testnet / dry-run | Allowed for engine checks; fills do not count toward the gate |

## Commands

| Command | Purpose |
|---|---|
| `fetch [--interval TF] [--lookback-days N]` | cache klines (default ~18 months 1h; deep research: `--lookback-days 2200`) |
| `backtest [--folds K]` | IS/OOS report + gate verdict + attribution + fold stability |
| `search [--unlock-new-study]` | replay the known grid; new hashes locked |
| `sensitivity [--pct P]` | one-at-a-time parameter plateau analysis (free, no budget) |
| `permutetest --trials N` | significance vs shuffled-entry null distribution |
| `report [--md]` | research ledger dump (budget, hashes, windows, halts) |
| `export --out FILE.csv` | trades table → CSV (tax records) |
| `trade [--once] [--dry-run] [--testnet\|--live]` | executor loop (testnet default; `--live` requires stored PASS then `GO`) |
| `flatten` | kill switch: cancel all → confirm → market-reduce → verify |
| `scripts/lab.sh` | lab tech: `fetch` + `backtest` only (no search, no trade, no LLM) |

Families are pluggable via the `SignalFamily` registry
(`src/strategy.rs`): each registers its signal engine and its FROZEN
pre-declared grid. Registered today: `ema_rsi_pullback` (12 variants),
`zband_meanrev` (6 reserved slots). Autonomous deployment guide:
[docs/OPERATIONS.md](docs/OPERATIONS.md).

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

## Pre-registered go / no-go gate

Go live only if **all** hold on out-of-sample data after full costs
(0.1% taker per side + 0.05% slippage per side). Thresholds were frozen
before results were seen:

1. Profit factor ≥ 1.30
2. Profitable OOS on ≥ 3 of 5 tested pairs
3. OOS max drawdown < 20%
4. ≥ 20 OOS trades per pair
5. ≤ 20 distinct configurations across the whole search

Fail any → no live deployment. Iterate with a **new** OOS window or abandon
the variant. The gate exists because a 1h strategy backtested without
realistic costs reliably looks profitable and isn't.

## Docker

Research-only image. Default command is `--help`; nothing trades without an
explicit operator command.

```sh
docker compose run --rm app fetch
docker compose run --rm app backtest
docker compose run --rm app search
```

Tagged images publish to `ghcr.io/siewong007/trading_platform` on `v*` tags.

## Layout

```
config/     strategy TOML (interpreted; variants need no recompile)
src/        single Rust binary — exchange, strategy, risk, executor, backtest
scripts/    smoke_local.sh, lab.sh
docs/       RUNBOOK.md and design specs
data/       local SQLite + kline cache (gitignored)
```

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md). Bug reports and small, gated research
improvements are welcome. Changes that weaken the live refuse, spend reserved
variant slots, or add an LLM to the trade path will be rejected.

## License

This project is licensed under the [MIT License](LICENSE).

## Disclaimer

This software is research tooling. It can lose money. Past backtests, including
passing gates, are not a guarantee of future results. You are solely
responsible for API keys, exchange settings, and any orders placed. The
authors are not liable for trading losses.

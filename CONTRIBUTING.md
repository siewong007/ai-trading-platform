# Contributing to trading_platform

Thanks for taking an interest. This repo is a **personal research lab** for
Binance spot, not a live income system. Please read this file and the
[Code of Conduct](CODE_OF_CONDUCT.md) before opening an issue or pull request.

## Research-only freeze

As of 2026-08-22 the pre-registered gate verdict is **NO-GO**. Live trading is
**refused in code** until a stored overall PASS exists. `GO` cannot override
FAIL or a missing verdict.

Before you send a change, treat these as hard rules:

- Do **not** weaken or bypass the live-trade refuse (`trade --live` without a
  stored PASS).
- Do **not** spend reserved variant slots. `search` may replay the known 12
  hashes. New hashes need `--unlock-new-study` **and** the literal stdin word
  `NEW-OOS`, and only after a written new out-of-sample study.
- Do **not** put an LLM in the trade path. Lab tech is `scripts/lab.sh`
  (`fetch` + `backtest` only).
- Do **not** commit `.env`, API keys, SQLite databases, or anything under
  `data/`.
- Testnet and `--dry-run` are for engine checks. Fills never count toward the
  gate.

Operations detail: [docs/RUNBOOK.md](docs/RUNBOOK.md). Freeze design:
[docs/superpowers/specs/2026-08-22-research-only-freeze-design.md](docs/superpowers/specs/2026-08-22-research-only-freeze-design.md).

## Development setup

Requirements:

- Rust stable (edition 2021)
- Network access to Binance public REST for `fetch` / `backtest`

```sh
git clone https://github.com/siewong007/ai-trading-platform.git
cd ai-trading-platform
cp .env.example .env   # only needed for trade / flatten
cargo build
scripts/smoke_local.sh
```

`scripts/smoke_local.sh` builds, runs the full test suite, proves `trade`
refuses cleanly without keys, and checks CSV export. It uses a throwaway temp
directory and does not touch repo `data/` or `.env`.

Optional measurement (no search, no trade):

```sh
scripts/lab.sh
```

## Making a change

1. Open an issue first for anything that changes the gate, risk rails,
   executor, or variant budget. Small doc and test-only PRs can skip this.
2. Branch from `main`.
3. Keep the change small and on one topic.
4. Match existing Rust style in `src/`. Run `cargo fmt` and `cargo test`
   before you push.
5. If you touch `trade`, `flatten`, `search`, or the gate, also run
   `scripts/smoke_local.sh`.
6. Open a pull request against `main`. Describe **what** changed and **why**,
   and call out any behavior that could place orders.

### Pull request checklist

- [ ] `cargo test` is green
- [ ] `scripts/smoke_local.sh` is green if you touched CLI, gate, executor, or export
- [ ] No secrets, `.env`, or `*.db` files
- [ ] Live refuse and variant-budget lock still hold
- [ ] Docs updated if commands or operator steps changed (`README.md`,
      `docs/RUNBOOK.md`)

## Reporting bugs

Include:

- Command you ran (redact keys)
- Expected vs actual behavior
- Relevant log lines with secrets stripped
- OS and `rustc --version`

Security-sensitive reports (key leakage, unsigned-order bugs, live-refuse
bypass) should go to siewong007@gmail.com instead of a public issue.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).

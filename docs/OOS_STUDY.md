# OOS Window Ledger & Reserved-Slot Study Policy

Pre-registration record required before any `search --unlock-new-study`
invocation (README freeze 2026-08-22; RUNBOOK §0; spec §5). Written
2026-08-25 from `data/trading.db` + `src/main.rs` mechanics. No slots spent.

## How the window is computed

- OOS = most recent `(1 − IS_FRACTION)` of cached klines;
  `IS_FRACTION = 0.70` (`src/main.rs:88`), `split_idx = ⌊len × 0.70⌋`
  (`src/main.rs:341`).
- Config hash covers strategy/backtest params only (`src/strategy.rs:49`);
  it does NOT encode the window. Therefore extending `fetch` slides the
  OOS window forward over newer calendar time — the only clean way to get
  new out-of-sample data.

## Study ledger

| Study | Data range (UTC) | OOS window (UTC) | Verdict |
|---|---|---|---|
| Original grid (freeze 2026-08-22) | 2025-02-17 19:00 → 2026-08-22 | ≈ 2026-03-09 → 2026-08-22 | FAIL (all 12 variants) |
| Current state | 2025-02-17 19:00 → 2026-08-25 11:00 | **2026-03-12 08:00 → 2026-08-25 11:00** | FAIL (replay of known 12, free) |

Overlap between original and current OOS: > 99 %. Only ~3 days of the
current window were unseen by the failing study. **This is still the same
OOS window for pre-registration purposes.**

## Decision rule (pre-registered here, before any results)

The 8 reserved variant slots may be spent only when:

1. **Window disjointness**: the OOS window at time of study starts on or
   after the END of the last studied window (≥ 2026-08-25). With the
   rolling ~18-month cache and 70/30 split, OOS length stays ≈ 166 days,
   so the earliest qualifying date is **≈ 2027-02-07 UTC**. No earlier
   unlock is a re-roll of seen data.
2. **Documentation exists**: this file updated with the exact candidate
   window bounds BEFORE running.
3. **One shot**: all planned configs run once against the new window.
   No iterative re-runs on partial results; no post-hoc param edits after
   seeing the ranked table.
4. Gate thresholds stay exactly as frozen (PF ≥ 1.30, ≥ 3/5 pairs
   profitable, DD < 20 %, ≥ 20 OOS trades/pair, ≤ 20 configs ever).

Execution is OPERATOR-only (RUNBOOK §0 bars agents from
`--unlock-new-study`). Command template for that day:

```sh
cd "/Volumes/APPLE EXTERNAL SSD /Personal Projects/ai-trading-platform"
<target-dir>/trading_platform fetch
printf 'NEW-OOS\n' | <target-dir>/trading_platform search \
  --config config/strategy_ema_rsi.toml --unlock-new-study
```

## Honest expectation (recorded 2026-08-25)

All 12 known variants have worst-pair OOS PF ≤ 0.89 after costs — below
1.0 breakeven, far below the 1.30 gate. A calendar shift alone flipping
that to PASS is unlikely. If the edge family is not structurally improved
by the time a valid window opens, the statistically correct move per the
README is: **abandon this EMA/RSI pullback family and reserve the 8 slots
for a different strategy design**, not a re-roll.

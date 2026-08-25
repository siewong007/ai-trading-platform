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

## Deep-history regime change (recorded 2026-08-25 16:30 UTC)

Cache extended via `fetch --lookback-days 2200` (new CLI flag): 52,780
candles/pair spanning 2020-08-16 → 2026-08-25 — six years, multiple full
cycles. IS/OOS split recomputes automatically: **OOS is now ≈ 22 months
(2024-10 → 2026-08)** instead of 5.4 months. All free-path measurements
below run on this deeper basis. The reserved-slot study policy is
UNCHANGED: its window must still start after the last studied window ends
(≥ 2026-08-25), operator-documented before unlock.

### Deep-basis verdicts (free paths, informational)

| Family | Gate | Folds K=6 | Permutation (200) | Attribution |
|---|---|---|---|---|
| ema_rsi_pullback | FAIL | mixed, net negative | actual −170.6 vs null −48.2 ± 14.1, **p = 1.000** | worst buckets 20h/12h UTC |
| zband_meanrev | FAIL (DD 64%!) | negative in 27/30 cells | actual −765.6 vs null −684.1 ± 36.0, **p = 0.995** | all six buckets negative |

**Key inference:** with p ≈ 1.00, BOTH families place trades *systematically
worse than random entries with identical exits*. This is not absence of
edge — it is measured negative signal content at 1h frequency for these
indicator logics. Six years of evidence across two orthogonal families.

**Registered lead (NOT tuned, NOT celebrated):** the only positive session
bucket anywhere is ema_rsi entries in the 00–04h UTC window (+15.11 across
the full OOS). Any hour-conditioned successor family must be designed,
frozen, and tested on a disjoint future window like every other — this note
is a hypothesis registration, not a result.

## Gen-2 candidates — measured free, freeze memo (2026-08-25)

Two new families developed per
[gen-2 design](superpowers/specs/2026-08-25-research-gen2-design.md).
All measurements on the deep cache (OOS ≈ 2024-10 → 2026-08), zero budget
spent.

### donchian_vol (breakout continuation) — ABANDON

Gate FAIL · folds negative in most cells · permutation actual −252.1 vs
null −185.4 ± 22.2, **p = 1.000**. Same signature as gen-1: systematically
worse than random entries. Never spends slots.

### session_ema_rsi (hour-gated pullback) — FIRST SIGNIFICANT SEPARATION

Gate FAIL on trade-count floor only (frequency starved by design).
Permutation: **actual +11.45 vs null −12.43 ± 8.16, p = 0.015** — first
config in project history to separate from luck at 0.05. Fold view: profits
concentrated in early folds; aggregate ≈ breakeven-to-slightly-positive.

**Honest caveats:** (1) the window was selected from bucket analysis ON this
same data — the permutation partially self-fulfills, so the true edge is
almost certainly smaller than +11 suggests and may be zero; (2) thin counts
per pair (floor unmet); (3) sensitivity: base slightly negative, neighbors
mixed — no clean plateau.

### Freeze memo (operator decision required before any unlock)

| Item | Recommendation |
|---|---|
| zband_meanrev (6 reserved) | formally abandon: worse-than-random, DD 64% |
| donchian_vol | abandoned pre-registration (measured free) |
| session_ema_rsi (3 frozen) | SPEND — the only candidate with positive separation |
| Resulting budget | 12 + 3 = 15/20 used; 5 spare |

Study window for the session_ema_rsi unlock must be disjoint from all data
through 2026-08-25 and documented here before `--unlock-new-study`.

## Honest expectation (recorded 2026-08-25)

All 12 known variants have worst-pair OOS PF ≤ 0.89 after costs — below
1.0 breakeven, far below the 1.30 gate. A calendar shift alone flipping
that to PASS is unlikely. If the edge family is not structurally improved
by the time a valid window opens, the statistically correct move per the
README is: **abandon this EMA/RSI pullback family and reserve the 8 slots
for a different strategy design**, not a re-roll.

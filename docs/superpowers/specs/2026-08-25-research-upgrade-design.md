# Research Ability Upgrade — Design

Date: 2026-08-25 · Status: approved (operator: "implement all")
Branch: feat/research-upgrade · Predecessor: zband spec + OOS_STUDY.md

## Scope rule

MEASUREMENT-layer only. Frozen items untouched: gate thresholds, the two
pre-declared grids, config-hash semantics (legacy hashes byte-stable),
variant budget accounting, executor/live path, cost constants. New code
must default to off so existing outputs stay byte-compatible.

## Items

1. **Temporal fold stability** — `backtest --folds K`: partition trades
   into K equal consecutive OOS-style windows (same partition pattern as
   existing IS/OOS split; signals are window-independent because indicators
   are rolling). Report per-fold n/PF/pnl + summary (minPF, spread).
   K=0 default = today's output.
2. **Parameter-sensitivity plateaus** — `sensitivity --config X [--pct 20]`:
   one-at-a-time ±pct perturbation of the family's tunable params via the
   free evaluate path (record_hash=None ⇒ no budget charge). Verdict:
   PLATEAU when base OOS-pnl>0 AND ≥ half the neighbors also >0.
3. **Luck-adjusted best-of-N** — normal-approximation expected maximum of
   T standard normals × trial PF dispersion vs observed best; surfaced in
   `report`. Honest label: approximation, not exact SPA.
4. **Window bounds persistence** — nullable oos_start_ts/oos_end_ts on
   backtest_runs (+ idempotent migration), filled by evaluate_config.
5. **Trade attribution** — TradeRecord gains mfe_r/mae_r (excursions in R
   multiples) + bars_held, tracked in Position during simulation;
   backtest prints an attribution block (avg MFE/MAE, median hold, worst
   hour-bucket); export CSV gains the columns.
6. **Strategy family registry** — `trait SignalFamily {name, signals,
   grid}` replacing name-string if/else dispatch; frozen grid values and
   legacy behavior identical (existing suite is the regression net).
7. **`report` subcommand** — JSON dump of budget ledger, known hashes w/
   latest runs incl. window bounds, halt state; `--md` human summary.
8. **Multi-timeframe fetch** — `fetch --interval <tf>` override (klines PK
   already includes interval); engine untouched.
9. **Permutation test** — `permutetest --config X --trials N`: realized
   plans' (entry,stop,target) reassigned to random valid indices via
   internal xorshift RNG (no new deps); null PF distribution vs actual.
   Depends on extracting `run_with_signals`.

## Execution order

4 → 5 → 1 → 7 → 3 → 9 → 2 → 8 → 6 (windows first: 1/7 consume them;
registry last: pure refactor over finished features).

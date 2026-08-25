# Z-Score Band Fade (`zband_meanrev`) — Design Spec

Date: 2026-08-25
Status: Approved design, pre-implementation
Predecessor: `2026-08-22-research-only-freeze-design.md`; gate context in
`docs/OOS_STUDY.md` (window ledger + reserved-slot policy)

## Why

The EMA/RSI trend-pullback family failed the pre-registered gate 12/12:
worst-pair OOS PF ≤ 0.89 after costs (0.1 % taker/side + 0.05 %
slippage/side), 7–15 OOS trades/pair vs the 20-trade floor. Two structural
causes: (a) the signal fires rarely on 1 h bars, so statistics are thin;
(b) pullback-continuation entries buy strength that mean-reverts to a loss
after round-trip costs. A new family must fix both: higher natural trade
frequency and a mechanism orthogonal to trend continuation.

Family chosen: **mean-reversion band fade** (operator decision 2026-08-25,
from candidates: z-score bands / Bollinger %B / short-RSI reversion).
Short-RSI was rejected as same indicator class as the failed family.

## Signal (long-only spot, per closed 1 h candle)

Let `mean_i`, `σ_i` be the rolling arithmetic mean and population standard
deviation of closes over the last `lookback_bars` bars ending at bar `i`.
Define `z_i = (close_i − mean_i) / σ_i`.

Entry (long) at bar `i` close when ALL hold:

1. Warmup satisfied: `i ≥ max(lookback_bars, atr_period + 101)` (the +101
   guarantees full ATR history for the veto median below).
2. `z_i ≤ −z_entry`.
3. Vol-spike veto: `atr_i ≤ 1.5 × median(atr_{i-100}, …, atr_{i-1})`
   (fixed constants, NOT grid dimensions). The strategy refuses to fade an
   active crash; one bad hour can otherwise dominate the loss distribution.
4. Flat position for this pair (engine enforces single position/pair).

Exits via existing OCO placement:

- Stop: `entry − atr_multiplier × atr_i` (existing risk rail, unchanged)
- Target: `mean_i` (fade back to fair value); limit order
- `risk_reward_ratio` is unused by this family and stays fixed in TOML

Known v1 limitation, accepted: no separate regime filter beyond the veto.
Fewer tunables = smaller frozen grid = less selection surface. If the gate
fails on drawdown grounds, a regime filter is the FIRST candidate for any
future redesign — not more grid points.

## Configuration

New file `config/zband_meanrev.toml`:

```toml
[strategy]
name = "zband_meanrev"
pairs = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT"]
timeframe = "1h"
lookback_bars = 48          # NEW (grid dim)
z_entry = 2.0               # NEW (grid dim)
ema_fast = 50               # unused by family; fixed for hash stability
ema_slow = 200              # unused; fixed
rsi_period = 14             # unused; fixed
rsi_entry_threshold = 35.0  # unused; fixed
atr_period = 14
atr_multiplier = 2.0        # stop distance
risk_reward_ratio = 1.5     # unused; fixed

[backtest]                  # identical rails, unchanged
start_equity_usd = 200.0
risk_per_trade_usd = 2.0
max_notional_pct_equity = 0.5
min_notional_usd = 15.0
```

Unused legacy fields stay present and fixed so the canonical hash covers
every effective parameter with no special cases. New fields deserialize via
`#[serde(default)]` so existing TOML keeps parsing unchanged.

## Pre-declared variant grid — FROZEN 2026-08-25, before any run

| Config | lookback_bars | z_entry |
|---|---|---|
| zb-1 | 24 | 2.0 |
| zb-2 | 24 | 2.5 |
| zb-3 | 48 | 2.0 |
| zb-4 | 48 | 2.5 |
| zb-5 | 96 | 2.0 |
| zb-6 | 96 | 2.5 |

Six of the eight remaining budget slots. Two slots stay reserved as
insurance against hash accidents during the study. **No additions, no
post-hoc substitutions** after any result is seen. Gate thresholds remain
exactly as frozen (PF ≥ 1.30, ≥ 3/5 pairs profitable, OOS DD < 20 %,
≥ 20 OOS trades/pair, ≤ 20 configs ever).

## Implementation surface

1. `src/indicators.rs`: add `rolling_stats(closes, n) -> (Vec<Option<f64>>
   means, Vec<Option<f64>> stds)` — population σ, O(n·lookback) is fine at
   13 k bars. Unit tests against hand-computed vectors.
2. `src/strategy.rs`: extend `StrategySection` with `lookback_bars`,
   `z_entry` (serde defaults); extend `config_hash` field list; dispatch
   `generate_signals` by `name` between `ema_rsi_pullback` (unchanged) and
   `zband_meanrev` (new `generate_zband_signals`).
3. `src/main.rs` `run_search`: per-family pre-declared grids dispatched by
   config name; ema_rsi grid byte-identical to today.
4. `src/db.rs`: migration adds nullable `lookback_bars`, `z_entry` columns
   to `backtest_runs`; row struct extended; old rows read back as NULL.
5. Unchanged: executor, exchange, risk rails, metrics/gate math, ws,
   signed client. No LLM anywhere in the trade path.

## Testing

- Indicator: known-vector tests for mean/σ incl. insufficient-warmup None;
  constant series → σ = 0 (and signal division guard).
- Signals: synthetic series where a crafted dip crosses −z_entry and
  produces the exact TradePlan; warmup region all None; spike-veto blocks
  entry; z just outside threshold does not fire.
- Hash: changes when either new param changes; stable across identical
  configs; old-family hash values unchanged (regression test pins them).
- Search: new-family grid runs under budget accounting; refusal still
  fires without unlock for unseen hashes; known-hash replay free.
- Full `cargo test` + `scripts/smoke_local.sh` green.

## Validation protocol (honest research)

1. Development may use free `backtest` runs freely (charges no budget,
   persists nothing) for sanity and debugging.
2. The frozen grid above is selected on data through 2026-08-25; because
   the future study window is disjoint (starts ≥ 2026-08-25 end), that
   selection is in-sample tuning against truly unseen future data —
   legitimate.
3. The ONE charged evaluation happens per `docs/OOS_STUDY.md`: earliest
   ≈ 2027-02-07 UTC, operator-typed `NEW-OOS --unlock-new-study` search,
   all six configs, one shot, stored verdict decides GO/NO-GO.

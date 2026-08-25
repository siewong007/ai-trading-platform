# Research Generation 2 — `session_ema_rsi` + `donchian_vol` Design

Date: 2026-08-25 · Status: approved · Branch: feat/research-gen2

## Motivation

Deep history proved both registered families place trades worse than random
(p ≥ 0.995). Two structurally distinct candidates are developed on FREE
paths only; slot allocation among {gen-1 leftovers, these two} happens once,
documented, before any future unlock. Development charges zero budget.

## Family 1: `session_ema_rsi`

EMA/RSI pullback engine + UTC entry-hour gate (the registered 00–04h lead,
widened for trade-count survival given the ≥20-trades/pair floor).

- New field: `entry_window_utc: Option<String>` e.g. `"00-08"` (inclusive
  start hour, exclusive end hour; windows may wrap midnight).
- Entry requires `hour_in_window(candle_hour_utc, window)`.
- Frozen grid (3): `"00-08"`, `"00-12"`, `"22-06"` — tests whether avoiding
  the bleeding US session (12–20h UTC) fixes the family, not just one bucket.
- Est. frequency ~15–45 trades/pair/yr — floor-viable on ≥12-month windows.

## Family 2: `donchian_vol`

Breakout continuation — opposite micro-assumption from every tested family.

- New field: `breakout_lookback_bars: Option<usize>` (hours; 72/120/168 = 3/5/7 days).
- Entry: close strictly above the highest high of the prior
  `breakout_lookback_bars` bars, vol-spike veto passes (same constants as zband).
- Exits OCO-native: stop = entry − atr_mult·ATR, target = entry +
  risk_reward_ratio·(entry − stop).
- Frozen grid (3): lookbacks 72 / 120 / 168.
- Est. frequency ~30–60 trades/pair/yr.

## Budget & registry

Both register via `SignalFamily`; hashes include the new fields ONLY when
set (legacy/zband hashes byte-stable — established pattern). Worst-case
spend if everything ever runs: 12 + 6 (zband, held) + 3 + 3 = 24 > 20 ⇒ an
operator allocation decision is REQUIRED before any unlock; documented in
OOS_STUDY.md as "freeze memo" once deep-history measurements exist.
Development and measurement here spend nothing.

## Testing

Pure `hour_in_window` (wrap cases); session gating via time-shifted
synthetic pullback series (same trigger, hour in vs out of window);
Donchian breakout/no-breakout/veto synthetic cases; hash stability &
change tests for new fields; registry grid-count tests (3+3, exact values);
FULL legacy suite unchanged.

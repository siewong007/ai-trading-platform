# Z-Score Band Fade (`zband_meanrev`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the `zband_meanrev` long-only mean-reversion strategy behind the existing `TradePlan` pipeline, with its frozen 6-config grid wired into `search`.

**Architecture:** New pure-signal function dispatched by `strategy.name` inside `generate_signals`, so backtest and executor are untouched. Two new optional config fields extend the canonical hash. `backtest_runs` gains two nullable columns via an idempotent ALTER TABLE migration. `run_search` picks the pre-declared grid by family name.

**Tech Stack:** Rust 2021, sqlx/sqlite, existing crate deps only — **no new dependencies**.

## Global Constraints

- Gate thresholds frozen: PF ≥ 1.30, ≥ 3/5 profitable pairs, OOS DD < 20 %, ≥ 20 OOS trades/pair, ≤ 20 distinct config hashes EVER (`GATE_MAX_VARIANTS`).
- Frozen grid (spec §Pre-declared): (24,2.0) (24,2.5) (48,2.0) (48,2.5) (96,2.0) (96,2.5) — six hashes; no additions ever.
- Existing `ema_rsi_pullback` behavior byte-identical: same signals, same 12-config grid, same hashes.
- Executor, risk rails, metrics/gate math, exchange/ws/signed client: UNTOUCHED. No LLM in trade path.
- Old TOML must keep parsing (`#[serde(default)]` on new fields).
- TDD every task: failing test → minimal impl → green → commit.
- Binary lives at `/Users/goaltosuceed/.cargo-target/trading_platform/{debug,release}/trading_platform` (build.target-dir redirect in `.cargo/config.toml`). Run all cargo commands from the repo root.

---

### Task 1: `rolling_stats` indicator

**Files:**
- Modify: `src/indicators.rs`

**Interfaces:**
- Produces: `pub fn rolling_stats(values: &[f64], period: usize) -> (Vec<Option<f64>>, Vec<Option<f64>>)` — (rolling arithmetic mean, rolling **population** std dev); entries valid from index `period-1`, `None` before; `(None, None)` when `values.len() < period || period == 0`.

- [ ] **Step 1: Failing tests** (append to `mod tests` in `src/indicators.rs`)

```rust
    #[test]
    fn rolling_stats_hand_computed() {
        // windows of 3 ending at idx 2..4 over [1,2,3,4,5]
        // means: 2, 3, 4 ; population sigma: sqrt(2/3) each (deviations ±1,0)
        let (m, s) = rolling_stats(&[1.0, 2.0, 3.0, 4.0, 5.0], 3);
        assert_eq!(m[0], None);
        assert_eq!(m[1], None);
        assert!(approx(m[2].unwrap(), 2.0));
        assert!(approx(m[3].unwrap(), 3.0));
        assert!(approx(m[4].unwrap(), 4.0));
        let want = (2.0f64 / 3.0).sqrt();
        assert!(approx(s[2].unwrap(), want));
        assert!(approx(s[4].unwrap(), want));
    }

    #[test]
    fn rolling_stats_constant_series_zero_sigma() {
        let (m, s) = rolling_stats(&[5.0; 4], 3);
        assert!(approx(m[2].unwrap(), 5.0));
        assert!(approx(s[2].unwrap(), 0.0));
    }

    #[test]
    fn rolling_stats_short_input_all_none() {
        let (m, s) = rolling_stats(&[1.0, 2.0], 3);
        assert!(m.iter().all(|v| v.is_none()));
        assert!(s.iter().all(|v| v.is_none()));
    }
```

- [ ] **Step 2: Verify compile failure** — Run: `cargo test rolling_stats` → EXPECT: does not compile (`rolling_stats` not found).

- [ ] **Step 3: Minimal implementation** (append after `atr` in `src/indicators.rs`)

```rust
/// Rolling arithmetic mean and POPULATION standard deviation over a trailing
/// window of `period` values. Valid from index `period-1`; earlier entries
/// (and short inputs) are None.
pub fn rolling_stats(values: &[f64], period: usize) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let n = values.len();
    let mut means: Vec<Option<f64>> = vec![None; n];
    let mut stds: Vec<Option<f64>> = vec![None; n];
    if period == 0 || n < period {
        return (means, stds);
    }
    for i in (period - 1)..n {
        let w = &values[i + 1 - period..=i];
        let mean = w.iter().sum::<f64>() / period as f64;
        let var = w.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / period as f64;
        means[i] = Some(mean);
        stds[i] = Some(var.sqrt());
    }
    (means, stds)
}
```

- [ ] **Step 4: Green** — Run: `cargo test rolling_stats` → EXPECT: 3 passed.
- [ ] **Step 5: Commit**

```bash
git add src/indicators.rs
git commit -m "feat(indicators): rolling mean + population stddev"
```

---

### Task 2: Config fields + hash extension

**Files:**
- Modify: `src/strategy.rs` (struct `StrategySection` ~line 12, `config_hash` ~line 54, `test_cfg()` ~line 139)

**Interfaces:**
- Produces: `StrategySection { …, pub lookback_bars: Option<usize>, pub z_entry: Option<f64> }` (serde defaults → `None`). Hash string gains keys `lookback_bars`, `z_entry`; absent = empty value, so old-family hashes remain internally consistent (they were never persisted against literal hash strings in tests).

- [ ] **Step 1: Failing tests** (in `mod tests` of `src/strategy.rs`; also fix `test_cfg()` compile break FIRST by adding `lookback_bars: None, z_entry: None,` to it)

```rust
    const HASH_TOML_ZB: &str = r#"
[strategy]
name = "zband_meanrev"
pairs = ["BTCUSDT"]
timeframe = "1h"
lookback_bars = 48
z_entry = 2.0
ema_fast = 50
ema_slow = 200
rsi_period = 14
rsi_entry_threshold = 35.0
atr_period = 14
atr_multiplier = 2.0
risk_reward_ratio = 1.5

[backtest]
start_equity_usd = 200.0
risk_per_trade_usd = 2.0
max_notional_pct_equity = 0.5
min_notional_usd = 15.0
"#;

    #[test]
    fn zband_config_parses_with_new_fields() {
        let c: StrategyConfig = toml::from_str(HASH_TOML_ZB).unwrap();
        assert_eq!(c.strategy.name, "zband_meanrev");
        assert_eq!(c.strategy.lookback_bars, Some(48));
        assert_eq!(c.strategy.z_entry, Some(2.0));
    }

    #[test]
    fn hash_changes_with_band_params_and_defaults_are_stable() {
        let a: StrategyConfig = toml::from_str(HASH_TOML_A).unwrap();
        assert_eq!(a.strategy.lookback_bars, None);
        let mut m = a.clone();
        m.strategy.lookback_bars = Some(48);
        assert_ne!(a.config_hash(), m.config_hash(), "lookback must change hash");
        let mut m = a.clone();
        m.strategy.z_entry = Some(2.5);
        assert_ne!(a.config_hash(), m.config_hash(), "z_entry must change hash");
    }
```

- [ ] **Step 2: Verify failure** — Run: `cargo test zband_config_parses` → EXPECT: compile error (no such fields).
- [ ] **Step 3: Minimal implementation**

Struct (after `risk_reward_ratio`):

```rust
    #[serde(default)]
    pub lookback_bars: Option<usize>,
    #[serde(default)]
    pub z_entry: Option<f64>,
```

Hash — extend `fields` vec before `fields.sort()`:

```rust
            (
                "lookback_bars".into(),
                s.lookback_bars.map(|v| v.to_string()).unwrap_or_default(),
            ),
            ("z_entry".into(), s.z_entry.map(fx).unwrap_or_default()),
```

- [ ] **Step 4: Green** — Run: `cargo test` → EXPECT: all pass (old hash-stability tests unaffected).
- [ ] **Step 5: Commit**

```bash
git add src/strategy.rs
git commit -m "feat(strategy): optional band params in config + canonical hash"
```

---

### Task 3: `generate_zband_signals` + name dispatch

**Files:**
- Modify: `src/strategy.rs` (rename body of `generate_signals` → `generate_ema_rsi_signals`; new dispatcher + zband fn + `option_median` helper + tests)

**Interfaces:**
- Consumes: `rolling_stats` (Task 1), `StrategySection.lookback_bars/z_entry` (Task 2).
- Produces: `generate_signals(candles, cfg) -> Vec<Option<TradePlan>>` unchanged signature; routes `"zband_meanrev"` to zband, everything else to legacy. Private `fn option_median(vals: &[Option<f64>]) -> Option<f64>`.
- Backtest/executor keep calling `generate_signals` — zero changes downstream.

- [ ] **Step 1: Failing tests**

```rust
    fn zband_cfg() -> StrategySection {
        StrategySection {
            lookback_bars: Some(48),
            z_entry: Some(2.0),
            ..test_cfg()
        }
    }

    #[test]
    fn zband_fires_on_gradual_dip_via_dispatch() {
        // NOTE (why this shape): a pure linear ramp never exceeds z ≈ −√3, and
        // any steep drop spikes TR above the veto if the baseline is calm.
        // So: choppy baseline (alternating ±0.5) keeps median ATR ≈ 2, then an
        // 11-bar slide of −1/bar stays under the veto (TR ≈ 2 ≤ 1.5×median)
        // while pushing z past −2 by convex accumulation.
        let mut cfg = zband_cfg();
        cfg.name = "zband_meanrev".into();
        cfg.atr_period = 3;
        let mut cs: Vec<Candle> = Vec::new();
        let mut i = 0usize;
        for _ in 0..120 {
            let c = if i % 2 == 0 { 100.5 } else { 99.5 };
            cs.push(candle(i, c, c));
            i += 1;
        }
        let mut price = 99.5;
        for _ in 0..11 {
            let o = price;
            price -= 1.0;
            cs.push(candle(i, o, price));
            i += 1;
        }
        let plans = generate_signals(&cs, &cfg);
        let hits: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_some())
            .map(|(ix, _)| ix)
            .collect();
        assert!(!hits.is_empty(), "expected at least one fade entry");
        assert!(hits.iter().all(|&h| h >= 121), "fires only during the slide");
        let p = plans[hits[0]].unwrap();
        assert!(p.stop < p.entry && p.entry < p.target);
    }

    #[test]
    fn zband_vetoes_single_crash_bar() {
        let mut cfg = zband_cfg();
        cfg.name = "zband_meanrev".into();
        cfg.atr_period = 3;
        let mut cs: Vec<Candle> = Vec::new();
        for (i, _) in std::iter::repeat(()).take(130).enumerate() {
            cs.push(candle(i, 100.0, 100.0));
        }
        // one catastrophic bar: z hugely negative but ATR spikes past veto
        cs.push(candle(130, 100.0, 50.0));
        assert!(generate_signals(&cs, &cfg).iter().all(|p| p.is_none()));
    }

    #[test]
    fn zband_quiet_chop_never_fires() {
        let mut cfg = zband_cfg();
        cfg.name = "zband_meanrev".into();
        cfg.atr_period = 3;
        let mut cs: Vec<Candle> = Vec::new();
        // alternating ±0.5 around 100: |z| stays well under 2
        for i in 0..160 {
            let c = if i % 2 == 0 { 100.5 } else { 99.5 };
            cs.push(candle(i, c, c));
        }
        assert!(generate_signals(&cs, &cfg).iter().all(|p| p.is_none()));
    }

    #[test]
    fn zband_warmup_is_none() {
        let mut cfg = zband_cfg();
        cfg.name = "zband_meanrev".into();
        cfg.atr_period = 3;
        let mut cs: Vec<Candle> = Vec::new();
        for i in 0..110 {
            cs.push(candle(i, 100.0, 100.0));
        }
        cs.push(candle(110, 100.0, 90.0));
        let plans = generate_signals(&cs, &cfg);
        assert!(plans[..106].iter().all(|p| p.is_none()));
    }

    #[test]
    fn option_median_even_length_averages_middle() {
        let v = vec![Some(1.0), Some(3.0), None, Some(5.0), Some(7.0)];
        assert!(option_median(&v).is_some());
        // sorted non-None: 1,3,5,7 -> median 4.0
        assert!((option_median(&v).unwrap() - 4.0).abs() < 1e-9);
        assert_eq!(option_median(&[None, None]), None);
    }
```

- [ ] **Step 2: Verify failure** — Run: `cargo test zband` → EXPECT: compile error.
- [ ] **Step 3: Minimal implementation** (in `src/strategy.rs`; rename old `generate_signals` body to `generate_ema_rsi_signals`, keep signature; imports gain `crate::indicators::rolling_stats`)

```rust
/// Vol-spike veto constants — FIXED, deliberately not grid dimensions (spec).
const VETO_ATR_MULT: f64 = 1.5;
const VETO_MEDIAN_BARS: usize = 100;

/// Long-only z-score band fade (spec 2026-08-25): enter when close sinks
/// `z_entry` sigmas below the trailing mean, targeting a fade back to that
/// mean, ATR stop below. Refuses entries while volatility is spiking.
pub fn generate_zband_signals(
    candles: &[Candle],
    cfg: &StrategySection,
) -> Vec<Option<TradePlan>> {
    let n = candles.len();
    let mut out: Vec<Option<TradePlan>> = vec![None; n];
    let lookback = cfg.lookback_bars.unwrap_or(48);
    let z_entry = cfg.z_entry.unwrap_or(2.0);
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let highs: Vec<f64> = candles.iter().map(|c| c.high).collect();
    let lows: Vec<f64> = candles.iter().map(|c| c.low).collect();
    let (means, stds) = rolling_stats(&closes, lookback);
    let atr_v = atr(&highs, &lows, &closes, cfg.atr_period);

    let warmup = lookback.max(cfg.atr_period + VETO_MEDIAN_BARS + 1);
    for i in warmup..n {
        let (Some(m), Some(s)) = (means[i], stds[i]) else {
            continue;
        };
        if s <= 0.0 {
            continue;
        }
        let Some(a) = atr_v[i] else { continue };
        let Some(atr_med) = option_median(&atr_v[i - VETO_MEDIAN_BARS..i]) else {
            continue;
        };
        if a > VETO_ATR_MULT * atr_med {
            continue; // active crash — do not catch knives
        }
        if (closes[i] - m) / s > -z_entry {
            continue;
        }
        let entry = closes[i];
        let stop = entry - cfg.atr_multiplier * a;
        if stop >= entry {
            continue;
        }
        out[i] = Some(TradePlan {
            entry,
            stop,
            target: m,
        });
    }
    out
}

/// Median of present values; None when nothing present. Even lengths
/// average the two middle order statistics.
fn option_median(vals: &[Option<f64>]) -> Option<f64> {
    let mut v: Vec<f64> = vals.iter().filter_map(|x| *x).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    Some((v[(v.len() - 1) / 2] + v[v.len() / 2]) / 2.0)
}

/// Family dispatcher: strategy TOML selects the signal engine by name.
pub fn generate_signals(candles: &[Candle], cfg: &StrategySection) -> Vec<Option<TradePlan>> {
    if cfg.name == "zband_meanrev" {
        generate_zband_signals(candles, cfg)
    } else {
        generate_ema_rsi_signals(candles, cfg)
    }
}
```

- [ ] **Step 4: Green** — Run: `cargo test` → EXPECT: all pass including ALL legacy strategy tests (dispatch default preserves them).
- [ ] **Step 5: Commit**

```bash
git add src/strategy.rs
git commit -m "feat(strategy): zband_meanrev signal family + name dispatch"
```

---

### Task 4: Config file + free backtest sanity

**Files:**
- Create: `config/zband_meanrev.toml`

**Interfaces:**
- Consumes: everything above. `backtest` CLI persists nothing (record_hash=None) — charges zero budget.

- [ ] **Step 1: Write config** (grid point zb-3 as baseline)

```toml
# zband_meanrev — z-score band fade (spec: docs/superpowers/specs/2026-08-25-zband-meanrev-design.md)
# Grid points vary ONLY lookback_bars × z_entry; everything else fixed.
[strategy]
name = "zband_meanrev"
pairs = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT"]
timeframe = "1h"
lookback_bars = 48
z_entry = 2.0
ema_fast = 50
ema_slow = 200
rsi_period = 14
rsi_entry_threshold = 35.0
atr_period = 14
atr_multiplier = 2.0
risk_reward_ratio = 1.5

[backtest]
start_equity_usd = 200.0
risk_per_trade_usd = 2.0
max_notional_pct_equity = 0.5
min_notional_usd = 15.0
```

- [ ] **Step 2: Sanity run (free, no budget charge)**

Run: `cargo run --release -- backtest --config config/zband_meanrev.toml`
EXPECT: exits 0, prints IS/OOS report table. Trades may be sparse — informational only, NOT a gate event.

- [ ] **Step 3: Verify no budget charge**

Run: `sqlite3 data/trading.db "SELECT value FROM config_state WHERE key='variant_budget_used';"`
EXPECT: `12` (unchanged).

- [ ] **Step 4: Commit**

```bash
git add config/zband_meanrev.toml
git commit -m "feat(config): zband_meanrev strategy config (zb-3 baseline)"
```

---

### Task 5: Nullable band columns in `backtest_runs`

**Files:**
- Modify: `src/db.rs` (SCHEMA string ~line 78, `Db::open` ~line 99, `BacktestRunRow` ~line 263, `record_backtest_results` INSERT ~line 306, existing tests constructing `BacktestRunRow` ~line 455+)
- Modify: `src/main.rs` (`evaluate_config` row construction ~line 360)

**Interfaces:**
- Produces: `BacktestRunRow { …, pub lookback_bars: Option<i64>, pub z_entry: Option<f64> }`; columns `lookback_bars INTEGER NULL, z_entry REAL NULL`; migration idempotent (safe on fresh AND existing DBs).

- [ ] **Step 1: Failing test** (in `mod tests` of `src/db.rs`)

```rust
    #[tokio::test]
    async fn band_columns_round_trip() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let rows = vec![BacktestRunRow {
            symbol: "BTCUSDT".into(),
            rsi_entry: 35.0,
            atr_mult: 2.0,
            rr: 1.5,
            lookback_bars: Some(48),
            z_entry: Some(2.0),
            oos_trades: 21,
            oos_pf: 1.4,
            oos_pnl: 5.0,
            oos_dd: 3.0,
        }];
        db.record_backtest_results("zbhash", &rows).await.unwrap();
        let stored = sqlx::query(
            "SELECT lookback_bars, z_entry FROM backtest_runs WHERE config_hash='zbhash'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(stored.get::<Option<i64>, _>("lookback_bars"), Some(48));
        assert_eq!(stored.get::<Option<f64>, _>("z_entry"), Some(2.0));
    }
```

- [ ] **Step 2: Verify failure** — Run: `cargo test band_columns_round_trip` → EXPECT: compile error (struct fields missing).
- [ ] **Step 3: Minimal implementation**

a) SCHEMA: inside `CREATE TABLE IF NOT EXISTS backtest_runs(...)`, after `oos_dd REAL NOT NULL,` add:

```sql
  lookback_bars INTEGER,
  z_entry REAL,
```

b) Idempotent migration for existing DBs — in `Db::open`, after `sqlx::query(SCHEMA).execute(&pool).await?;` add:

```rust
        for ddl in [
            "ALTER TABLE backtest_runs ADD COLUMN lookback_bars INTEGER",
            "ALTER TABLE backtest_runs ADD COLUMN z_entry REAL",
        ] {
            if let Err(e) = sqlx::query(ddl).execute(&pool).await {
                let msg = format!("{e}");
                if !msg.contains("duplicate column name") {
                    return Err(e.into());
                }
            }
        }
```

c) Struct: add `pub lookback_bars: Option<i64>, pub z_entry: Option<f64>,` to `BacktestRunRow`.

d) INSERT in `record_backtest_results`: columns/values become `(config_hash,symbol,rsi_entry,atr_mult,rr,lookback_bars,z_entry,oos_trades,oos_pf,oos_pnl,oos_dd,ran_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)` with `.bind(r.lookback_bars).bind(r.z_entry)` after `.bind(r.rr)`.

e) Update ALL existing `BacktestRunRow { … }` literals in `src/db.rs` tests to include `lookback_bars: None, z_entry: None,`.

f) `src/main.rs` `evaluate_config`: in the `if record_hash.is_some()` block add:

```rust
                lookback_bars: strat.lookback_bars.map(|v| v as i64),
                z_entry: strat.z_entry,
```

- [ ] **Step 4: Green + real-DB migration check**

Run: `cargo test && sqlite3 data/trading.db "PRAGMA table_info(backtest_runs);" | grep lookback`
EXPECT: tests pass; production DB shows the new column (migration ran on open during earlier steps).

- [ ] **Step 5: Commit**

```bash
git add src/db.rs src/main.rs
git commit -m "feat(db): nullable band params on backtest_runs + idempotent migration"
```

---

### Task 6: Per-family pre-declared search grids

**Files:**
- Modify: `src/main.rs` (`run_search` grid section ~line 494, `Row` struct + printouts ~lines 545-600)

**Interfaces:**
- Produces: `fn zband_grid() -> [(usize, f64); 6]` — exactly the frozen table. `Row` loses `rsi/atr/rr` fields, gains `label: String`. Console lines change shape for zband; ema_rsi output text preserved via equivalent labels.

- [ ] **Step 1: Failing test** (in existing `mod tests` of `src/main.rs`)

```rust
    #[test]
    fn zband_grid_matches_frozen_spec_exactly() {
        let g = super::zband_grid();
        assert_eq!(
            g,
            [(24, 2.0), (24, 2.5), (48, 2.0), (48, 2.5), (96, 2.0), (96, 2.5)]
        );
    }
```

- [ ] **Step 2: Verify failure** — Run: `cargo test zband_grid_matches` → EXPECT: compile error.
- [ ] **Step 3: Minimal implementation**

a) Free function near `run_search`:

```rust
/// Frozen 2026-08-25 (spec §Pre-declared grid): 6 of the 8 reserved slots.
/// Never edited after seeing any result.
fn zband_grid() -> [(usize, f64); 6] {
    [
        (24, 2.0),
        (24, 2.5),
        (48, 2.0),
        (48, 2.5),
        (96, 2.0),
        (96, 2.5),
    ]
}
```

b) In `run_search`, replace the hardcoded triple-loop with:

```rust
    let use_zband = base.strategy.name == "zband_meanrev";
    // Pre-declared grids (spec: ≤ 20 distinct configs EVER, never resets)
    let mut variants: Vec<(f64, f64, f64)> = Vec::new(); // legacy (rsi_e, atr_m, rr)
    let mut zb_variants: Vec<(usize, f64)> = Vec::new(); // zband (lookback, z)
    if use_zband {
        zb_variants.extend(zband_grid());
    } else {
        for rsi_e in [30.0, 35.0] {
            for atr_m in [1.5, 2.0, 2.5] {
                for rr in [1.5, 2.0] {
                    variants.push((rsi_e, atr_m, rr));
                }
            }
        }
    }
```

c) Loop head branches on family; both arms share the same body except how `strat` is built and labeled:

```rust
    let mut rows: Vec<Row> = Vec::new();
    let mut run_variant = |strat: crate::strategy::StrategySection,
                           label: String,
                           used: &mut u32,
                           known: &mut std::collections::HashSet<String>,
                           unlock: bool| async move {
        // (body identical for both families: hash, budget check, evaluate,
        //  Row push — see legacy arm below; only strat/label differ)
    };
```

Concretely, inside the existing loop replace the fixed triple destructure with:

```rust
    for variant in variants_or_zb {
        let mut strat = base.strategy.clone();
        let label;
        if use_zband {
            let (lb, z) = variant_zb; // (usize, f64)
            strat.lookback_bars = Some(lb);
            strat.z_entry = Some(z);
            label = format!("lb={lb:>3} z={z}");
        } else {
            let (rsi_e, atr_m, rr) = variant_leg; // (f64, f64, f64)
            strat.rsi_entry_threshold = rsi_e;
            strat.atr_multiplier = atr_m;
            strat.risk_reward_ratio = rr;
            label = format!("rsi={rsi_e:>4} atr={atr_m:>3} rr={rr:>3}");
        }
```

(If Rust's ownership makes one unified loop awkward, two sequential `for` loops — one per family — calling a shared `evaluate_and_row(&db, strat, &base.backtest, hash, used, &mut known, unlock) -> Row` helper is equally acceptable. The budget/refusal semantics MUST be identical in both paths.)

d) `Row` struct becomes `{ label: String, pairs_passing_floor: usize, profitable_pairs: usize, worst_pf: f64, total_oos_trades: usize, pass: bool }`; progress `println!` prints `{label} | floor:{} prof:{profitable} worstPF:{:.2} trades:{} budget:{used}/{GATE_MAX_VARIANTS} | {}`; ranked-summary/best-line print `row.label`. Sorting keyed off `worst_pf`/`profitable_pairs` unchanged.

- [ ] **Step 4: Green + replay sanity**

Run: `cargo test && cargo run --release -- search --config config/strategy_ema_rsi.toml | tail -4`
EXPECT: tests pass; legacy search replays free, final line `OVERALL: NO variant passed the gate` unchanged.

Then (informational, still FREE — these six hashes are NOT spent until the operator unlock):

Run: `cargo run --release -- backtest --config config/zband_meanrev.toml`
EXPECT: exit 0.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(search): per-family pre-declared grids (zband frozen 6)"
```

---

### Task 7: Full verification sweep

- [ ] **Step 1:** `cargo test` → all green.
- [ ] **Step 2:** `./scripts/smoke_local.sh` → ends `smoke: OK (build + tests + keyless refusal + export)`.
- [ ] **Step 3:** `sqlite3 data/trading.db "SELECT DISTINCT config_hash FROM backtest_runs;" | wc -l` → `12` (budget intact; zband uncharged).
- [ ] **Step 4:** Fix anything red, then final commit if needed:

```bash
git add -A && git commit -m "chore: verification sweep green"
```

(no-op allowed if nothing changed)

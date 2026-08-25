# Research Upgrade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Checkbox steps.

**Goal:** Land the 9 research-ability items from the companion design doc, TDD, one commit each, existing behavior byte-compatible by default.

**Global constraints:** no new crates; gate/grid/hash/live untouched; new flags default off; every task ends `cargo test` green + commit on feat/research-upgrade.

---

### Task A (=#4): window bounds persistence
Files: src/db.rs (SCHEMA backtest_runs +2 nullable cols, migration list, BacktestRunRow fields oos_start_ts/oos_end_ts Option<i64>, INSERT), src/main.rs evaluate_config fills from candles[split_idx].open_time / candles.last().open_time.
Test: db round-trip Some values; update all row literals with None.
Commit: "feat(db): persist OOS window bounds per run"

### Task B (=#5): trade attribution
Files: src/backtest.rs TradeRecord += mfe_r,mae_r,bars_held (f64,f64,i64); Position += mfe,mae,bars tracked in loop: while open each candle r=(c.close-entry)/(entry-stop); mfe=max(mfe,r) when r>0; mae=min(mae,r); bars+=1; stamp into TradeRecord at exit.
main.rs print_report: attribution block per config (avg MFE/MAE over trades, median bars_held, worst 4h-bucket by pnl using entry_ts). export CSV (trades_csv main.rs) += 3 cols; header test update.
Tests: synthetic one-trade scenario hand-computed MFE/MAE/bars; CSV header contains columns.
Commit: "feat(backtest): MFE/MAE/holding attribution"

### Task C (=#1): --folds K stability
Files: src/main.rs Cli::Backtest += folds:usize default 0; after OOS report, if folds>0: split candle span into K equal consecutive windows AFTER the IS split point; partition oos_trades per window (entry_ts bounds like IS/OOS partition); metrics::compute per fold; print fold table + minPF/spread line. Zero folds → unchanged output.
Tests: metrics-level unit — build 3 fake trades across 3 windows, assert partition counts; CLI default parse.
Commit: "feat(backtest): temporal fold stability (--folds)"

### Task D (=#7): report subcommand
Files: src/main.rs Report { json:bool, md:bool }; fn run_report(db) collecting: variant_budget_used, distinct hashes w/ latest ran_at + symbol rows (incl. new window bounds), halt day_state keys; JSON via serde_json to stdout; --md renders table text. Register command in enum.
Tests: memory db seeded → run_report json parses & contains hash; md contains budget line.
Commit: "feat(cli): report subcommand (ledger dump)"

### Task E (=#3): luck-adjusted best-of-N
Files: src/metrics.rs fn expected_max_normal(t:usize)->f64 (μ+σ·E[max]; E[max] via polynomial approx of Φ inverse integral — implement standard approximation for E[max] of t iid normals: use formula via expected value integration with erf approx? Simplest defensible: simulate-free closed form using Taylor: E[max]≈σ·(sqrt(2 ln t) − (ln ln t + ln 4π)/(2 sqrt(2 ln t))) for t≥2, 0 for t<2). search ranked-output footer + report md append line: observed best worstPF vs expected-by-luck given used trials.
Tests: known-value checks (t=1→0; monotone increasing; t=12 sanity ~1.5-1.8σ).
Commit: "feat(metrics): luck-adjusted best-of-N expectation"

### Task F (=#9): permutation test
Files: src/backtest.rs extract pub fn run_with_signals(candles,strat,bt,signals)->BacktestOutput; run() delegates. src/main.rs Permutetest {config,trials:usize default 200}: generate_signals once; collect plans vec; xorshift64* RNG (internal impl, seed from fixed const + trials); for each trial: shuffle plan ASSIGNMENT onto random signal-slot indices (same count, same valid index set), rebuild Vec<Option<TradePlan>>, run_with_signals per pair (use BTCUSDT only for speed? no — all pairs, aggregate pnl), compute aggregate PF proxy = total_pnl>0 share + mean pnl; p = frac(trials with mean_pnl ≥ actual). Print null mean/std/p.
Tests: run_with_signals equals run() output on fixture; xorshift determinism; perfect-timing signals → p small (<0.05) with 100 trials; shuffled-length conservation.
Commit: "feat(backtest): permutation significance test"

### Task G (=#2): sensitivity plateaus
Files: src/main.rs Sensitivity {config, pct:f64 default 20.0}; family param table: zband → [(lookback_bars,mul),(z_entry,mul),(atr_multiplier,mul)]; legacy → [(rsi_entry_threshold..),(atr_multiplier..),(risk_reward_ratio..)]; for each param × {1±pct/100}: clone strat, apply, evaluate_config(None), sum OOS pnl. Output table + verdict PLATEAU if base pnl>0 && ≥half neighbors >0.
Tests: plateau decision fn pure unit; param-table completeness per family name.
Commit: "feat(cli): sensitivity plateau analysis"

### Task H (=#8): multi-timeframe fetch
Files: src/main.rs Fetch += interval:Option<String>; pass to ex.fetch_klines + upsert_klines overriding cfg tf when present.
Tests: cli parse default None; db already keyed by interval (load_klines two-interval round trip exists implicitly — add explicit test).
Commit: "feat(fetch): --interval override"

### Task I (=#6): SignalFamily registry
Files: src/strategy.rs trait SignalFamily {name:&'static str; signals(&StrategySection,&[Candle])->Vec<Option<TradePlan>>; grid(&StrategySection)->Vec<(String,StrategySection)>} ; static registry [EmaRsiFamily, ZbandFamily]; generate_signals looks up by cfg.name (unknown → ema_rsi as today). main.rs run_search jobs built via registry lookup (fallback legacy loop for unknown names preserved).
Tests: FULL EXISTING SUITE MUST PASS UNCHANGED (regression proof); plus registry returns frozen zband_grid values and 12-variant legacy grid.
Commit: "refactor(strategy): SignalFamily trait registry"

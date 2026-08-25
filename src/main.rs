mod backtest;
mod bus;
mod cache;
mod db;
mod exchange;
mod executor;
mod indicators;
mod metrics;
mod pipeline;
mod risk;
mod signed;
mod strategy;
mod types;
mod ws;

use backtest::{run, BacktestOutput};
use clap::{Parser, Subcommand};
use db::{BacktestRunRow, Db, TradeRow};
use exchange::Exchange;
use executor::{CycleOutcome, Executor};
use metrics::{
    evaluate_gate, max_drawdown_pct, GateVerdict, Metrics, PairReport, GATE_MAX_VARIANTS,
};
use signed::{Keys, SignedClient};
use strategy::{BacktestSection, SignalFamily, StrategyConfig, StrategySection};
use types::Candle;

#[derive(Parser)]
#[command(name = "trading_platform", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download and cache ~18 months of 1h klines for the configured pairs
    Fetch {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        /// override the strategy timeframe for this cache (e.g. 15m, 4h)
        #[arg(long)]
        interval: Option<String>,
    },
    /// Backtest one strategy config; print IS/OOS report + gate verdict
    Backtest {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        /// temporal fold stability analysis (0 = off)
        #[arg(long, default_value_t = 0)]
        folds: usize,
    },
    /// Run the pre-declared variant grid within the 20-config budget
    Search {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        /// Allow new config hashes this run (requires typing NEW-OOS)
        #[arg(long)]
        unlock_new_study: bool,
    },
    /// One-at-a-time parameter neighborhood robustness (free, no budget)
    Sensitivity {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        #[arg(long, default_value_t = 20.0)]
        pct: f64,
    },

    /// Permutation significance of a config's simulated PnL
    Permutetest {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        #[arg(long, default_value_t = 200)]
        trials: usize,
    },

    /// Dump the research ledger (budget, hashes, windows, halts)
    Report {
        /// emit human-readable markdown instead of JSON
        #[arg(long, default_value_t = false)]
        md: bool,
    },

    /// Export trades table to CSV (tax records)
    Export {
        #[arg(long, default_value = "data/trades_export.csv")]
        out: String,
    },
    /// Run the executor loop (TESTNET default; --live requires typed GO)
    Trade {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        #[arg(long)]
        testnet: bool,
        #[arg(long)]
        live: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        once: bool,
    },
    /// Kill switch: cancel all orders, market-reduce, halt until manual reset
    Flatten {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        #[arg(long)]
        testnet: bool,
        #[arg(long)]
        live: bool,
    },
}

const BINANCE_BASE: &str = "https://api.binance.com";
const TESTNET_BASE: &str = "https://testnet.binance.vision";
/// ~18 months of hourly candles worth of lookback (spec §5)
const LOOKBACK_DAYS: i64 = 550;
const IS_FRACTION: f64 = 0.70;

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;
    match cli.command {
        Command::Fetch { config, interval } => rt.block_on(run_fetch(&config, interval))?,
        Command::Permutetest { config, trials } => {
            rt.block_on(run_permutetest(&config, trials))?;
        }
        Command::Sensitivity { config, pct } => {
            rt.block_on(run_sensitivity(&config, pct))?;
        }
        Command::Report { md } => {
            let db = rt.block_on(Db::open_default())?;
            let ledger = rt.block_on(db.ledger())?;
            if md {
                println!("# research ledger");
                println!("- budget: {}/20", ledger["variant_budget_used"]);
                println!("- distinct configs: {}", ledger["distinct_hashes"]);
                let pfs: Vec<f64> = ledger["hashes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|h| h["results"].as_array().unwrap().iter())
                    .filter_map(|r| r["oos_pf"].as_f64())
                    .collect();
                if let Some(exp) = crate::metrics::luck_adjusted_best_pf(&pfs) {
                    let observed = pfs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    println!(
                        "- luck-adjusted best PF by chance ({} trials): {:.2}; observed best: {:.2} {}",
                        pfs.len(),
                        exp,
                        observed,
                        if observed > exp { "→ beats luck" } else { "→ within luck" }
                    );
                }
                for h in ledger["hashes"].as_array().unwrap() {
                    let sym: Vec<String> = h["results"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|r| {
                            format!(
                                "{} n={} pf={:.2} pnl={:+.2}",
                                r["symbol"], r["oos_trades"], r["oos_pf"], r["oos_pnl"]
                            )
                        })
                        .collect();
                    println!("- `{}` — {}", &h["hash"].as_str().unwrap()[..8], sym.join("; "));
                }
            } else {
                println!("{ledger}");
            }
        }
        Command::Backtest { config, folds } => {
            let results = rt.block_on(run_backtest(&config))?;
            if folds > 0 {
                print_folds(&results, folds);
            }
        }
        Command::Search {
            config,
            unlock_new_study,
        } => rt.block_on(run_search(&config, unlock_new_study))?,
        Command::Export { out } => rt.block_on(run_export(&out))?,
        Command::Trade {
            config,
            testnet,
            live,
            dry_run,
            once,
        } => {
            let base = select_base(testnet, live)?;
            rt.block_on(run_trade(&config, base, dry_run, once))?
        }
        Command::Flatten {
            config,
            testnet,
            live,
        } => {
            let base = select_base(testnet, live)?;
            rt.block_on(run_flatten(&config, base))?
        }
    }
    Ok(())
}

fn select_base(testnet: bool, live: bool) -> anyhow::Result<&'static str> {
    anyhow::ensure!(
        !(testnet && live),
        "--testnet and --live are mutually exclusive"
    );
    Ok(if live { BINANCE_BASE } else { TESTNET_BASE })
}

fn gate_banner(latest_pass: Option<bool>) -> String {
    match latest_pass {
        None => "gate verdict on record: NO GATE RESULT ON RECORD".to_string(),
        Some(false) => "latest stored search OVERALL verdict: FAIL\n\
                        PRE-REGISTERED GATE VERDICT: NO-GO — running against spec §5 advice"
            .to_string(),
        Some(true) => {
            "latest stored search OVERALL verdict: GO (pre-registered gate PASS on record)"
                .to_string()
        }
    }
}

fn confirm_live_input(input: &str) -> bool {
    input.trim() == "GO"
}

fn confirm_live(latest_pass: Option<bool>) -> anyhow::Result<()> {
    println!("{}", gate_banner(latest_pass));
    println!("LIVE mode trades real funds. Type GO to continue:");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    anyhow::ensure!(
        confirm_live_input(&line),
        "aborted: live trading requires the literal word GO on stdin"
    );
    Ok(())
}

fn live_trade_permitted(latest_pass: Option<bool>) -> bool {
    latest_pass == Some(true)
}

fn refuse_live_trade(latest_pass: Option<bool>) -> anyhow::Result<()> {
    if live_trade_permitted(latest_pass) {
        return Ok(());
    }
    anyhow::bail!(
        "{}\nlive trading refused until a stored overall gate PASS exists",
        gate_banner(latest_pass)
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn telegram_send(text: &str) -> anyhow::Result<()> {
    let token = std::env::var("TELEGRAM_BOT_TOKEN")?;
    let chat_id = std::env::var("TELEGRAM_CHAT_ID")?;
    let sanitize = |e: reqwest::Error| {
        // without_url: never leak the bot token embedded in request URLs
        anyhow::anyhow!("telegram request failed: {}", e.without_url())
    };
    let resp = reqwest::Client::new()
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .json(&serde_json::json!({ "chat_id": chat_id, "text": text }))
        .send()
        .await
        .map_err(sanitize)?;
    resp.error_for_status().map_err(sanitize)?;
    Ok(())
}

async fn notify(text: &str) {
    if std::env::var("TELEGRAM_BOT_TOKEN").is_err() || std::env::var("TELEGRAM_CHAT_ID").is_err() {
        return;
    }
    if let Err(e) = telegram_send(text).await {
        tracing::warn!("telegram notify failed: {e:#}");
    }
}

async fn run_trade(config_path: &str, base: &str, dry_run: bool, once: bool) -> anyhow::Result<()> {
    let cfg = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;
    let verdict = db.latest_search_overall_verdict().await?;
    println!("{}", gate_banner(verdict));
    if base == BINANCE_BASE {
        refuse_live_trade(verdict)?;
        confirm_live(verdict)?;
    }
    let ex = Executor {
        sc: SignedClient::new(base, Keys::from_env()?)?,
        db,
        strat: cfg.strategy.clone(),
        bt: cfg.backtest.clone(),
        hash: cfg.config_hash(),
        dry_run,
    };
    let reconcile = ex.reconcile().await?;
    tracing::info!("{reconcile}");
    notify(&format!("trade started ({base})\n{reconcile}")).await;
    let mut cycle: u64 = 0;
    loop {
        cycle += 1;
        match ex.run_cycle(now_ms()).await {
            Ok(outcome) => {
                let open = matches!(
                    outcome,
                    CycleOutcome::PlacedEntry { .. }
                        | CycleOutcome::AwaitingFill
                        | CycleOutcome::PlacedOco { .. }
                        | CycleOutcome::PositionLive
                );
                tracing::info!(
                    "heartbeat alive cycle={cycle} pos={}",
                    if open { "open" } else { "flat" }
                );
                if once {
                    break;
                }
            }
            Err(e) => {
                tracing::error!("cycle {cycle} failed: {e:#}");
                notify(&format!("trade cycle {cycle} error: {e:#}")).await;
            }
        }
        let day = risk::load_day_state(&ex.db, &ex.hash).await?;
        if day.halted {
            let reason = day.halt_reason.unwrap_or_else(|| "unspecified".into());
            tracing::warn!("HALT: {reason} — exiting until manual reset");
            notify(&format!("HALT: {reason} — engine stopped")).await;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
    Ok(())
}

async fn run_flatten(config_path: &str, base: &str) -> anyhow::Result<()> {
    let cfg = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;
    let verdict = db.latest_search_overall_verdict().await?;
    println!("{}", gate_banner(verdict));
    if base == BINANCE_BASE {
        confirm_live(verdict)?;
    }
    let ex = Executor {
        sc: SignedClient::new(base, Keys::from_env()?)?,
        db,
        strat: cfg.strategy.clone(),
        bt: cfg.backtest.clone(),
        hash: cfg.config_hash(),
        dry_run: false,
    };
    let report = ex.flatten_all().await?;
    println!("{report}");
    notify(&format!("FLATTEN executed ({base}):\n{report}")).await;
    Ok(())
}

async fn run_fetch(
    config_path: &str,
    interval: Option<String>,
) -> anyhow::Result<()> {
    let cfg = StrategyConfig::load(config_path)?;
    let tf = interval.unwrap_or_else(|| cfg.strategy.timeframe.clone());
    let db = Db::open_default().await?;
    let ex = Exchange::new(BINANCE_BASE)?;
    let start = chrono::Utc::now().timestamp_millis() - LOOKBACK_DAYS * 86_400_000;
    for pair in &cfg.strategy.pairs {
        let ks = ex.fetch_klines(pair, &tf, start).await?;
        let span_days = if ks.len() > 1 {
            (ks.last().unwrap().open_time - ks.first().unwrap().open_time) / 86_400_000
        } else {
            0
        };
        db.upsert_klines(pair, &tf, &ks).await?;
        tracing::info!("{pair}: cached {} candles (~{span_days} days)", ks.len());
    }
    Ok(())
}

async fn load_series(db: &Db, pair: &str, timeframe: &str) -> anyhow::Result<Vec<Candle>> {
    let candles = db.load_klines(pair, timeframe).await?;
    if candles.len() < 500 {
        anyhow::bail!(
            "{pair}: only {} cached candles — run `fetch` first",
            candles.len()
        );
    }
    Ok(candles)
}

struct PairResult {
    symbol: String,
    is_metrics: Metrics,
    oos_metrics: Metrics,
    attribution: Option<crate::backtest::Attribution>,
    oos_trades: Vec<crate::backtest::TradeRecord>,
}

async fn evaluate_config(
    db: &Db,
    strat: &StrategySection,
    bt: &BacktestSection,
    record_hash: Option<&str>,
) -> anyhow::Result<Vec<PairResult>> {
    let mut results = Vec::new();
    let mut rows: Vec<BacktestRunRow> = Vec::new();
    for pair in &strat.pairs {
        let candles = load_series(db, pair, &strat.timeframe).await?;
        let out: BacktestOutput = run(&candles, strat, bt);

        let split_idx = ((candles.len() as f64 * IS_FRACTION) as usize).min(candles.len() - 1);
        let oos_start_ts = candles[split_idx].open_time;

        let (is_trades, oos_trades): (Vec<_>, Vec<_>) = out
            .trades
            .iter()
            .cloned()
            .partition(|t| t.entry_ts < oos_start_ts);
        let (is_curve, oos_curve): (Vec<_>, Vec<_>) = out
            .equity_curve
            .iter()
            .cloned()
            .partition(|p| p.ts < oos_start_ts);

        let is_metrics = metrics::compute(&is_trades, &is_curve);
        let mut oos_metrics = metrics::compute(&oos_trades, &oos_curve);
        // DD must be measured within the OOS window only
        oos_metrics.max_drawdown_pct = max_drawdown_pct(&oos_curve);

        if record_hash.is_some() {
            rows.push(BacktestRunRow {
                symbol: pair.clone(),
                rsi_entry: strat.rsi_entry_threshold,
                atr_mult: strat.atr_multiplier,
                rr: strat.risk_reward_ratio,
                lookback_bars: strat.lookback_bars.map(|v| v as i64),
                z_entry: strat.z_entry,
                oos_start_ts: Some(oos_start_ts),
                oos_end_ts: candles.last().map(|c| c.open_time),
                oos_trades: oos_metrics.total_trades as i64,
                oos_pf: oos_metrics.profit_factor,
                oos_pnl: oos_metrics.net_pnl,
                oos_dd: oos_metrics.max_drawdown_pct,
            });
        }

        let attribution = crate::backtest::summarize_attribution(&out.trades);
        results.push(PairResult {
            symbol: pair.clone(),
            attribution,
            oos_trades: oos_trades.clone(),
            is_metrics,
            oos_metrics,
        });
    }
    if let Some(hash) = record_hash {
        // single transactional write: rows upserted + budget charged once per
        // NEW distinct hash (re-runs are free)
        db.record_backtest_results(hash, &rows).await?;
    }
    Ok(results)
}

fn print_report(results: &[PairResult]) -> GateVerdict {
    println!(
        "{:<10} {:>8} {:>8} {:>8} | {:>8} {:>8} {:>8} {:>9}",
        "pair", "IS_n", "IS_PF", "IS_pnl", "OOS_n", "OOS_PF", "OOS_DD%", "OOS_pnl"
    );
    let reports: Vec<PairReport> = results
        .iter()
        .map(|r| PairReport {
            symbol: r.symbol.clone(),
            metrics: r.oos_metrics.clone(),
        })
        .collect();
    for (r, rep) in results.iter().zip(reports.iter()) {
        let pf = |m: &Metrics| {
            if m.profit_factor.is_infinite() {
                "inf".to_string()
            } else {
                format!("{:.2}", m.profit_factor)
            }
        };
        println!(
            "{:<10} {:>8} {:>8} {:>8.2} | {:>8} {:>8} {:>8.1} {:>9.2}",
            rep.symbol,
            r.is_metrics.total_trades,
            pf(&r.is_metrics),
            r.is_metrics.net_pnl,
            r.oos_metrics.total_trades,
            pf(&r.oos_metrics),
            r.oos_metrics.max_drawdown_pct,
            r.oos_metrics.net_pnl,
        );
    }
    if results.iter().any(|r| r.attribution.is_some()) {
        println!("\nATTRIBUTION (all simulated trades per pair)");
        println!(
            "{:<10} {:>8} {:>8} {:>8} {:>10}",
            "pair", "avgMFE_R", "avgMAE_R", "medBars", "worst4h"
        );
        for r in results {
            let Some(a) = &r.attribution else { continue };
            println!(
                "{:<10} {:>8.2} {:>8.2} {:>8.0} {:>10} ({:+.2})",
                r.symbol, a.avg_mfe_r, a.avg_mae_r, a.median_bars_held,
                format!("{}h", a.worst_bucket.0 * 4), a.worst_bucket.1
            );
        }
    }
    let verdict = evaluate_gate(&reports);
    println!(
        "\nGATE VERDICT: {}",
        if verdict.pass { "PASS ✅" } else { "FAIL ❌" }
    );
    for reason in &verdict.reasons {
        println!("  - {reason}");
    }
    for note in &verdict.notes {
        println!("  note: {note}");
    }
    verdict
}


/// K equal consecutive [start,end) windows covering [start, end].
pub fn fold_bounds(start: i64, end: i64, k: usize) -> Vec<(i64, i64)> {
    if k == 0 || end <= start {
        return vec![];
    }
    let w = (end - start) / k as i64;
    (0..k)
        .map(|i| {
            let a = start + (i as i64) * w;
            let b = if i == k - 1 { end } else { a + w };
            (a, b)
        })
        .collect()
}

fn print_folds(results: &[PairResult], folds: usize) {
    println!("\nFOLD STABILITY (K={folds}, consecutive OOS windows)");
    for r in results {
        if r.oos_trades.is_empty() {
            continue;
        }
        let start = r.oos_trades.iter().map(|t| t.entry_ts).min().unwrap();
        let end = r.oos_trades.iter().map(|t| t.exit_ts).max().unwrap();
        let bounds = fold_bounds(start, end, folds);
        let cells: Vec<String> = bounds
            .iter()
            .map(|(a, b)| {
                let seg: Vec<_> = r
                    .oos_trades
                    .iter()
                    .filter(|t| t.entry_ts >= *a && t.entry_ts < *b)
                    .collect();
                let pnl: f64 = seg.iter().map(|t| t.pnl).sum();
                format!("{:>2}:{:+7.2}", seg.len(), pnl)
            })
            .collect();
        println!("{:<10} {}", r.symbol, cells.join("  "));
    }
    println!("RANKING IS ANALYSIS ONLY — fold spread is a robustness signal, not a gate input.");
}


/// xorshift64* — deterministic, dependency-free RNG for permutation tests.
pub struct XorShift(u64);
impl XorShift {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    /// k distinct indices sampled from 0..n (partial Fisher-Yates)
    pub fn sample_distinct(&mut self, n: usize, k: usize) -> Vec<usize> {
        let mut pool: Vec<usize> = (0..n).collect();
        let k = k.min(n);
        for i in 0..k {
            let j = i + (self.next_u64() % ((n - i) as u64)) as usize;
            pool.swap(i, j);
        }
        pool.truncate(k);
        pool
    }
}

fn aggregate_pnl(candles_by_pair: &[(String, Vec<crate::types::Candle>)],
                 strat: &StrategySection,
                 bt: &BacktestSection,
                 signals_by_pair: &[Vec<Option<crate::strategy::TradePlan>>]) -> f64 {
    candles_by_pair
        .iter()
        .zip(signals_by_pair)
        .map(|((_, cs), sigs)| crate::backtest::run_with_signals(cs, strat, bt, sigs.clone())
            .trades.iter().map(|t| t.pnl).sum::<f64>())
        .sum()
}

async fn run_permutetest(config_path: &str, trials: usize) -> anyhow::Result<()> {
    let cfg = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;
    anyhow::ensure!(trials >= 10, "need at least 10 trials");
    let mut by_pair: Vec<(String, Vec<crate::types::Candle>)> = Vec::new();
    let mut real_sigs: Vec<Vec<Option<crate::strategy::TradePlan>>> = Vec::new();
    for pair in &cfg.strategy.pairs {
        let candles = load_series(&db, pair, &cfg.strategy.timeframe).await?;
        let sigs = crate::strategy::generate_signals(&candles, &cfg.strategy);
        if sigs.iter().any(|s| s.is_some()) {
            by_pair.push((pair.clone(), candles));
            real_sigs.push(sigs);
        }
    }
    anyhow::ensure!(!by_pair.is_empty(), "config produced no signals to permute");

    // valid slots: any candle index where the original family could emit a
    // signal — conservatively the observed min..len-2 band
    let actual = aggregate_pnl(&by_pair, &cfg.strategy, &cfg.backtest, &real_sigs);
    let mut rng = XorShift::new(0x9E37_79B9_7F4A_7C15 ^ trials as u64);
    let mut null: Vec<f64> = Vec::with_capacity(trials);
    let rng_state = std::cell::RefCell::new(&mut rng);
    let pairs_n = by_pair.len();
    for _ in 0..trials {
        let mut sigs: Vec<Vec<Option<crate::strategy::TradePlan>>> =
            vec![vec![None; 0]; pairs_n];
        {
            let mut r = rng_state.borrow_mut();
            for (pi, (_, cs)) in by_pair.iter().enumerate() {
                let plan_slots: Vec<usize> = real_sigs[pi]
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.is_some())
                    .map(|(i, _)| i)
                    .collect();
                let plans: Vec<crate::strategy::TradePlan> = plan_slots
                    .iter()
                    .map(|&i| real_sigs[pi][i].unwrap())
                    .collect();
                let lo = plan_slots.first().copied().unwrap_or(1);
                let hi = cs.len().saturating_sub(2);
                let n_slots = hi.saturating_sub(lo);
                let mut v: Vec<Option<crate::strategy::TradePlan>> = vec![None; cs.len()];
                if n_slots > 0 && !plans.is_empty() {
                    for idx in r.sample_distinct(n_slots, plans.len()) {
                        v[lo + idx] = Some(plans[idx % plans.len()]);
                    }
                }
                sigs[pi] = v;
            }
        }
        null.push(aggregate_pnl(&by_pair, &cfg.strategy, &cfg.backtest, &sigs));
    }
    drop(rng_state);
    let ge = null.iter().filter(|&&p| p >= actual).count();
    let mean = null.iter().sum::<f64>() / null.len() as f64;
    let var = null.iter().map(|p| (p - mean) * (p - mean)).sum::<f64>() / null.len() as f64;
    println!("PERMUTATION TEST ({trials} trials, deterministic seed)");
    println!("actual aggregate pnl : {actual:+.2}");
    println!("null mean/std        : {mean:+.2} / {:.2}", var.sqrt());
    println!("null p-value         : {}/{} = {:.3}", ge, null.len(),
        ge as f64 / null.len() as f64);
    println!(
        "verdict: {}",
        if ge == 0 { "p < 1/trials — strong evidence of edge" }
        else if (ge as f64) / (null.len() as f64) < 0.05 { "significant at 0.05" }
        else { "NOT distinguishable from luck" }
    );
    Ok(())
}


/// PLATEAU when the base config profits AND at least half its one-at-a-time
/// neighbors also profit — an edge that dies at ±pct neighbors is curve-fit.
fn plateau_verdict(base_pnl: f64, neighbor_pnls: &[f64]) -> &'static str {
    if neighbor_pnls.is_empty() {
        return "NO NEIGHBORS";
    }
    let positive_neighbors = neighbor_pnls.iter().filter(|&&p| p > 0.0).count();
    if base_pnl > 0.0 && positive_neighbors * 2 >= neighbor_pnls.len() {
        "PLATEAU"
    } else if base_pnl > 0.0 {
        "SPIKE (neighbors do not confirm)"
    } else {
        "BASE UNPROFITABLE"
    }
}

/// Tunable params per family: (key, human label). Applied multiplicatively.
fn family_params(name: &str) -> Vec<(&'static str, &'static str)> {
    match name {
        "zband_meanrev" => vec![
            ("lookback_bars", "lookback"),
            ("z_entry", "z_entry"),
            ("atr_multiplier", "atr_mult"),
        ],
        _ => vec![
            ("rsi_entry_threshold", "rsi_entry"),
            ("atr_multiplier", "atr_mult"),
            ("risk_reward_ratio", "rr"),
        ],
    }
}

fn apply_param(strat: &mut StrategySection, key: &str, mul: f64) {
    match key {
        "lookback_bars" => {
            let b = strat.lookback_bars.unwrap_or(48);
            strat.lookback_bars = Some(((b as f64 * mul).round() as usize).max(5));
        }
        "z_entry" => {
            let z = strat.z_entry.unwrap_or(2.0);
            strat.z_entry = Some(z * mul);
        }
        "rsi_entry_threshold" => strat.rsi_entry_threshold *= mul,
        "atr_multiplier" => strat.atr_multiplier *= mul,
        "risk_reward_ratio" => strat.risk_reward_ratio *= mul,
        _ => {}
    }
}

async fn run_sensitivity(config_path: &str, pct: f64) -> anyhow::Result<()> {
    let cfg = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;
    anyhow::ensure!((0.0..50.0).contains(&pct), "pct must be in [0,50)");
    let params = family_params(&cfg.strategy.name);
    let base_results =
        evaluate_config(&db, &cfg.strategy, &cfg.backtest, None).await?;
    let base_pnl: f64 = base_results
        .iter()
        .map(|r| r.oos_metrics.net_pnl)
        .sum();
    println!(
        "SENSITIVITY (±{:.0}%, one-at-a-time; free runs, no budget charge)",
        pct
    );
    println!("base aggregate OOS pnl: {base_pnl:+.2}");
    let mut neighbors: Vec<f64> = Vec::new();
    for (key, label) in &params {
        for dir in [-1.0f64, 1.0] {
            let mul = 1.0 + dir * pct / 100.0;
            let mut s = cfg.strategy.clone();
            apply_param(&mut s, key, mul);
            let res = evaluate_config(&db, &s, &cfg.backtest, None).await?;
            let pnl: f64 = res.iter().map(|r| r.oos_metrics.net_pnl).sum();
            println!("{:<10} x{:<6} pnl {:+9.2} {}", label, mul, pnl,
                if pnl > 0.0 { "+" } else { "-" });
            neighbors.push(pnl);
        }
    }
    println!("verdict: {} (base {:+.2}, {}/{} neighbors positive)",
        plateau_verdict(base_pnl, &neighbors), base_pnl,
        neighbors.iter().filter(|&&p| p > 0.0).count(), neighbors.len());
    Ok(())
}

async fn run_backtest(config_path: &str) -> anyhow::Result<Vec<PairResult>> {
    let cfg = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;
    let results = evaluate_config(&db, &cfg.strategy, &cfg.backtest, None).await?;
    print_report(&results);
    Ok(results)
}

fn confirm_new_study_input(input: &str) -> bool {
    input.trim() == "NEW-OOS"
}

fn confirm_new_study() -> anyhow::Result<()> {
    println!("Unlocking reserved variant slots. Type NEW-OOS to continue:");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    anyhow::ensure!(
        confirm_new_study_input(&line),
        "aborted: new configs require the literal phrase NEW-OOS on stdin"
    );
    Ok(())
}

/// Pre-registered rule (spec §5): at most GATE_MAX_VARIANTS DISTINCT configs
/// may EVER run. Re-running an already-known hash is free; a NEW hash that
/// would push the distinct total past the cap is refused BEFORE any work.
/// New hashes also require `--unlock-new-study` + NEW-OOS for this run.
fn check_variant_budget(
    used_distinct: u32,
    is_new_hash: bool,
    new_study_unlocked: bool,
) -> anyhow::Result<()> {
    if is_new_hash && !new_study_unlocked {
        anyhow::bail!(
            "refusing new config: remaining variant slots are reserved for a documented new OOS study. \
             Re-run a known hash, or pass --unlock-new-study and type NEW-OOS. \
             ({used_distinct}/{GATE_MAX_VARIANTS} distinct configs used)"
        );
    }
    if is_new_hash && used_distinct >= GATE_MAX_VARIANTS {
        anyhow::bail!(
            "refusing new config #{}: variant budget exhausted \
             ({used_distinct}/{GATE_MAX_VARIANTS} distinct configs). Per spec §5 the \
             budget never resets — new research requires a fresh out-of-sample \
             window, documented before running.",
            used_distinct + 1
        );
    }
    Ok(())
}

async fn run_search(config_path: &str, unlock_new_study: bool) -> anyhow::Result<()> {
    let base = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;

    // Pre-declared grids come from each family's frozen registration
    // (spec: ≤ 20 distinct configs EVER). Unknown family names fall back to
    // the original 12-variant ema_rsi grid — historical behavior preserved.
    let mut jobs: Vec<(String, StrategySection)> =
        match crate::strategy::find_family(&base.strategy.name) {
            Some(f) => f
                .grid_jobs(&base)
                .into_iter()
                .map(|j| (j.label, j.strat))
                .collect(),
            None => {
                let mut v = crate::strategy::EmaRsiFamily
                    .grid_jobs(&base)
                    .into_iter()
                    .map(|j| (j.label, j.strat))
                    .collect::<Vec<_>>();
                v
            }
        };

    // Budget counts DISTINCT config hashes ever run (never resets). The
    // persisted counter is authoritative but never allowed to under-count vs
    // the table.
    let mut known = db.known_config_hashes().await?;
    let counter: u32 = db
        .config_get("variant_budget_used")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut used = counter.max(db.distinct_config_count().await?);

    if unlock_new_study {
        confirm_new_study()?;
    }

    #[allow(dead_code)] // reported fields kept for ranked output
    struct Row {
        label: String,
        pairs_passing_floor: usize,
        profitable_pairs: usize,
        worst_pf: f64,
        total_oos_trades: usize,
        pass: bool,
    }
    let mut rows: Vec<Row> = Vec::new();

    for (label, strat) in jobs {
        let hash = StrategyConfig {
            strategy: strat.clone(),
            backtest: base.backtest.clone(),
        }
        .config_hash();

        // refuse NEW distinct hashes past the cap; re-running a known one is free
        check_variant_budget(used, !known.contains(&hash), unlock_new_study)?;

        let results = evaluate_config(&db, &strat, &base.backtest, Some(&hash)).await?;
        if known.insert(hash) {
            used += 1; // charged atomically alongside that hash's inserted rows
        }
        let reports: Vec<PairReport> = results
            .into_iter()
            .map(|r| PairReport {
                symbol: r.symbol,
                metrics: r.oos_metrics,
            })
            .collect();
        let verdict = evaluate_gate(&reports);
        let passing_floor = reports
            .iter()
            .filter(|r| r.metrics.total_trades >= metrics::GATE_MIN_TRADES_PER_PAIR)
            .count();
        let profitable = reports.iter().filter(|r| r.metrics.net_pnl > 0.0).count();
        let worst_pf = reports
            .iter()
            .map(|r| r.metrics.profit_factor)
            .fold(f64::INFINITY, f64::min);

        println!(
            "{label} | floor:{passing_floor} prof:{profitable} worstPF:{:.2} trades:{} budget:{used}/{GATE_MAX_VARIANTS} | {}",
            if worst_pf.is_infinite() { f64::NAN } else { worst_pf },
            reports.iter().map(|r| r.metrics.total_trades).sum::<usize>(),
            if verdict.pass { "PASS" } else { "fail" },
        );

        rows.push(Row {
            label,
            pairs_passing_floor: passing_floor,
            profitable_pairs: profitable,
            worst_pf,
            total_oos_trades: reports.iter().map(|r| r.metrics.total_trades).sum(),
            pass: verdict.pass,
        });
    }

    rows.sort_by(|a, b| b.worst_pf.partial_cmp(&a.worst_pf).unwrap());
    println!("\n=== ranked by worst-pair OOS PF (budget now {used}/{GATE_MAX_VARIANTS}) ===");
    println!("RANKING IS ANALYSIS ONLY — NOT A GO SIGNAL.");
    println!("The pre-registered gate verdict is the sole go/no-go authority.");
    if let Some(best) = rows.first() {
        println!(
            "best: {} (worstPF {:.2}, {} profitable pairs)",
            best.label,
            if best.worst_pf.is_infinite() {
                f64::NAN
            } else {
                best.worst_pf
            },
            best.profitable_pairs
        );
    }
    let any_pass = rows.iter().any(|r| r.pass);
    println!(
        "\nOVERALL: {}",
        if any_pass {
            "at least one variant passed the pre-registered gate"
        } else {
            "NO variant passed the gate — per spec, do not go live"
        }
    );
    Ok(())
}

fn trades_csv(trades: &[TradeRow]) -> String {
    let mut s = String::from(
        "id,client_order_id,symbol,side,qty,entry_price,entry_ts,exit_price,exit_ts,\
         fee_paid,pnl,pnl_pct,strategy,mode\n",
    );
    for t in trades {
        let f = |v: Option<f64>| v.map(|x| x.to_string()).unwrap_or_default();
        let i = |v: Option<i64>| v.map(|x| x.to_string()).unwrap_or_default();
        s.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            t.id,
            t.client_order_id.as_deref().unwrap_or(""),
            t.symbol,
            t.side,
            t.qty,
            t.entry_price,
            t.entry_ts,
            f(t.exit_price),
            i(t.exit_ts),
            t.fee_paid,
            f(t.pnl),
            f(t.pnl_pct),
            t.strategy,
            t.mode,
        ));
    }
    s
}

async fn run_export(out: &str) -> anyhow::Result<()> {
    let db = Db::open_default().await?;
    let trades = db.load_trades().await?;
    std::fs::write(out, trades_csv(&trades))?;
    tracing::info!("exported {} trades to {out}", trades.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plateau_verdict_shapes() {
        assert_eq!(super::plateau_verdict(10.0, &[5.0, 3.0, -1.0]), "PLATEAU");
        assert_eq!(super::plateau_verdict(10.0, &[5.0, -3.0, -1.0]),
                   "SPIKE (neighbors do not confirm)");
        assert_eq!(super::plateau_verdict(-2.0, &[5.0, 3.0]), "BASE UNPROFITABLE");
        assert_eq!(super::plateau_verdict(1.0, &[]), "NO NEIGHBORS");
    }

    #[test]
    fn apply_param_covers_both_families() {
        use crate::strategy::StrategySection;
        let mk = || StrategySection {
            name: "zband_meanrev".into(),
            pairs: vec![],
            timeframe: "1h".into(),
            ema_fast: 50,
            ema_slow: 200,
            rsi_period: 14,
            rsi_entry_threshold: 35.0,
            atr_period: 14,
            atr_multiplier: 2.0,
            risk_reward_ratio: 1.5,
            lookback_bars: Some(96),
            z_entry: Some(3.0),
        };
        let mut m = mk();
        super::apply_param(&mut m, "lookback_bars", 0.8);
        assert_eq!(m.lookback_bars, Some(77)); // 96*0.8=76.8 -> round 77
        super::apply_param(&mut m, "z_entry", 1.2);
        assert!((m.z_entry.unwrap() - 3.6).abs() < 1e-9);
        super::apply_param(&mut m, "atr_multiplier", 1.2);
        assert!((m.atr_multiplier - 2.4).abs() < 1e-9);
        // legacy path with None band fields must not panic
        let mut legacy = mk();
        legacy.name = "ema_rsi_pullback".into();
        legacy.lookback_bars = None;
        legacy.z_entry = None;
        for (k, _) in super::family_params("ema_rsi_pullback") {
            super::apply_param(&mut legacy, k, 1.1);
        }
        assert!((legacy.rsi_entry_threshold - 38.5).abs() < 1e-9);
        assert_eq!(legacy.lookback_bars, None);
    }

    #[test]
    fn xorshift_deterministic_and_sample_distinct_exact() {
        let mut a = super::XorShift::new(42);
        let mut b = super::XorShift::new(42);
        for _ in 0..8 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut r = super::XorShift::new(7);
        let s1 = r.sample_distinct(100, 10);
        assert_eq!(s1.len(), 10);
        let uniq: std::collections::HashSet<_> = s1.iter().collect();
        assert_eq!(uniq.len(), 10, "sampled indices must be distinct");
        assert!(s1.iter().all(|&i| i < 100));
        assert_eq!(r.sample_distinct(5, 99).len(), 5, "k clamped to n");
    }

    #[test]
    fn fold_bounds_partitions_evenly_and_covers_span() {
        let b = super::fold_bounds(0, 300, 3);
        assert_eq!(b, vec![(0, 100), (100, 200), (200, 300)]);
        // remainder lands in the last window (width uses integer division)
        let b2 = super::fold_bounds(0, 310, 3);
        assert_eq!(b2, vec![(0, 103), (103, 206), (206, 310)]);
        assert_eq!(super::fold_bounds(5, 5, 3), vec![]);
    }

    #[test]
    fn variant_budget_lock_refuses_new_hash_without_unlock() {
        let err = check_variant_budget(12, true, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("reserved"), "{msg}");
        assert!(msg.contains("--unlock-new-study"), "{msg}");
        assert!(msg.contains("NEW-OOS"), "{msg}");
    }

    #[test]
    fn variant_budget_lock_allows_known_hash_without_unlock() {
        assert!(check_variant_budget(12, false, false).is_ok());
        assert!(check_variant_budget(GATE_MAX_VARIANTS, false, false).is_ok());
        assert!(check_variant_budget(u32::MAX, false, false).is_ok());
    }

    #[test]
    fn variant_budget_unlock_allows_new_hash_under_cap() {
        assert!(check_variant_budget(GATE_MAX_VARIANTS - 1, true, true).is_ok());
    }

    #[test]
    fn variant_budget_unlock_still_refuses_21st_distinct_hash() {
        let err = check_variant_budget(GATE_MAX_VARIANTS, true, true).unwrap_err();
        assert!(err.to_string().contains("budget"), "{err}");
    }

    #[test]
    fn new_study_unlock_requires_the_literal_phrase() {
        assert!(!confirm_new_study_input("new-oos\n"));
        assert!(!confirm_new_study_input("GO\n"));
        assert!(!confirm_new_study_input(""));
        assert!(!confirm_new_study_input("NEW-OOS extra\n"));
        assert!(confirm_new_study_input("NEW-OOS\n"));
        assert!(confirm_new_study_input("NEW-OOS"));
    }

    #[test]
    fn search_subcommand_parses_unlock_flag() {
        let cli = Cli::try_parse_from(["tp", "search", "--config", "c.toml"]).unwrap();
        match cli.command {
            Command::Search {
                config,
                unlock_new_study,
            } => {
                assert_eq!(config, "c.toml");
                assert!(!unlock_new_study);
            }
            _ => panic!("expected Search"),
        }
        let cli = Cli::try_parse_from(["tp", "search", "--unlock-new-study"]).unwrap();
        match cli.command {
            Command::Search {
                unlock_new_study, ..
            } => assert!(unlock_new_study),
            _ => panic!("expected Search"),
        }
        assert!(
            Cli::try_parse_from(["tp", "trade", "--unlock-new-study"]).is_err(),
            "unlock flag is search-only"
        );
    }

    #[test]
    fn trade_subcommand_parses_all_flags_with_testnet_default() {
        let cli = Cli::try_parse_from(["tp", "trade", "--config", "c.toml"]).unwrap();
        match cli.command {
            Command::Trade {
                config,
                testnet,
                live,
                dry_run,
                once,
            } => {
                assert_eq!(config, "c.toml");
                assert!(!testnet);
                assert!(!live);
                assert!(!dry_run);
                assert!(!once);
            }
            _ => panic!("expected Trade"),
        }
        let cli = Cli::try_parse_from([
            "tp",
            "trade",
            "--config",
            "c.toml",
            "--live",
            "--dry-run",
            "--once",
        ])
        .unwrap();
        match cli.command {
            Command::Trade { testnet, live, .. } => {
                assert!(!testnet);
                assert!(live);
            }
            _ => panic!("expected Trade"),
        }
    }

    #[test]
    fn flatten_subcommand_parses_flags() {
        let cli = Cli::try_parse_from(["tp", "flatten", "--live"]).unwrap();
        match cli.command {
            Command::Flatten { testnet, live, .. } => {
                assert!(!testnet);
                assert!(live);
            }
            _ => panic!("expected Flatten"),
        }
    }

    #[test]
    fn base_selection_defaults_to_testnet_and_rejects_conflicting_flags() {
        assert_eq!(select_base(false, false).unwrap(), TESTNET_BASE);
        assert_eq!(select_base(true, false).unwrap(), TESTNET_BASE);
        assert_eq!(select_base(false, true).unwrap(), BINANCE_BASE);
        assert!(select_base(true, true).is_err());
    }

    #[test]
    fn gate_banner_shows_no_go_on_fail_and_no_gate_result_when_empty() {
        assert!(gate_banner(None).contains("NO GATE RESULT"));
        let fail = gate_banner(Some(false));
        assert!(fail.contains("NO-GO"), "{fail}");
        assert!(fail.contains("PRE-REGISTERED GATE VERDICT"));
        assert!(!gate_banner(Some(true)).contains("NO-GO"));
    }

    #[test]
    fn trades_csv_writes_header_and_blank_exit_cells_for_open_trade() {
        use db::TradeRow;
        let closed_then_open = vec![
            TradeRow {
                id: 1,
                client_order_id: Some("tp-x-9".into()),
                symbol: "BTCUSDT".into(),
                side: "SELL".into(),
                qty: 0.01,
                entry_price: 40_000.0,
                entry_ts: 1_700_000_000_000,
                exit_price: Some(41_000.0),
                exit_ts: Some(1_700_003_600_000),
                fee_paid: 0.08,
                pnl: Some(1.0),
                pnl_pct: Some(0.25),
                strategy: "ema_rsi".into(),
                mode: "live".into(),
            },
            TradeRow {
                id: 2,
                client_order_id: None,
                symbol: "ETHUSDT".into(),
                side: "BUY".into(),
                qty: 0.5,
                entry_price: 2_000.0,
                entry_ts: 1_700_000_100_000,
                exit_price: None,
                exit_ts: None,
                fee_paid: 0.02,
                pnl: None,
                pnl_pct: None,
                strategy: "ema_rsi".into(),
                mode: "live".into(),
            },
        ];
        let csv = trades_csv(&closed_then_open);
        let mut lines = csv.lines();
        assert_eq!(
            lines.next(),
            Some(
                "id,client_order_id,symbol,side,qty,entry_price,entry_ts,exit_price,exit_ts,fee_paid,pnl,pnl_pct,strategy,mode"
            )
        );
        assert_eq!(
            lines.next(),
            Some("1,tp-x-9,BTCUSDT,SELL,0.01,40000,1700000000000,41000,1700003600000,0.08,1,0.25,ema_rsi,live")
        );
        assert_eq!(
            lines.next(),
            Some("2,,ETHUSDT,BUY,0.5,2000,1700000100000,,,0.02,,,ema_rsi,live")
        );
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn live_confirmation_requires_the_literal_word_go() {
        assert!(!confirm_live_input("no\n"));
        assert!(!confirm_live_input("go\n"));
        assert!(!confirm_live_input(""));
        assert!(!confirm_live_input("GO GO\n"));
        assert!(confirm_live_input("GO\n"));
        assert!(confirm_live_input("GO"));
    }

    #[test]
    fn live_trade_permitted_only_on_stored_pass() {
        assert!(!live_trade_permitted(None));
        assert!(!live_trade_permitted(Some(false)));
        assert!(live_trade_permitted(Some(true)));
    }

    #[test]
    fn refuse_live_trade_errors_on_fail_and_missing_without_needing_go() {
        for v in [None, Some(false)] {
            let err = refuse_live_trade(v).unwrap_err().to_string();
            assert!(
                err.contains("live trading refused until a stored overall gate PASS exists"),
                "{err}"
            );
            assert!(err.contains(&gate_banner(v)), "{err}");
            assert!(!err.to_lowercase().contains("api_key"), "{err}");
            assert!(!err.to_lowercase().contains("secret"), "{err}");
        }
    }

    #[test]
    fn refuse_live_trade_ok_on_pass_so_go_prompt_can_run() {
        assert!(refuse_live_trade(Some(true)).is_ok());
    }

    async fn memory_db() -> Db {
        Db::open("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn latest_verdict_is_none_when_table_empty_then_reflects_newest_run() {
        let db = memory_db().await;
        assert_eq!(db.latest_search_overall_verdict().await.unwrap(), None);

        let fail_row = BacktestRunRow {
            symbol: "BTCUSDT".into(),
            rsi_entry: 30.0,
            atr_mult: 2.0,
            rr: 2.0,
            lookback_bars: None,
            z_entry: None,
            oos_start_ts: None,
            oos_end_ts: None,
            oos_trades: 25,
            oos_pf: 0.9,
            oos_pnl: -50.0,
            oos_dd: 12.0,
        };
        db.record_backtest_results("fail-hash", &[fail_row.clone()])
            .await
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.record_backtest_results("fail-hash-2", &[fail_row])
            .await
            .unwrap();
        assert_eq!(
            db.latest_search_overall_verdict().await.unwrap(),
            Some(false)
        );

        let pass_rows: Vec<BacktestRunRow> = ["ETHUSDT", "SOLUSDT", "ADAUSDT"]
            .iter()
            .map(|s| BacktestRunRow {
                symbol: s.to_string(),
                rsi_entry: 30.0,
                atr_mult: 2.0,
                rr: 2.0,
                lookback_bars: None,
                z_entry: None,
                oos_start_ts: None,
                oos_end_ts: None,
                oos_trades: 25,
                oos_pf: 2.0,
                oos_pnl: 10.0,
                oos_dd: 5.0,
            })
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.record_backtest_results("pass-hash", &pass_rows)
            .await
            .unwrap();
        assert_eq!(
            db.latest_search_overall_verdict().await.unwrap(),
            Some(true)
        );
    }
}

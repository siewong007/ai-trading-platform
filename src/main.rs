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
use strategy::{BacktestSection, StrategyConfig, StrategySection};
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
    },
    /// Backtest one strategy config; print IS/OOS report + gate verdict
    Backtest {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
    },
    /// Run the pre-declared variant grid within the 20-config budget
    Search {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
        /// Allow new config hashes this run (requires typing NEW-OOS)
        #[arg(long)]
        unlock_new_study: bool,
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
        Command::Fetch { config } => rt.block_on(run_fetch(&config))?,
        Command::Backtest { config } => rt.block_on(run_backtest(&config))?,
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

async fn run_fetch(config_path: &str) -> anyhow::Result<()> {
    let cfg = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;
    let ex = Exchange::new(BINANCE_BASE)?;
    let start = chrono::Utc::now().timestamp_millis() - LOOKBACK_DAYS * 86_400_000;
    for pair in &cfg.strategy.pairs {
        let ks = ex
            .fetch_klines(pair, &cfg.strategy.timeframe, start)
            .await?;
        let span_days = if ks.len() > 1 {
            (ks.last().unwrap().open_time - ks.first().unwrap().open_time) / 86_400_000
        } else {
            0
        };
        db.upsert_klines(pair, &cfg.strategy.timeframe, &ks).await?;
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
                oos_trades: oos_metrics.total_trades as i64,
                oos_pf: oos_metrics.profit_factor,
                oos_pnl: oos_metrics.net_pnl,
                oos_dd: oos_metrics.max_drawdown_pct,
            });
        }

        results.push(PairResult {
            symbol: pair.clone(),
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

async fn run_backtest(config_path: &str) -> anyhow::Result<()> {
    let cfg = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;
    let results = evaluate_config(&db, &cfg.strategy, &cfg.backtest, None).await?;
    print_report(&results);
    Ok(())
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

async fn run_search(config_path: &str, unlock_new_study: bool) -> anyhow::Result<()> {
    let base = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;

    // Pre-declared grids per family (spec: ≤ 20 distinct configs EVER).
    // zband grid is FROZEN 2026-08-25 — never edited after seeing results.
    let mut jobs: Vec<(String, StrategySection)> = Vec::new();
    if base.strategy.name == "zband_meanrev" {
        for (lb, z) in zband_grid() {
            let mut s = base.strategy.clone();
            s.lookback_bars = Some(lb);
            s.z_entry = Some(z);
            jobs.push((format!("lb={lb:>3} z={z}"), s));
        }
    } else {
        // legacy ema_rsi_pullback grid: 2 x 3 x 2 = 12 variants (spec)
        for rsi_e in [30.0, 35.0] {
            for atr_m in [1.5, 2.0, 2.5] {
                for rr in [1.5, 2.0] {
                    let mut s = base.strategy.clone();
                    s.rsi_entry_threshold = rsi_e;
                    s.atr_multiplier = atr_m;
                    s.risk_reward_ratio = rr;
                    jobs.push((format!("rsi={rsi_e:>4} atr={atr_m:>3} rr={rr:>3}"), s));
                }
            }
        }
    }

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
    fn zband_grid_matches_frozen_spec_exactly() {
        let g = zband_grid();
        assert_eq!(
            g,
            [(24, 2.0), (24, 2.5), (48, 2.0), (48, 2.5), (96, 2.0), (96, 2.5)]
        );
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

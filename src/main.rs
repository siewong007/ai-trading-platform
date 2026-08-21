mod backtest;
mod db;
mod exchange;
mod indicators;
mod metrics;
mod strategy;
mod types;

use backtest::{run, BacktestOutput};
use clap::{Parser, Subcommand};
use db::{BacktestRunRow, Db};
use exchange::Exchange;
use metrics::{
    evaluate_gate, max_drawdown_pct, GateVerdict, Metrics, PairReport, GATE_MAX_VARIANTS,
};
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
    },
    /// Export trades table to CSV (tax records)
    Export {
        #[arg(long, default_value = "data/trades_export.csv")]
        out: String,
    },
}

const BINANCE_BASE: &str = "https://api.binance.com";
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
        Command::Search { config } => rt.block_on(run_search(&config))?,
        Command::Export { out } => rt.block_on(run_export(&out))?,
    }
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

/// Pre-registered rule (spec §5): at most GATE_MAX_VARIANTS DISTINCT configs
/// may EVER run. Re-running an already-known hash is free; a NEW hash that
/// would push the distinct total past the cap is refused BEFORE any work.
fn check_variant_budget(used_distinct: u32, is_new_hash: bool) -> anyhow::Result<()> {
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

async fn run_search(config_path: &str) -> anyhow::Result<()> {
    let base = StrategyConfig::load(config_path)?;
    let db = Db::open_default().await?;

    // Pre-declared grid: 2 x 3 x 2 = 12 variants (spec: ≤ 20 distinct ever)
    let mut variants: Vec<(f64, f64, f64)> = Vec::new(); // (rsi_entry, atr_mult, rr)
    for rsi_e in [30.0, 35.0] {
        for atr_m in [1.5, 2.0, 2.5] {
            for rr in [1.5, 2.0] {
                variants.push((rsi_e, atr_m, rr));
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

    #[allow(dead_code)] // reported fields kept for ranked output
    struct Row {
        rsi: f64,
        atr: f64,
        rr: f64,
        pairs_passing_floor: usize,
        profitable_pairs: usize,
        worst_pf: f64,
        total_oos_trades: usize,
        pass: bool,
    }
    let mut rows: Vec<Row> = Vec::new();

    for (rsi_e, atr_m, rr) in variants {
        let mut strat = base.strategy.clone();
        strat.rsi_entry_threshold = rsi_e;
        strat.atr_multiplier = atr_m;
        strat.risk_reward_ratio = rr;
        let hash = StrategyConfig {
            strategy: strat.clone(),
            backtest: base.backtest.clone(),
        }
        .config_hash();

        // refuse NEW distinct hashes past the cap; re-running a known one is free
        check_variant_budget(used, !known.contains(&hash))?;

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
            "rsi={rsi_e:>4} atr={atr_m:>3} rr={rr:>3} | floor:{} prof:{profitable} worstPF:{:.2} trades:{} budget:{used}/{GATE_MAX_VARIANTS} | {}",
            passing_floor,
            if worst_pf.is_infinite() { f64::NAN } else { worst_pf },
            reports.iter().map(|r| r.metrics.total_trades).sum::<usize>(),
            if verdict.pass { "PASS" } else { "fail" },
        );

        rows.push(Row {
            rsi: rsi_e,
            atr: atr_m,
            rr,
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
            "best: rsi={} atr={} rr={} (worstPF {:.2}, {} profitable pairs)",
            best.rsi,
            best.atr,
            best.rr,
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

async fn run_export(_out: &str) -> anyhow::Result<()> {
    anyhow::bail!("export arrives with live trading (Phase 2): no fills exist yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_budget_refuses_21st_distinct_hash_but_reruns_are_free() {
        assert!(check_variant_budget(GATE_MAX_VARIANTS - 1, true).is_ok());
        // re-running an already-known hash is free, even at the cap
        assert!(check_variant_budget(GATE_MAX_VARIANTS, false).is_ok());
        assert!(check_variant_budget(u32::MAX, false).is_ok());
        // the 21st DISTINCT hash is refused before any work happens
        let err = check_variant_budget(GATE_MAX_VARIANTS, true).unwrap_err();
        assert!(err.to_string().contains("budget"), "{err}");
    }
}

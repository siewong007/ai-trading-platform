mod types;
mod indicators;
mod strategy;
mod db;
mod exchange;
mod backtest;
mod metrics;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "trading_platform", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download and cache klines for the configured pairs
    Fetch {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
    },
    /// Backtest one strategy config, print gate report
    Backtest {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
    },
    /// Run pre-declared variant grid within the 20-config budget
    Search {
        #[arg(long, default_value = "config/strategy_ema_rsi.toml")]
        config: String,
    },
}

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;
    match cli.command {
        Command::Fetch { config } => rt.block_on(async { run_fetch(&config).await }),
        Command::Backtest { config } => rt.block_on(async { run_backtest(&config).await }),
        Command::Search { config } => rt.block_on(async { run_search(&config).await }),
    }
}

async fn run_fetch(_config: &str) -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}
async fn run_backtest(_config: &str) -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}
async fn run_search(_config: &str) -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}

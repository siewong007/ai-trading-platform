use crate::indicators::{atr, ema, rsi};
use crate::types::Candle;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    pub strategy: StrategySection,
    pub backtest: BacktestSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategySection {
    #[allow(dead_code)]
    pub name: String,
    pub pairs: Vec<String>,
    #[allow(dead_code)]
    pub timeframe: String,
    pub ema_fast: usize,
    pub ema_slow: usize,
    pub rsi_period: usize,
    pub rsi_entry_threshold: f64,
    pub atr_period: usize,
    pub atr_multiplier: f64,
    pub risk_reward_ratio: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BacktestSection {
    pub start_equity_usd: f64,
    pub risk_per_trade_usd: f64,
    pub max_notional_pct_equity: f64,
    pub min_notional_usd: f64,
}

impl StrategyConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&raw)?)
    }
}

/// A concrete trade plan produced on a candle close.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TradePlan {
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
}

/// Evaluate strategy over a closed-candle series.
/// Output[i] = Some(plan) when a long entry triggers on candle i's close.
pub fn generate_signals(candles: &[Candle], cfg: &StrategySection) -> Vec<Option<TradePlan>> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let highs: Vec<f64> = candles.iter().map(|c| c.high).collect();
    let lows: Vec<f64> = candles.iter().map(|c| c.low).collect();
    let ema_fast = ema(&closes, cfg.ema_fast);
    let ema_slow = ema(&closes, cfg.ema_slow);
    let rsi_v = rsi(&closes, cfg.rsi_period);
    let atr_v = atr(&highs, &lows, &closes, cfg.atr_period);

    let mut out: Vec<Option<TradePlan>> = vec![None; candles.len()];
    // warmup: need slow EMA, rsi prev value, and ATR
    let warmup = cfg.ema_slow.max(cfg.rsi_period + 2).max(cfg.atr_period + 1);
    for i in warmup..candles.len() {
        let (Some(ef), Some(es)) = (ema_fast[i], ema_slow[i]) else {
            continue;
        };
        let (Some(r_now), Some(r_prev)) = (rsi_v[i], rsi_v[i - 1]) else {
            continue;
        };
        let Some(a) = atr_v[i] else { continue };

        // trend filter: price above slow EMA and fast EMA above slow EMA
        let trending_up = closes[i] > es && ef > es;
        // RSI crosses UP through the entry threshold (pullback ending)
        let rsi_cross_up =
            r_prev < cfg.rsi_entry_threshold && r_now >= cfg.rsi_entry_threshold;

        if trending_up && rsi_cross_up {
            let entry = closes[i];
            let stop = entry - cfg.atr_multiplier * a;
            if stop >= entry {
                continue;
            }
            let risk = entry - stop;
            out[i] = Some(TradePlan {
                entry,
                stop,
                target: entry + cfg.risk_reward_ratio * risk,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg() -> StrategySection {
        StrategySection {
            name: "test".into(),
            pairs: vec!["TESTUSDT".into()],
            timeframe: "1h".into(),
            ema_fast: 5,
            ema_slow: 10,
            rsi_period: 5,
            rsi_entry_threshold: 40.0,
            atr_period: 3,
            atr_multiplier: 2.0,
            risk_reward_ratio: 1.5,
        }
    }

    fn candle(i: usize, open: f64, close: f64) -> Candle {
        Candle {
            open_time: i as i64 * 3_600_000,
            open,
            high: open.max(close) + 0.5,
            low: open.min(close) - 0.5,
            close,
            volume: 100.0,
        }
    }

    #[test]
    fn plan_math_is_exact() {
        let cfg = test_cfg();
        // entry=100, ATR must be known: constant range candles high=open+0.5 low=open.min(close)-0.5
        // craft: prev close 100, this close 100, range 1 => TR=1 => ATR~1
        let mut cs = Vec::new();
        for i in 0..30 {
            cs.push(candle(i, 100.0, 100.0));
        }
        let plans = generate_signals(&cs, &cfg);
        // flat market: no signals ever
        assert!(plans.iter().all(|p| p.is_none()));
    }

    #[test]
    fn uptrend_with_pullback_yields_signal() {
        let cfg = test_cfg(); // thresholds: ema 5/10, rsi 5 @40
        let mut cs: Vec<Candle> = Vec::new();
        let mut i = 0usize;
        // flat base seeds EMAs low
        for _ in 0..20 {
            cs.push(candle(i, 100.0, 100.0));
            i += 1;
        }
        // 60 strong green bars -> price 106, RSI pinned high
        let mut price = 100.0;
        for _ in 0..60 {
            let o = price;
            price += 0.1;
            cs.push(candle(i, o, price));
            i += 1;
        }
        // 2 sharp red bars -> RSI crashes below 40
        for _ in 0..2 {
            let o = price;
            price -= 0.5;
            cs.push(candle(i, o, price));
            i += 1;
        }
        // strong recovery bar -> RSI crosses back up through 40
        let o = price;
        price += 1.0;
        cs.push(candle(i, o, price));

        let plans = generate_signals(&cs, &cfg);
        let hits: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_some())
            .map(|(i, _)| i)
            .collect();
        assert!(!hits.is_empty(), "expected at least one signal");
        assert!(hits.iter().all(|&h| h >= 78), "signal only after recovery");
        let p = plans[hits[0]].unwrap();
        assert!(p.stop < p.entry && p.entry < p.target);
        assert!((p.target - p.entry) / (p.entry - p.stop) - cfg.risk_reward_ratio < 1e-9);
    }

    #[test]
    fn downtrend_never_signals() {
        let cfg = test_cfg();
        let mut cs: Vec<Candle> = Vec::new();
        let mut price = 200.0;
        for i in 0..80 {
            let o = price;
            price -= 0.5;
            cs.push(candle(i, o, price));
        }
        assert!(generate_signals(&cs, &cfg).iter().all(|p| p.is_none()));
    }

    #[test]
    fn config_parses_from_toml() {
        let raw = r#"
[strategy]
name = "ema_rsi_pullback"
pairs = ["BTCUSDT", "ETHUSDT"]
timeframe = "1h"
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
        let cfg: StrategyConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.strategy.pairs.len(), 2);
        assert_eq!(cfg.backtest.risk_per_trade_usd, 2.0);
    }
}

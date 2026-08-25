use crate::indicators::{atr, ema, rolling_stats, rsi};
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
    #[serde(default)]
    pub lookback_bars: Option<usize>,
    #[serde(default)]
    pub z_entry: Option<f64>,
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
        Self::load_from_toml_str(&raw)
    }

    pub fn load_from_toml_str(raw: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(raw)?)
    }

    /// Content hash over EVERY effective parameter (signals + sizing) in a
    /// canonical sorted key=value form: identical configs always produce an
    /// identical hash and changing ANY field changes it. This is the unit the
    /// pre-registered variant budget charges against.
    pub fn config_hash(&self) -> String {
        use std::hash::{Hash, Hasher};
        let s = &self.strategy;
        let b = &self.backtest;
        let fx = |v: f64| format!("{:016x}", v.to_bits()); // bit-exact, stable
        let mut fields: Vec<(String, String)> = vec![
            ("atr_multiplier".into(), fx(s.atr_multiplier)),
            ("atr_period".into(), s.atr_period.to_string()),
            ("ema_fast".into(), s.ema_fast.to_string()),
            ("ema_slow".into(), s.ema_slow.to_string()),
            (
                "max_notional_pct_equity".into(),
                fx(b.max_notional_pct_equity),
            ),
            ("min_notional_usd".into(), fx(b.min_notional_usd)),
            ("pairs".into(), s.pairs.join(",")),
            ("risk_per_trade_usd".into(), fx(b.risk_per_trade_usd)),
            ("risk_reward_ratio".into(), fx(s.risk_reward_ratio)),
            ("rsi_entry_threshold".into(), fx(s.rsi_entry_threshold)),
            ("rsi_period".into(), s.rsi_period.to_string()),
            ("start_equity_usd".into(), fx(b.start_equity_usd)),
            ("strategy_name".into(), s.name.clone()),
            ("timeframe".into(), s.timeframe.clone()),
        ];
        // Family-specific params join the hash ONLY when set (Some): unset
        // (None) must NOT perturb legacy ema_rsi_pullback hashes or the
        // persisted variant-budget accounting would see them as new configs.
        if let Some(v) = s.lookback_bars {
            fields.push(("lookback_bars".into(), v.to_string()));
        }
        if let Some(v) = s.z_entry {
            fields.push(("z_entry".into(), fx(v)));
        }
        fields.sort(); // canonical ordering independent of TOML key order
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (k, v) in &fields {
            format!("{k}={v};").hash(&mut h);
        }
        format!("{:016x}", h.finish())
    }
}

/// A concrete trade plan produced on a candle close.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TradePlan {
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
}

/// One pre-declared variant produced by a family's frozen grid.
#[derive(Debug, Clone)]
pub struct FamilyGridJob {
    pub label: String,
    pub strat: StrategySection,
}

/// A signal family: its indicator engine plus its FROZEN pre-declared grid.
/// Adding a future family means implementing this trait and registering it —
/// never editing an existing family's grid after results are seen.
pub trait SignalFamily {
    fn name(&self) -> &'static str;
    fn signals(&self, cfg: &StrategySection, candles: &[Candle])
        -> Vec<Option<TradePlan>>;
    fn grid_jobs(&self, base: &StrategyConfig) -> Vec<FamilyGridJob>;
}

pub struct EmaRsiFamily;
pub struct ZbandFamily;

impl SignalFamily for EmaRsiFamily {
    fn name(&self) -> &'static str {
        "ema_rsi_pullback"
    }
    fn signals(
        &self,
        cfg: &StrategySection,
        candles: &[Candle],
    ) -> Vec<Option<TradePlan>> {
        generate_ema_rsi_signals(candles, cfg)
    }
    /// Pre-declared grid: 2 x 3 x 2 = 12 variants (spec: ≤ 20 distinct ever)
    fn grid_jobs(&self, base: &StrategyConfig) -> Vec<FamilyGridJob> {
        let mut v = Vec::new();
        for rsi_e in [30.0, 35.0] {
            for atr_m in [1.5, 2.0, 2.5] {
                for rr in [1.5, 2.0] {
                    let mut s = base.strategy.clone();
                    s.rsi_entry_threshold = rsi_e;
                    s.atr_multiplier = atr_m;
                    s.risk_reward_ratio = rr;
                    v.push(FamilyGridJob {
                        label: format!("rsi={rsi_e:>4} atr={atr_m:>3} rr={rr:>3}"),
                        strat: s,
                    });
                }
            }
        }
        v
    }
}

impl SignalFamily for ZbandFamily {
    fn name(&self) -> &'static str {
        "zband_meanrev"
    }
    fn signals(
        &self,
        cfg: &StrategySection,
        candles: &[Candle],
    ) -> Vec<Option<TradePlan>> {
        generate_zband_signals(candles, cfg)
    }
    /// Frozen 2026-08-25 (spec §Pre-declared grid): 6 of the 8 reserved slots.
    /// Never edited after seeing any result.
    fn grid_jobs(&self, base: &StrategyConfig) -> Vec<FamilyGridJob> {
        const FROZEN: [(usize, f64); 6] = [
            (24, 2.0),
            (24, 2.5),
            (48, 2.0),
            (48, 2.5),
            (96, 2.0),
            (96, 2.5),
        ];
        FROZEN
            .iter()
            .map(|&(lb, z)| {
                let mut s = base.strategy.clone();
                s.lookback_bars = Some(lb);
                s.z_entry = Some(z);
                FamilyGridJob {
                    label: format!("lb={lb:>3} z={z}"),
                    strat: s,
                }
            })
            .collect()
    }
}

/// Registry lookup; None for unknown family names.
pub fn find_family(name: &str) -> Option<&'static dyn SignalFamily> {
    const REGISTRY: &[&dyn SignalFamily] = &[&EmaRsiFamily, &ZbandFamily];
    REGISTRY.iter().copied().find(|f| f.name() == name)
}

/// Evaluate strategy over a closed-candle series.
/// Output[i] = Some(plan) when a long entry triggers on candle i's close.
/// Family dispatcher: strategy TOML selects the signal engine by name;
/// unknown names fall back to the original EMA/RSI engine.
pub fn generate_signals(candles: &[Candle], cfg: &StrategySection) -> Vec<Option<TradePlan>> {
    match find_family(&cfg.name) {
        Some(f) => f.signals(cfg, candles),
        None => generate_ema_rsi_signals(candles, cfg),
    }
}

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

    // warmup: full stats window plus enough ATR history for the veto median
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

fn generate_ema_rsi_signals(
    candles: &[Candle],
    cfg: &StrategySection,
) -> Vec<Option<TradePlan>> {
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
        let rsi_cross_up = r_prev < cfg.rsi_entry_threshold && r_now >= cfg.rsi_entry_threshold;

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
            lookback_bars: None,
            z_entry: None,
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

    const HASH_TOML_A: &str = r#"
[strategy]
name = "h"
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

    // identical VALUES, keys/sections in different order -> must hash equal
    const HASH_TOML_B: &str = r#"
[backtest]
min_notional_usd = 15.0
max_notional_pct_equity = 0.5
risk_per_trade_usd = 2.0
start_equity_usd = 200.0

[strategy]
risk_reward_ratio = 1.5
atr_multiplier = 2.0
atr_period = 14
rsi_entry_threshold = 35.0
rsi_period = 14
ema_slow = 200
ema_fast = 50
timeframe = "1h"
pairs = ["BTCUSDT", "ETHUSDT"]
name = "h"
"#;

    #[test]
    fn config_hash_stable_across_identical_configs() {
        let a: StrategyConfig = toml::from_str(HASH_TOML_A).unwrap();
        let b: StrategyConfig = toml::from_str(HASH_TOML_B).unwrap();
        assert_eq!(a.config_hash(), b.config_hash());
        let again: StrategyConfig = toml::from_str(HASH_TOML_A).unwrap();
        assert_eq!(a.config_hash(), again.config_hash());
    }

    #[test]
    fn config_hash_changes_when_signal_or_sizing_param_changes() {
        let a: StrategyConfig = toml::from_str(HASH_TOML_A).unwrap();
        let mut variants: Vec<(&str, StrategyConfig)> = Vec::new();
        let mut m = a.clone();
        m.strategy.rsi_entry_threshold = 36.0;
        variants.push(("rsi_entry_threshold", m));
        let mut m = a.clone();
        m.strategy.ema_fast = 49;
        variants.push(("ema_fast", m));
        let mut m = a.clone();
        m.strategy.atr_multiplier = 2.5;
        variants.push(("atr_multiplier", m));
        let mut m = a.clone();
        m.backtest.risk_per_trade_usd = 3.0;
        variants.push(("risk_per_trade_usd", m));
        let mut m = a.clone();
        m.backtest.min_notional_usd = 20.0;
        variants.push(("min_notional_usd", m));
        for (name, cfg) in variants {
            assert_ne!(
                a.config_hash(),
                cfg.config_hash(),
                "{name} must change the hash"
            );
        }
    }

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

    fn zband_cfg() -> StrategySection {
        StrategySection {
            lookback_bars: Some(48),
            z_entry: Some(2.0),
            ..test_cfg()
        }
    }

    #[test]
    fn zband_fires_on_gradual_dip_via_dispatch() {
        // NOTE (why this shape): a pure linear ramp never exceeds z ≈ -√3, and
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
        assert!(hits.iter().all(|&h| h >= 120), "fires only during the slide");
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
    fn zband_registry_grid_is_frozen_exactly() {
        let base: StrategyConfig = toml::from_str(HASH_TOML_ZB).unwrap();
        let jobs = find_family("zband_meanrev").unwrap().grid_jobs(&base);
        let got: Vec<(usize, f64)> = jobs
            .iter()
            .map(|j| (j.strat.lookback_bars.unwrap(), j.strat.z_entry.unwrap()))
            .collect();
        assert_eq!(
            got,
            [(24, 2.0), (24, 2.5), (48, 2.0), (48, 2.5), (96, 2.0), (96, 2.5)]
        );
        assert!(jobs.iter().all(|j| j.label.starts_with("lb=")));
    }

    #[test]
    fn legacy_grid_and_fallback_are_unchanged() {
        let base: StrategyConfig = toml::from_str(HASH_TOML_A).unwrap();
        // registered ema_rsi family
        assert_eq!(
            find_family("ema_rsi_pullback").unwrap().grid_jobs(&base).len(),
            12
        );
        // unknown family name falls back to the 12-variant grid
        let mut unknown = base.clone();
        unknown.strategy.name = "mystery".into();
        assert!(find_family("mystery").is_none());
        let fb = crate::strategy::EmaRsiFamily.grid_jobs(&unknown);
        assert_eq!(fb.len(), 12);
        assert!(fb[0].label.trim().starts_with("rsi="));
    }

    #[test]
    fn option_median_even_length_averages_middle() {
        let v = vec![Some(1.0), Some(3.0), None, Some(5.0), Some(7.0)];
        assert!(option_median(&v).is_some());
        // sorted non-None: 1,3,5,7 -> median 4.0
        assert!((option_median(&v).unwrap() - 4.0).abs() < 1e-9);
        assert_eq!(option_median(&[None, None]), None);
    }
}

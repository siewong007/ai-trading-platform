use crate::strategy::{generate_signals, BacktestSection, StrategySection};
use crate::types::Candle;

/// Hard cost model from spec §5 — shared by every backtest, never relaxed.
pub const TAKER_FEE_RATE: f64 = 0.001; // per side
pub const SLIPPAGE_RATE: f64 = 0.0005; // per side

#[derive(Debug, Clone)]
#[allow(dead_code)] // consumed by CSV export + executor in Phase 2
pub struct TradeRecord {
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub qty: f64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub pnl: f64,
    /// net PnL as a fraction of entry notional (fees + slippage included)
    pub pnl_pct: f64,
    pub exit_reason: ExitReason,
    /// best close-based excursion while held, in R multiples (risk units)
    pub mfe_r: f64,
    /// worst close-based excursion while held, in R multiples
    pub mae_r: f64,
    /// candles held including entry and exit bars
    pub bars_held: i64,
}

/// Aggregate trade-level attribution for one simulation.
#[derive(Debug, Clone)]
pub struct Attribution {
    pub avg_mfe_r: f64,
    pub avg_mae_r: f64,
    pub median_bars_held: f64,
    /// 4h UTC bucket (0..=5) with the lowest summed pnl, and its sum
    pub worst_bucket: (u8, f64),
    /// summed pnl by 4h UTC bucket (session-conditioned lead hunting)
    pub buckets: [f64; 6],
}

/// Summarize attribution over simulated trades; None when empty.
pub fn summarize_attribution(trades: &[TradeRecord]) -> Option<Attribution> {
    if trades.is_empty() {
        return None;
    }
    let n = trades.len() as f64;
    let avg_mfe_r = trades.iter().map(|t| t.mfe_r).sum::<f64>() / n;
    let avg_mae_r = trades.iter().map(|t| t.mae_r).sum::<f64>() / n;
    let mut bars: Vec<i64> = trades.iter().map(|t| t.bars_held).collect();
    bars.sort_unstable();
    let median_bars_held = bars[bars.len() / 2] as f64;
    let mut buckets = [0.0f64; 6];
    for t in trades {
        let hour = (t.entry_ts / 3_600_000).rem_euclid(24);
        buckets[(hour / 4) as usize] += t.pnl;
    }
    let (bi, bv) = buckets
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap();
    Some(Attribution {
        avg_mfe_r,
        avg_mae_r,
        median_bars_held,
        worst_bucket: (bi as u8, *bv),
        buckets,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExitReason {
    Stop,
    Target,
}

#[derive(Debug, Clone, Copy)]
pub struct EquityPoint {
    pub ts: i64,
    pub equity: f64,
}

struct Position {
    qty: f64,
    entry_price: f64,
    stop: f64,
    target: f64,
    entry_idx: usize,
    entry_fee: f64,
    mfe_r: f64,
    mae_r: f64,
    bars_held: i64,
}

#[allow(dead_code)]
pub struct BacktestOutput {
    pub trades: Vec<TradeRecord>,
    pub equity_curve: Vec<EquityPoint>,
    pub final_equity: f64,
}

fn position_sizing(
    plan_fill_price: f64,
    stop: f64,
    equity: f64,
    bt: &BacktestSection,
) -> Option<f64> {
    let risk_dist = plan_fill_price - stop;
    if risk_dist <= 0.0 {
        return None;
    }
    // fixed-dollar risk sizing
    let mut qty = bt.risk_per_trade_usd / risk_dist;
    // notional cap ≤ max_notional_pct of current equity
    let cap_qty = (bt.max_notional_pct_equity * equity) / plan_fill_price;
    qty = qty.min(cap_qty);
    let notional = qty * plan_fill_price;
    if notional < bt.min_notional_usd {
        return None; // below exchange floor + buffer -> skip trade
    }
    Some(qty)
}

/// Run a long-only backtest over one symbol's candles.
/// Fill model: signal on candle i close -> enter at candle i+1 open x (1+slip).
/// Exits checked from the entry bar onward; when stop and target are both
/// touched inside one candle the STOP is assumed hit first (conservative).
pub fn run(candles: &[Candle], strat: &StrategySection, bt: &BacktestSection) -> BacktestOutput {
    let signals = generate_signals(candles, strat);
    run_with_signals(candles, strat, bt, signals)
}

/// Same simulation with caller-supplied signals — enables attribution and
/// permutation studies without re-running indicator math.
pub fn run_with_signals(
    candles: &[Candle],
    _strat: &StrategySection,
    bt: &BacktestSection,
    signals: Vec<Option<crate::strategy::TradePlan>>,
) -> BacktestOutput {
    let mut trades = Vec::new();
    let mut equity_curve = Vec::new();
    let mut equity = bt.start_equity_usd;
    let mut pos: Option<Position> = None;

    for i in 0..candles.len() {
        let c = candles[i];

        if let Some(mut p) = pos.take_if_in_place() {
            // close-based excursion tracking in R multiples (risk = entry-stop)
            let risk_dist = p.entry_price - p.stop;
            if risk_dist > 0.0 {
                let r = (c.close - p.entry_price) / risk_dist;
                pos_exc(&mut p, r);
            }
            // exit checks on this candle (entry bar included -> gap handling)
            let exit: Option<(f64, ExitReason)> = if c.low <= p.stop {
                Some((p.stop * (1.0 - SLIPPAGE_RATE), ExitReason::Stop))
            } else if c.high >= p.target {
                Some((p.target * (1.0 - SLIPPAGE_RATE), ExitReason::Target))
            } else {
                None
            };
            if let Some((exit_price, reason)) = exit {
                let exit_fee = exit_price * p.qty * TAKER_FEE_RATE;
                let pnl = (exit_price - p.entry_price) * p.qty - p.entry_fee - exit_fee;
                equity += pnl;
                trades.push(TradeRecord {
                    entry_ts: candles[p.entry_idx].open_time,
                    exit_ts: c.open_time,
                    qty: p.qty,
                    entry_price: p.entry_price,
                    exit_price,
                    pnl,
                    // net of both sides' fees AND slippage: pnl already is,
                    // and the denominator is the gross entry notional
                    pnl_pct: pnl / (p.entry_price * p.qty),
                    exit_reason: reason,
                    mfe_r: p.mfe_r,
                    mae_r: p.mae_r,
                    bars_held: p.bars_held,
                });
            } else {
                pos = Some(p);
            }
        }

        // new entry: signal on THIS candle close fills at NEXT candle open
        if pos.is_none() && i + 1 < candles.len() {
            if let Some(plan) = signals[i] {
                let fill = candles[i + 1].open * (1.0 + SLIPPAGE_RATE);
                if let Some(qty) = position_sizing(fill, plan.stop, equity, bt) {
                    let entry_fee = fill * qty * TAKER_FEE_RATE;
                    pos = Some(Position {
                        qty,
                        entry_price: fill,
                        stop: plan.stop,
                        target: plan.target,
                        entry_idx: i + 1,
                        entry_fee,
                        mfe_r: 0.0,
                        mae_r: 0.0,
                        // entry bar itself is counted when its iteration
                        // runs pos_exc below — do not pre-count here
                        bars_held: 0,
                    });
                }
            }
        }

        // mark-to-market equity for drawdown measurement
        let mtm = match &pos {
            Some(p) => equity + (c.close - p.entry_price) * p.qty,
            None => equity,
        };
        equity_curve.push(EquityPoint {
            ts: c.open_time,
            equity: mtm,
        });
    }

    BacktestOutput {
        trades,
        final_equity: equity,
        equity_curve,
    }
}

/// Fold a close-based R excursion into running MFE/MAE and advance hold count.
fn pos_exc(p: &mut Position, r: f64) {
    if r > p.mfe_r {
        p.mfe_r = r;
    }
    if r < p.mae_r {
        p.mae_r = r;
    }
    p.bars_held += 1;
}

// tiny helper to allow take() with condition without cloning
trait TakeIf {
    fn take_if_in_place(&mut self) -> Option<Position>;
}
impl TakeIf for Option<Position> {
    fn take_if_in_place(&mut self) -> Option<Position> {
        self.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::StrategyConfig;

    #[test]
    fn attribution_tracked_in_r_multiples() {
        // entry fills at 100 (slippage ~0 on flat opens), stop 95 => R=5.
        // candles after entry bar: close 108 -> MFE +1.6R; close 96 -> MAE -0.8R;
        // exit at stop on the third held candle.
        let mut cs = Vec::new();
        let mk = |i: usize, o: f64, h: f64, l: f64, c: f64| crate::types::Candle {
            open_time: i as i64 * 3_600_000,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
        };
        cs.push(mk(0, 99.0, 99.5, 98.5, 99.0)); // signal bar
        cs.push(mk(1, 100.0, 100.6, 99.4, 100.0)); // entry fills here @ ~100.05
        cs.push(mk(2, 100.2, 108.5, 100.0, 108.0)); // held: MFE
        cs.push(mk(3, 107.0, 107.5, 95.8, 96.0)); // held: MAE (low above stop)
        cs.push(mk(4, 96.0, 96.5, 94.0, 94.0)); // stop hit intra-candle
        let strat = crate::strategy::StrategySection {
            name: "t".into(),
            pairs: vec!["T".into()],
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
            entry_window_utc: None,
            breakout_lookback_bars: None,
        };
        let bt = BacktestSection {
            start_equity_usd: 10_000.0,
            risk_per_trade_usd: 100.0,
            max_notional_pct_equity: 0.9,
            min_notional_usd: 15.0,
        };
        let plan = Some(crate::strategy::TradePlan {
            entry: 100.0,
            stop: 95.0,
            target: 110.0,
        });
        let sigs: Vec<Option<crate::strategy::TradePlan>> =
            (0..cs.len()).map(|i| if i == 0 { plan } else { None }).collect();
        // held bars: entry bar idx1 + idx2 + idx3 + exit bar idx4
        let out = run_with_signals(&cs, &strat, &bt, sigs);
        assert_eq!(out.trades.len(), 1);
        let t = &out.trades[0];
        assert!((t.entry_price - 100.0).abs() < 0.11, "fill at open+slip, got {}", t.entry_price);
        let risk = t.entry_price - 95.0;
        assert!(((t.mfe_r) - ((108.0 - t.entry_price) / risk)).abs() < 1e-9);
        // exit bar close (94) is included: tracked before exit check
        assert!(((t.mae_r) - ((94.0 - t.entry_price) / risk)).abs() < 1e-9);
        assert_eq!(t.bars_held, 4);
        let a = summarize_attribution(&out.trades).unwrap();
        assert!((a.avg_mfe_r - t.mfe_r).abs() < 1e-12);
        assert_eq!(a.median_bars_held, 4.0);
    }

    const CFG_TOML: &str = include_str!("../config/strategy_ema_rsi.toml");

    fn small_cfg() -> StrategySection {
        crate::strategy::StrategySection {
            name: "t".into(),
            pairs: vec!["T".into()],
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
            entry_window_utc: None,
            breakout_lookback_bars: None,
        }
    }

    fn bt_cfg(equity: f64, risk: f64) -> BacktestSection {
        // single source of truth: floors/caps come from the shipped config,
        // never re-hardcoded here
        let cfg = StrategyConfig::load_from_toml_str(CFG_TOML).unwrap();
        BacktestSection {
            start_equity_usd: equity,
            risk_per_trade_usd: risk,
            max_notional_pct_equity: cfg.backtest.max_notional_pct_equity,
            min_notional_usd: cfg.backtest.min_notional_usd,
        }
    }

    fn candle(i: usize, o: f64, c: f64) -> Candle {
        Candle {
            open_time: i as i64 * 3_600_000,
            open: o,
            high: o.max(c) + 0.5,
            low: o.min(c) - 0.5,
            close: c,
            volume: 100.0,
        }
    }

    /// Deterministic scenario producing EXACTLY ONE signal, hand-engineered:
    /// bars 0-14 flat @100 with wide bodies (h=102,l=98) pin TR=4, so
    /// ATR(3)=4.0 exactly. Bar 15 dips to 99.5 -> RSI(5) drops to 0; bar 16
    /// recovers to 100.5 -> RSI crosses up through 40 while close > slow EMA
    /// and fast EMA > slow EMA => plan on bar 16, fill on bar 17 (the last
    /// bar, which rallies through the target without nearing the stop).
    fn one_signal_series() -> Vec<Candle> {
        let ts = |i: usize| i as i64 * 3_600_000;
        let mut cs: Vec<Candle> = (0..15)
            .map(|i| Candle {
                open_time: ts(i),
                open: 100.0,
                high: 102.0,
                low: 98.0,
                close: 100.0,
                volume: 100.0,
            })
            .collect();
        cs.push(Candle {
            open_time: ts(15),
            open: 100.0,
            high: 101.5,
            low: 97.5,
            close: 99.5,
            volume: 100.0,
        });
        cs.push(Candle {
            open_time: ts(16),
            open: 99.5,
            high: 102.5,
            low: 98.5,
            close: 100.5,
            volume: 100.0,
        });
        cs.push(Candle {
            open_time: ts(17),
            open: 100.0,
            high: 115.0,
            low: 99.5,
            close: 114.0,
            volume: 100.0,
        });
        cs
    }

    /// small_cfg with the ATR multiplier engineered to 2.1125 so the stop
    /// distance from the actual fill is exactly $8.00 and $2 risk buys
    /// exactly 0.25 units — every downstream number is a finite decimal.
    fn exact_strat() -> StrategySection {
        StrategySection {
            atr_multiplier: 2.1125,
            ..small_cfg()
        }
    }

    #[test]
    fn config_file_loads_and_matches_spec_constants() {
        let cfg = StrategyConfig::load_from_toml_str(CFG_TOML).unwrap();
        assert_eq!(cfg.strategy.pairs.len(), 5);
        assert_eq!(cfg.backtest.risk_per_trade_usd, 2.0);
    }

    #[test]
    fn sizing_respects_fixed_risk_then_caps() {
        // risk dominates: $2 risk / $2 dist = 1.0 unit @100 => $100 notional on $400 eq (cap 200) ok
        let q = position_sizing(100.0, 98.0, 400.0, &bt_cfg(400.0, 2.0)).unwrap();
        assert!((q - 1.0).abs() < 1e-9);
        // cap binds: $50 risk would be huge; cap at 50% of 200 = 100 notional => 1.0 unit
        let q = position_sizing(100.0, 50.0, 200.0, &bt_cfg(200.0, 50.0)).unwrap();
        assert!((q - 1.0).abs() < 1e-9);
        // below floor skipped: tiny risk over wide stop -> dust notional
        assert!(position_sizing(100.0, 90.0, 200.0, &bt_cfg(200.0, 0.01)).is_none());
    }

    #[test]
    fn full_run_books_hand_computed_exact_literals() {
        // ALL numbers below derived by hand BEFORE running the engine:
        //
        //   plan on bar 16: entry = close = 100.5, ATR(3) = 4.0 (all TRs are 4)
        //     stop   = 100.5 - 2.1125 * 4          = 92.05
        //     target = 100.5 + 1.5 * (100.5-92.05) = 113.175
        //   fill on bar 17: 100 * (1 + 0.0005)     = 100.05      (slippage)
        //   sizing: $2 risk / (100.05 - 92.05 = 8.00) = 0.25 units
        //           cap = 0.5*200/100.05 = 0.99950... (not binding)
        //           notional = 25.0125 >= $15 floor -> trade allowed
        //   entry fee = 100.05    * 0.25 * 0.001 = 0.0250125
        //   exit fill  = 113.175  * (1 - 0.0005) = 113.1184125 (slippage)
        //   exit fee   = 113.1184125 * 0.25 * 0.001 = 0.028279603125
        //   gross      = (113.1184125 - 100.05) * 0.25 = 3.267103125
        //   net PnL    = 3.267103125 - 0.0250125 - 0.028279603125
        //              = 3.213811021875
        //   final eq   = 200 + 3.213811021875 = 203.213811021875
        let out = run(&one_signal_series(), &exact_strat(), &bt_cfg(200.0, 2.0));
        assert_eq!(out.trades.len(), 1);
        let t = &out.trades[0];
        assert_eq!(t.exit_reason, ExitReason::Target);
        assert!((t.qty - 0.25).abs() < 1e-9);
        assert!(
            (t.entry_price - 100.05).abs() < 1e-9,
            "entry fill incl. slippage"
        );
        assert!(
            (t.exit_price - 113.1184125).abs() < 1e-9,
            "target exit fill incl. slippage"
        );
        // per-side fee literals booked inside the net figure:
        let entry_fee: f64 = 0.0250125;
        let exit_fee: f64 = 0.028279603125;
        assert!((entry_fee - 100.05 * 0.25 * TAKER_FEE_RATE).abs() < 1e-15);
        assert!((exit_fee - 113.1184125 * 0.25 * TAKER_FEE_RATE).abs() < 1e-15);
        assert!(
            (t.pnl - 3.213_811_021_875).abs() < 1e-9,
            "net pnl literal, got {}",
            t.pnl
        );
        assert!((out.final_equity - 203.213_811_021_875).abs() < 1e-9);
    }

    #[test]
    fn stop_hit_when_market_crashes_after_entry() {
        let strat = small_cfg();
        let mut cs: Vec<Candle> = Vec::new();
        let mut i = 0usize;
        for _ in 0..20 {
            cs.push(candle(i, 100.0, 100.0));
            i += 1;
        }
        let mut price = 100.0;
        for _ in 0..60 {
            cs.push(candle(i, price, price + 0.1));
            price += 0.1;
            i += 1;
        }
        for _ in 0..2 {
            cs.push(candle(i, price, price - 0.5));
            price -= 0.5;
            i += 1;
        }
        cs.push(candle(i, price, price + 1.0));
        i += 1;
        // crash straight through any stop
        for _ in 0..3 {
            cs.push(candle(i, price, price - 6.0));
            price -= 6.0;
            i += 1;
        }
        let out = run(&cs, &strat, &bt_cfg(200.0, 2.0));
        assert_eq!(out.trades.len(), 1);
        assert_eq!(out.trades[0].exit_reason, ExitReason::Stop);
        assert!(out.trades[0].pnl < 0.0);
    }

    #[test]
    fn min_notional_skips_dust_signal_but_control_records_trade() {
        // Same valid signal (one_signal_series), two equity levels:
        // $20 equity -> notional cap binds at half equity = $10 < $15 floor,
        // so the position must be skipped entirely.
        let out = run(&one_signal_series(), &exact_strat(), &bt_cfg(20.0, 2.0));
        assert!(out.trades.is_empty());
        assert!((out.final_equity - 20.0).abs() < 1e-9);

        // Control: $40 equity lifts the cap to $20 >= $15 -> the identical
        // signal books its trade.
        let ctrl = run(&one_signal_series(), &exact_strat(), &bt_cfg(40.0, 2.0));
        assert_eq!(ctrl.trades.len(), 1);
    }
}

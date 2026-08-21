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
    /// (exit-entry)/entry net of costs
    pub pnl_pct: f64,
    pub exit_reason: ExitReason,
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
    let mut trades = Vec::new();
    let mut equity_curve = Vec::new();
    let mut equity = bt.start_equity_usd;
    let mut pos: Option<Position> = None;

    for i in 0..candles.len() {
        let c = candles[i];

        if let Some(p) = pos.take_if_in_place() {
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
                    pnl_pct: (exit_price - p.entry_price) / p.entry_price,
                    exit_reason: reason,
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
        }
    }

    fn bt_cfg(equity: f64, risk: f64) -> BacktestSection {
        BacktestSection {
            start_equity_usd: equity,
            risk_per_trade_usd: risk,
            max_notional_pct_equity: 0.5,
            min_notional_usd: 15.0,
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
        assert!((t.entry_price - 100.05).abs() < 1e-9, "entry fill incl. slippage");
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

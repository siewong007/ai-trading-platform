use crate::backtest::{EquityPoint, TradeRecord};

#[derive(Debug, Clone)]
pub struct Metrics {
    pub total_trades: usize,
    /// gross profit / gross loss; INF when no losses and >0 profit
    pub profit_factor: f64,
    #[allow(dead_code)] // surfaced in Phase 2 dashboard
    pub win_rate: f64,
    pub net_pnl: f64,
    pub max_drawdown_pct: f64,
}

pub fn compute(trades: &[TradeRecord], curve: &[EquityPoint]) -> Metrics {
    let gross_win: f64 = trades.iter().map(|t| t.pnl.max(0.0)).sum();
    let gross_loss: f64 = -trades.iter().map(|t| t.pnl.min(0.0)).sum::<f64>();
    let profit_factor = if gross_loss == 0.0 {
        if gross_win > 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    } else {
        gross_win / gross_loss
    };
    let wins = trades.iter().filter(|t| t.pnl > 0.0).count();
    let win_rate = if trades.is_empty() {
        0.0
    } else {
        wins as f64 / trades.len() as f64
    };
    Metrics {
        total_trades: trades.len(),
        profit_factor,
        win_rate,
        net_pnl: trades.iter().map(|t| t.pnl).sum(),
        max_drawdown_pct: max_drawdown_pct(curve),
    }
}

/// Max peak-to-trough decline in percent, from an equity curve.
pub fn max_drawdown_pct(curve: &[EquityPoint]) -> f64 {
    let mut peak = f64::NEG_INFINITY;
    let mut max_dd = 0.0f64;
    for p in curve {
        if p.equity > peak {
            peak = p.equity;
        }
        if peak > 0.0 {
            let dd = (peak - p.equity) / peak;
            if dd > max_dd {
                max_dd = dd;
            }
        }
    }
    max_dd * 100.0
}

/// Pre-registered gate from spec §5 — thresholds frozen BEFORE any results seen.
pub const GATE_MIN_PF: f64 = 1.30;
pub const GATE_MIN_PROFITABLE_PAIRS: usize = 3;
pub const GATE_MAX_DD_PCT: f64 = 20.0;
pub const GATE_MIN_TRADES_PER_PAIR: usize = 20;
pub const GATE_MAX_VARIANTS: u32 = 20;

#[derive(Debug)]
pub struct PairReport {
    pub symbol: String,
    pub metrics: Metrics,
}

#[derive(Debug)]
pub struct GateVerdict {
    pub pass: bool,
    /// fatal problems — any entry means FAIL
    pub reasons: Vec<String>,
    /// non-fatal observations (e.g. some pairs below the trade floor); they
    /// matter only via qualification, never as an independent veto
    pub notes: Vec<String>,
}

/// Error function, Abramowitz & Stegun 7.1.26 (|eps| < 1.5e-7).
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;
    let t = 1.0 / (1.0 + p * x);
    // A&S 7.1.26: erf(x) = 1 - (a1 t + a2 t^2 + a3 t^3 + a4 t^4 + a5 t^5) e^{-x^2}
    let poly = (((a5 * t + a4) * t + a3) * t + a2) * t + a1;
    let y = 1.0 - poly * t * (-x * x).exp();
    sign * y
}

/// Expected maximum of `t` iid standard normals, by Simpson integration of
/// the order-statistic density t·x·φ(x)·Φ(x)^(t-1); 0 for t<2.
pub fn expected_max_normal(t: usize) -> f64 {
    if t < 2 {
        return 0.0;
    }
    let tf = t as f64;
    let phi = |x: f64| (-0.5 * x * x).exp() / (2.0 * std::f64::consts::PI).sqrt();
    let cdf = |x: f64| 0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2));
    let f = |x: f64| tf * x * phi(x) * cdf(x).powf(tf - 1.0);
    let (a, b) = (-9.0f64, 9.0f64);
    let n = 600usize; // even
    let h = (b - a) / n as f64;
    let sum: f64 = (0..=n)
        .map(|i| {
            let x = a + i as f64 * h;
            let w = if i == 0 || i == n {
                1.0
            } else if i % 2 == 1 {
                4.0
            } else {
                2.0
            };
            w * f(x)
        })
        .sum();
    h / 3.0 * sum
}

pub fn luck_adjusted_best_pf(pfs: &[f64]) -> Option<f64> {
    let n = pfs.len();
    if n == 0 {
        return None;
    }
    // degenerate PFs (0.0 = no trades) carry no dispersion information
    let live: Vec<f64> = pfs.iter().copied().filter(|p| *p > 0.0).collect();
    if live.is_empty() {
        return None;
    }
    let m = live.iter().sum::<f64>() / live.len() as f64;
    let var = live.iter().map(|p| (p - m) * (p - m)).sum::<f64>() / live.len() as f64;
    Some(m + expected_max_normal(n) * var.sqrt())
}

pub fn evaluate_gate(reports: &[PairReport]) -> GateVerdict {
    let mut reasons = Vec::new();
    let mut notes = Vec::new();

    // pooled PF across all pairs that meet the sample-size floor
    let qualifying: Vec<&PairReport> = reports
        .iter()
        .filter(|r| r.metrics.total_trades >= GATE_MIN_TRADES_PER_PAIR)
        .collect();
    if qualifying.len() < reports.len() {
        notes.push(format!(
            "{}/{} pairs below trade-count floor ({} needed)",
            reports.len() - qualifying.len(),
            reports.len(),
            GATE_MIN_TRADES_PER_PAIR
        ));
    }
    // zero pairs qualify -> the PF check is impossible: the gate must FAIL,
    // never silently pass
    if qualifying.is_empty() {
        reasons.push(format!(
            "0/{} pairs meet the trade-count floor ({} needed) — profit-factor check impossible",
            reports.len(),
            GATE_MIN_TRADES_PER_PAIR
        ));
    }

    // strictest reading of the pre-registered gate: EVERY pair with enough
    // sample size must itself clear the PF floor
    let worst_pf = qualifying
        .iter()
        .map(|r| r.metrics.profit_factor)
        .fold(f64::INFINITY, f64::min);
    if !qualifying.is_empty() && worst_pf < GATE_MIN_PF {
        reasons.push(format!(
            "worst qualifying-pair PF {:.2} < {:.2}",
            worst_pf, GATE_MIN_PF
        ));
    }

    // "profitable OOS on >= 3 of N pairs", literally: thin pairs (< trade
    // floor) count as NOT profitable, so >=3 must hold among ALL evaluated
    let profitable_pairs = qualifying
        .iter()
        .filter(|r| r.metrics.net_pnl > 0.0)
        .count();
    if profitable_pairs < GATE_MIN_PROFITABLE_PAIRS {
        reasons.push(format!(
            "{} profitable pairs (of {} evaluated) < {} required",
            profitable_pairs,
            reports.len(),
            GATE_MIN_PROFITABLE_PAIRS
        ));
    }

    let worst_dd = reports
        .iter()
        .map(|r| r.metrics.max_drawdown_pct)
        .fold(0.0f64, f64::max);
    if worst_dd >= GATE_MAX_DD_PCT {
        reasons.push(format!(
            "max drawdown {:.1}% >= {:.1}%",
            worst_dd, GATE_MAX_DD_PCT
        ));
    }

    GateVerdict {
        pass: reasons.is_empty(),
        reasons,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::ExitReason;

    #[test]
    fn expected_max_normal_known_shapes() {
        assert_eq!(expected_max_normal(0), 0.0);
        assert_eq!(expected_max_normal(1), 0.0);
        let e2 = expected_max_normal(2);
        assert!((e2 - 0.564).abs() < 0.05, "E[max of 2]≈0.564, got {e2}");
        let e12 = expected_max_normal(12);
        assert!(e12 > expected_max_normal(10) && (1.3..=1.7).contains(&e12),
            "t=12 should be ≈1.5σ, got {e12}");
        assert!(expected_max_normal(100) > expected_max_normal(50));
    }

    #[test]
    fn luck_adjusted_uses_dispersion_of_live_pfs() {
        // identical pfs -> zero dispersion -> expectation equals the mean
        let flat = vec![1.0, 1.0, 1.0, 1.0];
        let a = luck_adjusted_best_pf(&flat).unwrap();
        assert!((a - 1.0).abs() < 1e-9);
        // degenerate zeros are excluded from dispersion
        let mixed = vec![1.0, 1.0, 0.0, 0.0];
        let b = luck_adjusted_best_pf(&mixed).unwrap();
        assert!((b - 1.0).abs() < 1e-9);
        assert!(luck_adjusted_best_pf(&[0.0, 0.0]).is_none());
        assert!(luck_adjusted_best_pf(&[]).is_none());
    }


    fn tr(pnl: f64) -> TradeRecord {
        TradeRecord {
            entry_ts: 0,
            exit_ts: 1,
            qty: 0.01,
            entry_price: 100.0,
            exit_price: 100.0 + pnl * 100.0,
            pnl,
            pnl_pct: pnl / 10.0,
            mfe_r: 0.0,
            mae_r: 0.0,
            bars_held: 1,
            exit_reason: if pnl > 0.0 {
                ExitReason::Target
            } else {
                ExitReason::Stop
            },
        }
    }

    #[test]
    fn metrics_basic_counts() {
        let trades = vec![tr(3.0), tr(-1.0), tr(2.0)];
        let m = compute(&trades, &[]);
        assert_eq!(m.total_trades, 3);
        assert!((m.win_rate - 2.0 / 3.0).abs() < 1e-9);
        assert!((m.profit_factor - 5.0).abs() < 1e-9); // 5 win / 1 loss
        assert!((m.net_pnl - 4.0).abs() < 1e-9);
    }

    #[test]
    fn no_losses_gives_infinite_pf_only_with_profit() {
        assert_eq!(compute(&[tr(1.0)], &[]).profit_factor, f64::INFINITY);
        assert_eq!(compute(&[], &[]).profit_factor, 0.0);
    }

    #[test]
    fn drawdown_known_case() {
        let curve: Vec<EquityPoint> = [200.0, 210.0, 189.0, 205.0]
            .iter()
            .enumerate()
            .map(|(i, &e)| EquityPoint {
                ts: i as i64,
                equity: e,
            })
            .collect();
        // peak 210 trough 189 => dd = 21/210 = 10%
        assert!((max_drawdown_pct(&curve) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn gate_fails_without_enough_trades_or_pairs() {
        let good_pair = |sym: &str, n: usize, pnl_per: f64| PairReport {
            symbol: sym.into(),
            metrics: compute(&(0..n).map(|_| tr(pnl_per)).collect::<Vec<_>>(), &[]),
        };
        // all winning but only 5 trades/pair on 2 pairs
        let v = evaluate_gate(&[good_pair("A", 5, 1.0), good_pair("B", 5, 1.0)]);
        assert!(!v.pass);

        // 4/4 pairs, 30 trades each, PF inf, dd small -> pass
        let v = evaluate_gate(&[
            good_pair("A", 30, 1.0),
            good_pair("B", 30, 1.0),
            good_pair("C", 30, 1.0),
            good_pair("D", 30, 1.0),
        ]);
        assert!(v.pass, "expected pass, got {:?}", v.reasons);

        // one losing pair drags the worst qualifying-pair PF below the floor
        let v = evaluate_gate(&[
            good_pair("A", 30, 1.0),
            good_pair("B", 30, 1.0),
            good_pair("C", 30, 1.0),
            good_pair("D", 30, -1.0),
        ]);
        assert!(!v.pass);
    }

    #[test]
    fn gate_counts_only_thick_pairs_as_profitable() {
        let pair = |sym: &str, n: usize, pnl_per: f64| PairReport {
            symbol: sym.into(),
            metrics: compute(&(0..n).map(|_| tr(pnl_per)).collect::<Vec<_>>(), &[]),
        };

        // 2 profitable of 5 evaluated -> FAIL: needs >= 3 among ALL pairs,
        // and thin pairs can never count as profitable
        let v = evaluate_gate(&[
            pair("A", 30, 1.0),
            pair("B", 30, 1.0),
            pair("C", 5, 1.0), // profitable-looking but below trade floor
            pair("D", 5, 1.0),
            pair("E", 5, 1.0),
        ]);
        assert!(!v.pass);
        assert!(
            v.reasons.iter().any(|r| r.contains("profitable pairs")),
            "{:?}",
            v.reasons
        );

        // 3 of 5 -> PASS: A/B/C qualify and are profitable; thin D/E only add
        // a below-floor note
        let v = evaluate_gate(&[
            pair("A", 30, 1.0),
            pair("B", 30, 1.0),
            pair("C", 30, 1.0),
            pair("D", 5, 1.0),
            pair("E", 5, -1.0),
        ]);
        assert!(v.pass, "expected 3-of-5 pass, got {:?}", v.reasons);
    }

    #[test]
    fn gate_fails_when_zero_pairs_qualify() {
        // all five pairs below the trade-count floor -> zero qualifying pairs:
        // gate must fail explicitly instead of silently skipping the PF check
        let thin: Vec<PairReport> = (0..5)
            .map(|i| PairReport {
                symbol: format!("P{i}"),
                metrics: compute(&(0..5).map(|_| tr(1.0)).collect::<Vec<_>>(), &[]),
            })
            .collect();
        let v = evaluate_gate(&thin);
        assert!(!v.pass);
        assert!(
            v.reasons.iter().any(|r| r.contains("0/5")),
            "{:?}",
            v.reasons
        );
    }

    #[test]
    fn gate_fails_on_low_pf_and_deep_drawdown() {
        // 24 wins x $1 vs 20 losses x $1 => PF = 24/20 = 1.2 < 1.3, 44 trades
        let mixed: Vec<TradeRecord> = (0..20)
            .map(|_| tr(-1.0))
            .chain((0..24).map(|_| tr(1.0)))
            .collect();
        let low_pf = PairReport {
            symbol: "Z".into(),
            metrics: compute(&mixed, &[]),
        };
        assert!((low_pf.metrics.profit_factor - 1.2).abs() < 1e-9);
        let v = evaluate_gate(&[low_pf]);
        assert!(
            v.reasons.iter().any(|r| r.contains("PF")),
            "{:?}",
            v.reasons
        );

        // deep drawdown fails even with great PF
        let curve = vec![
            EquityPoint {
                ts: 0,
                equity: 100.0,
            },
            EquityPoint {
                ts: 1,
                equity: 70.0,
            },
            EquityPoint {
                ts: 2,
                equity: 99.0,
            },
        ];
        let deep_dd = PairReport {
            symbol: "W".into(),
            metrics: compute(&(0..30).map(|_| tr(1.0)).collect::<Vec<_>>(), &curve),
        };
        assert!(deep_dd.metrics.max_drawdown_pct >= GATE_MAX_DD_PCT);
        let v = evaluate_gate(&[deep_dd]);
        assert!(!v.pass);
    }
}

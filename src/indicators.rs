/// Exponential moving average. Returns EMA aligned to input length;
/// first value seeded with SMA of the first `period` values (earlier entries are None).
pub fn ema(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out: Vec<Option<f64>> = vec![None; values.len()];
    if values.len() < period || period == 0 {
        return out;
    }
    let k = 2.0 / (period as f64 + 1.0);
    let seed: f64 = values[..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = Some(seed);
    let mut prev = seed;
    for i in period..values.len() {
        prev = (values[i] - prev) * k + prev;
        out[i] = Some(prev);
    }
    out
}

/// Wilder's RSI.
pub fn rsi(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out: Vec<Option<f64>> = vec![None; n];
    if n <= period || period == 0 {
        return out;
    }
    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;
    for w in closes[..(period + 1).min(closes.len())].windows(2) {
        let d = w[1] - w[0];
        if d >= 0.0 {
            avg_gain += d;
        } else {
            avg_loss -= d;
        }
    }
    avg_gain /= period as f64;
    avg_loss /= period as f64;
    out[period] = Some(rsi_from(avg_gain, avg_loss));
    for i in (period + 1)..n {
        let d = closes[i] - closes[i - 1];
        let (g, l) = if d >= 0.0 { (d, 0.0) } else { (0.0, -d) };
        avg_gain = (avg_gain * (period as f64 - 1.0) + g) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + l) / period as f64;
        out[i] = Some(rsi_from(avg_gain, avg_loss));
    }
    out
}

fn rsi_from(avg_gain: f64, avg_loss: f64) -> f64 {
    if avg_loss == 0.0 {
        return 100.0;
    }
    let rs = avg_gain / avg_loss;
    100.0 - 100.0 / (1.0 + rs)
}

/// Wilder's ATR over OHLC candles.
pub fn atr(
    highs: &[f64],
    lows: &[f64],
    closes: &[f64],
    period: usize,
) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out: Vec<Option<f64>> = vec![None; n];
    if n < period + 1 || period == 0 {
        return out;
    }
    let tr: Vec<f64> = (1..n)
        .map(|i| {
            (highs[i] - lows[i])
                .max((highs[i] - closes[i - 1]).abs())
                .max((lows[i] - closes[i - 1]).abs())
        })
        .collect();
    // tr[0] corresponds to candle index 1
    let mut a: f64 = tr[..period].iter().sum::<f64>() / period as f64;
    out[period] = Some(a);
    for i in (period + 1)..n {
        a = (a * (period as f64 - 1.0) + tr[i - 1]) / period as f64;
        out[i] = Some(a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn ema_matches_hand_computed() {
        // SMA(3) of [1,2,3,4,5] seeds at index 2 => 2.0; k=0.5
        let e = ema(&[1.0, 2.0, 3.0, 4.0, 5.0], 3);
        assert_eq!(e[0], None);
        assert_eq!(e[1], None);
        assert!(approx(e[2].unwrap(), 2.0));
        // (4-2)*0.5+2 = 3
        assert!(approx(e[3].unwrap(), 3.0));
        // (5-3)*0.5+3 = 4
        assert!(approx(e[4].unwrap(), 4.0));
    }

    #[test]
    fn rsi_all_gains_is_100() {
        let r = rsi(&[1.0, 2.0, 3.0, 4.0, 5.0], 3);
        assert!(approx(r[3].unwrap(), 100.0));
    }

    #[test]
    fn rsi_hand_computed_case() {
        // gains: 1,1,1 ; losses: 0 over first 4 points, period 3
        // avg_gain=1, avg_loss=0 -> RSI=100 at idx 3
        // next delta -1: avg_gain=2/3... check formula continuity instead on known series
        let r = rsi(&[10.0, 11.0, 12.0, 13.0, 12.0], 3);
        assert_eq!(r[0], None);
        assert!(r[3].is_some());
        assert!(approx(r[3].unwrap(), 100.0));
        // after one loss: avg_gain=(1*2+0)/3=2/3, avg_loss=(0*2+1)/3=1/3
        let rs = (2.0 / 3.0) / (1.0 / 3.0);
        let expect = 100.0 - 100.0 / (1.0 + rs);
        assert!(approx(r[4].unwrap(), expect));
    }

    #[test]
    fn atr_constant_range() {
        // flat closes at 10, high 11 low 9 => TR=2 every bar
        let h = [11.0; 6];
        let l = [9.0; 6];
        let c = [10.0; 6];
        let a = atr(&h, &l, &c, 3);
        assert_eq!(a[0], None);
        assert_eq!(a[2], None); // needs period TRs => available from idx period(3)
        assert!(approx(a[3].unwrap(), 2.0));
        assert!(approx(a[4].unwrap(), 2.0));
    }

    #[test]
    fn short_input_yields_none() {
        assert!(ema(&[1.0, 2.0], 3).iter().all(|v| v.is_none()));
        assert!(rsi(&[1.0, 2.0], 14).iter().all(|v| v.is_none()));
    }
}

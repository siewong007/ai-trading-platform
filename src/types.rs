use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candle {
    pub open_time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// Raw Binance kline row: array of mixed strings/ints.
#[derive(Deserialize)]
struct RawKline(
    i64,                        // open time
    String,                     // open
    String,                     // high
    String,                     // low
    String,                     // close
    String,                     // volume
    #[allow(dead_code)] i64,    // close time
    #[allow(dead_code)] String, // quote volume
    #[allow(dead_code)] i64,    // trades
    #[allow(dead_code)] String, // taker base vol
    #[allow(dead_code)] String, // taker quote vol
    #[allow(dead_code)] String, // ignore
);

impl TryFrom<RawKline> for Candle {
    type Error = anyhow::Error;
    fn try_from(r: RawKline) -> Result<Self, Self::Error> {
        let RawKline(open_time, o, h, l, c, v, ..) = r;
        Ok(Self {
            open_time,
            open: o.parse()?,
            high: h.parse()?,
            low: l.parse()?,
            close: c.parse()?,
            volume: v.parse()?,
        })
    }
}

pub fn parse_klines(json: &str) -> anyhow::Result<Vec<Candle>> {
    let raw: Vec<RawKline> = serde_json::from_str(json)?;
    raw.into_iter().map(Candle::try_from).collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)] // Phase 2 (executor) consumes these
pub enum Side {
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)] // Phase 2 (shadow/live modes) consumes this
pub enum FillMode {
    Backtest,
    Shadow,
    Live,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
      [1499040000000,"0.01634790","0.80000000","0.01575800","0.01577100","148976.11427815",1499644799999,"2434.19055334",308,"1756.87402397","28.46694368","0"],
      [1499043600000,"0.01577100","0.01577100","0.01575800","0.01577100","1000.0",1499648399999,"15.771",10,"500.0","7.88","0"]
    ]"#;

    #[test]
    fn parses_binance_kline_array() {
        let candles = parse_klines(SAMPLE).unwrap();
        assert_eq!(candles.len(), 2);
        let c = &candles[0];
        assert_eq!(c.open_time, 1_499_040_000_000);
        assert!((c.open - 0.01634790).abs() < 1e-9);
        assert!((c.high - 0.8).abs() < 1e-9);
        assert!((c.low - 0.01575800).abs() < 1e-9);
        assert!((c.close - 0.01577100).abs() < 1e-9);
        assert!((c.volume - 148_976.11427815).abs() < 1e-6);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_klines("not json").is_err());
    }

    #[test]
    fn rejects_non_numeric_prices_in_valid_json_shape() {
        // valid JSON array-of-arrays, but the OHLCV strings are not numbers
        let bad = r#"[["1499040000000","zero","0.8","0.015","0.0157","100"]]"#;
        assert!(parse_klines(bad).is_err());

        // and a volume string that merely looks numeric-ish
        let bad2 = r#"[["1499040000000","1","1","1","1","1,0"]]"#;
        assert!(parse_klines(bad2).is_err());
    }
}

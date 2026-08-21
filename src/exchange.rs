use crate::types::{parse_klines, Candle};

pub struct Exchange {
    http: reqwest::Client,
    base: String,
    backoff_base_secs: u64,
}

const KLINE_LIMIT: u32 = 1000;

impl Exchange {
    pub fn new(base: &str) -> anyhow::Result<Self> {
        Self::with_backoff(base, 30)
    }

    pub fn with_backoff(base: &str, backoff_base_secs: u64) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base: base.trim_end_matches('/').to_string(),
            backoff_base_secs,
        })
    }

    #[allow(dead_code)] // health checks use this in Phase 2
    pub async fn ping(&self) -> anyhow::Result<()> {
        self.http
            .get(format!("{}/api/v3/ping", self.base))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Fetch klines paginated from `start_ms` (inclusive) to present.
    pub async fn fetch_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
    ) -> anyhow::Result<Vec<Candle>> {
        let mut out = Vec::new();
        let mut cursor = start_ms;
        loop {
            let batch = self.fetch_page(symbol, interval, cursor).await?;
            let got = batch.len();
            if let Some(last) = batch.last() {
                cursor = last.open_time + 1;
            }
            out.extend(batch);
            if got < KLINE_LIMIT as usize {
                break;
            }
        }
        out.dedup_by(|a, b| a.open_time == b.open_time);
        Ok(out)
    }

    async fn fetch_page(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
    ) -> anyhow::Result<Vec<Candle>> {
        let url = format!(
            "{}/api/v3/klines?symbol={}&interval={}&startTime={}&limit={}",
            self.base, symbol, interval, start_ms, KLINE_LIMIT
        );
        let mut attempts = 0;
        loop {
            attempts += 1;
            let resp = self.http.get(&url).send().await?;
            match resp.status().as_u16() {
                200 => {
                    let text = resp.text().await?;
                    return parse_klines(&text);
                }
                429 | 418 if attempts < 5 => {
                    tracing::warn!(%symbol, status = %resp.status(), "rate limited, backing off");
                    tokio::time::sleep(std::time::Duration::from_secs(
                        self.backoff_base_secs * attempts as u64,
                    ))
                    .await;
                }
                s => anyhow::bail!("binance klines {symbol} HTTP {s} after {attempts} attempts"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn paginates_until_short_page() {
        let server = MockServer::start().await;
        let page1 = r#"[[1000,"10","11","9","10","1",1999999,"q",1,"t","tq","0"]]"#;
        let page2 = r#"[]"#;
        Mock::given(method("GET"))
            .and(path("/api/v3/klines"))
            .and(query_param("startTime", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page1))
            .mount(&server)
            .await;
        // second call starts at 1001 (last open_time + 1)
        Mock::given(method("GET"))
            .and(path("/api/v3/klines"))
            .and(query_param("startTime", "1001"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page2))
            .mount(&server)
            .await;

        let ex = Exchange::with_backoff(&server.uri(), 0).unwrap();
        let ks = ex.fetch_klines("BTCUSDT", "1h", 1000).await.unwrap();
        assert_eq!(ks.len(), 1);
        assert_eq!(ks[0].open_time, 1000);
    }

    #[tokio::test]
    async fn retries_on_rate_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"[[5000,"1","1","1","1","1",5999,"q",1,"t","tq","0"]]"#),
            )
            .mount(&server)
            .await;
        let ex = Exchange::with_backoff(&server.uri(), 0).unwrap();
        let ks = ex.fetch_klines("X", "1h", 5000).await.unwrap();
        assert_eq!(ks[0].close, 1.0);
    }

    #[tokio::test]
    #[ignore] // live network test: cargo test -- --ignored
    async fn live_binance_reachable_and_parses() {
        let ex = Exchange::new("https://api.binance.com").unwrap();
        ex.ping().await.unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let ks = ex
            .fetch_klines("BTCUSDT", "1h", now - 7 * 86_400_000)
            .await
            .unwrap();
        assert!(ks.len() > 100);
        assert!(ks.windows(2).all(|w| w[0].open_time < w[1].open_time));
    }
}

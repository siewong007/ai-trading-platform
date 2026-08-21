use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const RECV_WINDOW: u64 = 5000;
const TIMEOUT_SECS: u64 = 30;

pub struct Keys {
    pub api_key: String,
    pub secret: String,
}

impl Keys {
    pub fn from_env() -> anyhow::Result<Keys> {
        let api_key = std::env::var("BINANCE_API_KEY")
            .map_err(|_| anyhow::anyhow!("missing env var BINANCE_API_KEY"))?;
        let secret = std::env::var("BINANCE_API_SECRET")
            .map_err(|_| anyhow::anyhow!("missing env var BINANCE_API_SECRET"))?;
        Ok(Keys { api_key, secret })
    }
}

pub fn sign_query(secret: &str, query: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(query.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // Phase 2 (executor) consumes these
pub struct Balance {
    pub asset: String,
    pub free: f64,
    pub locked: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)] // Phase 2 (executor) consumes these
pub struct SymbolFilters {
    pub step_size: f64,
    pub min_qty: f64,
    pub min_notional: f64,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // Phase 2 (executor) consumes these
pub struct OpenOrder {
    pub order_id: i64,
    pub client_order_id: String,
    pub symbol: String,
    pub side: String,
    pub otype: String,
    pub price: f64,
    pub orig_qty: f64,
    pub executed_qty: f64,
    pub status: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)] // Phase 2 (executor) consumes these
pub struct MyTrade {
    pub id: i64,
    pub order_id: i64,
    pub price: f64,
    pub qty: f64,
    pub commission: f64,
    pub time: i64,
}

pub struct SignedClient {
    http: reqwest::Client,
    base: String,
    keys: Keys,
}

impl SignedClient {
    pub fn new(base: &str, keys: Keys) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
                .build()?,
            base: base.trim_end_matches('/').to_string(),
            keys,
        })
    }

    pub async fn signed_get(&self, path: &str, params: &[(&str, &str)]) -> anyhow::Result<String> {
        self.request(reqwest::Method::GET, path, params).await
    }

    pub async fn signed_post(&self, path: &str, params: &[(&str, &str)]) -> anyhow::Result<String> {
        self.request(reqwest::Method::POST, path, params).await
    }

    pub async fn signed_delete(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<String> {
        self.request(reqwest::Method::DELETE, path, params).await
    }

    #[allow(dead_code)] // Phase 2 (executor) consumes this
    pub async fn balances(&self) -> anyhow::Result<Vec<Balance>> {
        let body = self.signed_get("/api/v3/account", &[]).await?;
        let raw: RawAccount = serde_json::from_str(&body)?;
        raw.balances
            .into_iter()
            .map(|b| {
                Ok(Balance {
                    asset: b.asset,
                    free: b.free.parse()?,
                    locked: b.locked.parse()?,
                })
            })
            .collect()
    }

    #[allow(dead_code)] // Phase 2 (executor) consumes this
    pub async fn open_orders(&self, symbol: &str) -> anyhow::Result<Vec<OpenOrder>> {
        let body = self
            .signed_get("/api/v3/openOrders", &[("symbol", symbol)])
            .await?;
        let raw: Vec<RawOpenOrder> = serde_json::from_str(&body)?;
        raw.into_iter()
            .map(|o| {
                Ok(OpenOrder {
                    order_id: o.order_id,
                    client_order_id: o.client_order_id,
                    symbol: o.symbol,
                    side: o.side,
                    otype: o.otype,
                    price: o.price.parse()?,
                    orig_qty: o.orig_qty.parse()?,
                    executed_qty: o.executed_qty.parse()?,
                    status: o.status,
                })
            })
            .collect()
    }

    #[allow(dead_code)] // Phase 2 (executor) consumes this
    pub async fn my_trades(&self, symbol: &str, limit: u32) -> anyhow::Result<Vec<MyTrade>> {
        let limit_str = limit.to_string();
        let body = self
            .signed_get(
                "/api/v3/myTrades",
                &[("symbol", symbol), ("limit", limit_str.as_str())],
            )
            .await?;
        let raw: Vec<RawMyTrade> = serde_json::from_str(&body)?;
        raw.into_iter()
            .map(|t| {
                Ok(MyTrade {
                    id: t.id,
                    order_id: t.order_id,
                    price: t.price.parse()?,
                    qty: t.qty.parse()?,
                    commission: t.commission.parse()?,
                    time: t.time,
                })
            })
            .collect()
    }

    #[allow(dead_code)] // Phase 2 (executor) consumes this
    pub async fn symbol_filters(base_url: &str, symbol: &str) -> anyhow::Result<SymbolFilters> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()?;
        let url = format!(
            "{}/api/v3/exchangeInfo?symbol={}",
            base_url.trim_end_matches('/'),
            symbol
        );
        let resp = http.get(&url).send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("binance HTTP {status}: {body}");
        }
        let info: RawExchangeInfo = serde_json::from_str(&body)?;
        let sym = info
            .symbols
            .into_iter()
            .find(|s| s.symbol == symbol)
            .ok_or_else(|| anyhow::anyhow!("exchangeInfo: symbol {symbol} not found"))?;
        let mut step_size = None;
        let mut min_qty = None;
        let mut notional = None;
        let mut legacy_notional = None;
        for f in sym.filters {
            match f {
                RawFilter::LotSize {
                    step_size: s,
                    min_qty: q,
                } => {
                    step_size = Some(s.parse()?);
                    min_qty = Some(q.parse()?);
                }
                RawFilter::Notional { min_notional } => notional = Some(min_notional),
                RawFilter::LegacyMinNotional { min_notional } => {
                    legacy_notional = Some(min_notional)
                }
                RawFilter::Other => {}
            }
        }
        let no_lot = || anyhow::anyhow!("exchangeInfo {symbol}: no LOT_SIZE filter");
        let min_notional = notional.or(legacy_notional).ok_or_else(|| {
            anyhow::anyhow!("exchangeInfo {symbol}: no NOTIONAL/MIN_NOTIONAL filter")
        })?;
        Ok(SymbolFilters {
            step_size: step_size.ok_or_else(no_lot)?,
            min_qty: min_qty.ok_or_else(no_lot)?,
            min_notional: min_notional.parse()?,
        })
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<String> {
        let mut parts: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
        parts.push(format!(
            "timestamp={}",
            chrono::Utc::now().timestamp_millis()
        ));
        parts.push(format!("recvWindow={RECV_WINDOW}"));
        let query = parts.join("&");
        let signature = sign_query(&self.keys.secret, &query);
        let url = format!("{}{}?{query}&signature={signature}", self.base, path);
        let resp = self
            .http
            .request(method, &url)
            .header("X-MBX-APIKEY", &self.keys.api_key)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            #[derive(serde::Deserialize)]
            struct BinanceError {
                code: i64,
                msg: String,
            }
            match serde_json::from_str::<BinanceError>(&body) {
                Ok(e) => anyhow::bail!("binance error {}: {}", e.code, e.msg),
                Err(_) => anyhow::bail!("binance HTTP {status}: {body}"),
            }
        }
        Ok(body)
    }
}

#[derive(Deserialize)]
struct RawAccount {
    balances: Vec<RawBalance>,
}

#[derive(Deserialize)]
struct RawBalance {
    asset: String,
    free: String,
    locked: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOpenOrder {
    order_id: i64,
    client_order_id: String,
    symbol: String,
    side: String,
    #[serde(rename = "type")]
    otype: String,
    price: String,
    orig_qty: String,
    executed_qty: String,
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMyTrade {
    id: i64,
    order_id: i64,
    price: String,
    qty: String,
    commission: String,
    time: i64,
}

#[derive(Deserialize)]
struct RawExchangeInfo {
    symbols: Vec<RawSymbolInfo>,
}

#[derive(Deserialize)]
struct RawSymbolInfo {
    symbol: String,
    filters: Vec<RawFilter>,
}

#[derive(Deserialize)]
#[serde(tag = "filterType")]
enum RawFilter {
    #[serde(rename = "LOT_SIZE")]
    LotSize {
        #[serde(rename = "stepSize")]
        step_size: String,
        #[serde(rename = "minQty")]
        min_qty: String,
    },
    #[serde(rename = "NOTIONAL")]
    Notional {
        #[serde(rename = "minNotional")]
        min_notional: String,
    },
    #[serde(rename = "MIN_NOTIONAL")]
    LegacyMinNotional {
        #[serde(rename = "minNotional")]
        min_notional: String,
    },
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET: &str = "test-secret";
    const API_KEY: &str = "test-api-key";

    #[test]
    fn rfc_4231_test_case_2_vector() {
        assert_eq!(
            sign_query("Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[tokio::test]
    async fn get_signs_params_in_given_order_signature_last_api_key_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;
        let client = signed_client(&server.uri());
        let body = client
            .signed_get("/api/v3/test", &[("symbol", "BTCUSDT"), ("side", "BUY")])
            .await
            .unwrap();
        assert_eq!(body, r#"{"ok":true}"#);

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        let query = reqs[0].url.query().unwrap();
        let (prefix, sig) = query.split_once("&signature=").unwrap();
        assert!(
            prefix.starts_with("symbol=BTCUSDT&side=BUY&timestamp="),
            "params must be joined in given order, timestamp auto-appended: {query}"
        );
        assert!(prefix.ends_with("&recvWindow=5000"), "{query}");
        assert_eq!(sig, sign_query(SECRET, prefix));
        let key = reqs[0]
            .headers
            .get("X-MBX-APIKEY")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(key, API_KEY);
    }

    #[tokio::test]
    async fn post_and_delete_use_their_methods_with_signed_query() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/order"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v3/order"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let client = signed_client(&server.uri());
        client
            .signed_post("/api/v3/order", &[("symbol", "BTCUSDT")])
            .await
            .unwrap();
        client
            .signed_delete("/api/v3/order", &[("symbol", "BTCUSDT")])
            .await
            .unwrap();

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].method.as_str(), "POST");
        assert_eq!(reqs[1].method.as_str(), "DELETE");
        for req in &reqs {
            let query = req.url.query().unwrap();
            let (prefix, sig) = query.split_once("&signature=").unwrap();
            assert_eq!(sig, sign_query(SECRET, prefix));
        }
    }

    #[tokio::test]
    async fn non_2xx_surfaces_binance_code_and_msg() {
        for (code, msg) in [
            (-1022, "Signature for this request is not valid."),
            (
                -1021,
                "Timestamp for this request is outside of the recvWindow.",
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(400)
                        .set_body_string(format!(r#"{{"code":{code},"msg":"{msg}"}}"#)),
                )
                .mount(&server)
                .await;
            let client = signed_client(&server.uri());
            let err = client.signed_get("/api/v3/test", &[]).await.unwrap_err();
            let expected = format!("binance error {code}: {msg}");
            assert!(err.to_string().contains(&expected), "{err}");
        }
    }

    #[test]
    fn from_env_reads_vars_and_errors_name_only_the_missing_var() {
        std::env::remove_var("BINANCE_API_KEY");
        std::env::remove_var("BINANCE_API_SECRET");
        let err = keys_err(Keys::from_env());
        assert!(err.contains("BINANCE_API_KEY"), "{err}");

        std::env::set_var("BINANCE_API_KEY", API_KEY);
        let err = keys_err(Keys::from_env());
        assert!(err.contains("BINANCE_API_SECRET"), "{err}");
        assert!(!err.contains(API_KEY), "error must not leak values: {err}");

        std::env::set_var("BINANCE_API_SECRET", SECRET);
        let keys = Keys::from_env().unwrap();
        assert_eq!(keys.api_key, API_KEY);
        assert_eq!(keys.secret, SECRET);
    }

    #[tokio::test]
    async fn balances_parses_plain_and_testnet_wallet_description_shapes() {
        let plain = r#"{"canTrade":true,"updateTime":1700000000000,"balances":[
            {"asset":"BTC","free":"4723846.89208100","locked":"0.00000000"},
            {"asset":"USDT","free":"100.50","locked":"2.25"}
        ]}"#;
        let testnet = r#"{"makerCommission":0,"canTrade":true,"updateTime":1700000000000,
            "accountType":"SPOT","balances":[{"asset":"BTC","free":"1.00000000","locked":"0.50000000"}],
            "permissions":["SPOT"],"walletDescription":"spot test wallet"}"#;
        for (shape, locked_usdt) in [(plain, None), (testnet, Some(0.5))] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v3/account"))
                .respond_with(ResponseTemplate::new(200).set_body_string(shape))
                .mount(&server)
                .await;
            let client = signed_client(&server.uri());
            let bs = client.balances().await.unwrap();
            if locked_usdt.is_none() {
                assert_eq!(bs.len(), 2);
                assert_eq!(bs[0].asset, "BTC");
                assert!((bs[0].free - 4_723_846.892_081).abs() < 1e-6);
                assert_eq!(bs[0].locked, 0.0);
                assert_eq!(bs[1].asset, "USDT");
                assert_eq!(bs[1].free, 100.5);
                assert_eq!(bs[1].locked, 2.25);
            } else {
                assert_eq!(bs.len(), 1);
                assert_eq!(bs[0].asset, "BTC");
                assert_eq!(bs[0].free, 1.0);
                assert_eq!(bs[0].locked, locked_usdt.unwrap());
            }
        }
    }

    #[tokio::test]
    async fn open_orders_parses_array_incl_zero_fill_new_order_and_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/openOrders"))
            .and(query_param("symbol", "BTCUSDT"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[{"symbol":"BTCUSDT","orderId":4293153,"orderListId":-1,"clientOrderId":"web_abc",
                "price":"40000.00000000","origQty":"0.01000000","executedQty":"0.00000000",
                "cummulativeQuoteQty":"0.00000000","status":"NEW","timeInForce":"GTC","type":"LIMIT",
                "side":"BUY","stopPrice":"0.00000000","icebergQty":"0.00000000","time":1499827319559,
                "updateTime":1499827319559,"isWorking":true,"origQuoteOrderQty":"0.40000000"}]"#,
            ))
            .mount(&server)
            .await;
        let client = signed_client(&server.uri());
        let os = client.open_orders("BTCUSDT").await.unwrap();
        assert_eq!(os.len(), 1);
        let o = &os[0];
        assert_eq!(o.order_id, 4_293_153);
        assert_eq!(o.client_order_id, "web_abc");
        assert_eq!(o.symbol, "BTCUSDT");
        assert_eq!(o.side, "BUY");
        assert_eq!(o.otype, "LIMIT");
        assert_eq!(o.price, 40_000.0);
        assert_eq!(o.orig_qty, 0.01);
        assert_eq!(o.executed_qty, 0.0);
        assert_eq!(o.status, "NEW");

        let empty_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/openOrders"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&empty_server)
            .await;
        let client = signed_client(&empty_server.uri());
        assert!(client.open_orders("ETHUSDT").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn my_trades_sends_limit_and_parses_rows() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/myTrades"))
            .and(query_param("symbol", "BTCUSDT"))
            .and(query_param("limit", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[{"symbol":"BTCUSDT","id":28457,"orderId":100234,"price":"4.00000100",
                "qty":"12.00000000","quoteQty":"48.000012","commission":"10.10000000",
                "commissionAsset":"BNB","time":1499865549590,"isBuyer":true,"isMaker":false,
                "isBestMatch":true}]"#,
            ))
            .mount(&server)
            .await;
        let client = signed_client(&server.uri());
        let ts = client.my_trades("BTCUSDT", 5).await.unwrap();
        assert_eq!(ts.len(), 1);
        let t = &ts[0];
        assert_eq!(t.id, 28_457);
        assert_eq!(t.order_id, 100_234);
        assert!((t.price - 4.000_001).abs() < 1e-9);
        assert_eq!(t.qty, 12.0);
        assert_eq!(t.commission, 10.1);
        assert_eq!(t.time, 1_499_865_549_590);
    }

    #[tokio::test]
    async fn symbol_filters_prefers_notional_falls_back_to_legacy_min_notional() {
        let modern = r#"{"timezone":"UTC","serverTime":1700000000000,"symbols":[{"symbol":"BTCUSDT",
            "status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT","filters":[
            {"filterType":"PRICE_FILTER","tickSize":"0.01000000"},
            {"filterType":"LOT_SIZE","minQty":"0.00010000","maxQty":"9000.00000000","stepSize":"0.00010000"},
            {"filterType":"NOTIONAL","minNotional":"10.00000000","applyMinToMarket":true,
             "maxNotional":null,"applyMaxToMarket":false,"avgPriceMins":5}]}]}"#;
        let legacy = r#"{"symbols":[{"symbol":"ETHBTC","status":"TRADING","filters":[
            {"filterType":"LOT_SIZE","minQty":"0.00100000","maxQty":"100.00000000","stepSize":"0.00100000"},
            {"filterType":"MIN_NOTIONAL","minNotional":"0.00010000","applyToMarket":true,
             "avgPriceMins":5}]}]}"#;
        for (body, symbol, step, min_qty, min_notional) in [
            (modern, "BTCUSDT", 0.0001, 0.0001, 10.0),
            (legacy, "ETHBTC", 0.001, 0.001, 0.0001),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v3/exchangeInfo"))
                .and(query_param("symbol", symbol))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server)
                .await;
            let f = SignedClient::symbol_filters(&server.uri(), symbol)
                .await
                .unwrap();
            assert_eq!(f.step_size, step);
            assert_eq!(f.min_qty, min_qty);
            assert_eq!(f.min_notional, min_notional);
        }
    }

    #[tokio::test]
    async fn symbol_filters_is_public_no_api_key_header_errors_on_unknown_symbol() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/exchangeInfo"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"symbols":[{"symbol":"BTCUSDT","filters":[
                    {"filterType":"LOT_SIZE","minQty":"0.00010000","stepSize":"0.00010000"},
                    {"filterType":"NOTIONAL","minNotional":"10.00000000"}]}]}"#,
            ))
            .mount(&server)
            .await;
        let f = SignedClient::symbol_filters(&server.uri(), "BTCUSDT")
            .await
            .unwrap();
        assert_eq!(f.min_notional, 10.0);
        let reqs = server.received_requests().await.unwrap();
        assert!(
            reqs[0].headers.get("X-MBX-APIKEY").is_none(),
            "exchangeInfo is public: must not send API key"
        );

        let missing = SignedClient::symbol_filters(&server.uri(), "DOGEUSDT").await;
        assert!(missing.is_err());
    }

    fn keys_err(result: anyhow::Result<Keys>) -> String {
        result.err().expect("expected Keys error").to_string()
    }

    fn signed_client(base: &str) -> SignedClient {
        SignedClient::new(
            base,
            Keys {
                api_key: API_KEY.to_string(),
                secret: SECRET.to_string(),
            },
        )
        .unwrap()
    }
}

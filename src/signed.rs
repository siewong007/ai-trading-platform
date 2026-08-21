use hmac::{Hmac, Mac};
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
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

use super::*;
use crate::signed::{fmt_price, Keys};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture_cfg() -> (StrategySection, BacktestSection) {
    let strat = StrategySection {
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
        entry_window_utc: None,
        breakout_lookback_bars: None,
    };
    let bt = BacktestSection {
        fee_rate: None,
        slippage: None,
        start_equity_usd: 200.0,
        risk_per_trade_usd: 2.0,
        max_notional_pct_equity: 0.5,
        min_notional_usd: 15.0,
    };
    (strat, bt)
}

fn candle(i: i64, open: f64, close: f64) -> Candle {
    Candle {
        open_time: i * 3_600_000,
        open,
        high: open.max(close) + 0.5,
        low: open.min(close) - 0.5,
        close,
        volume: 100.0,
    }
}

/// Series whose LAST CLOSED candle produces a signal (mirrors strategy tests).
fn signal_series() -> Vec<Candle> {
    let mut cs = Vec::new();
    let mut t = 0i64;
    for _ in 0..20 {
        cs.push(candle(t, 100.0, 100.0));
        t += 1;
    }
    let mut price = 100.0;
    for _ in 0..60 {
        let o = price;
        price += 0.1;
        cs.push(candle(t, o, price));
        t += 1;
    }
    for _ in 0..2 {
        let o = price;
        price -= 0.5;
        cs.push(candle(t, o, price));
        t += 1;
    }
    let o = price;
    cs.push(candle(t, o, price + 1.0)); // signal candle
    t += 1;
    cs.push(candle(t, price + 1.0, price + 1.0)); // open (unclosed) candle
    cs
}

async fn memory_db() -> Db {
    Db::open("sqlite::memory:").await.unwrap()
}

const ACCOUNT_JSON: &str = r#"{"canTrade":true,"balances":[{"asset":"USDT","free":"200.00000000","locked":"0.00000000"},{"asset":"TEST","free":"0.00000000","locked":"0.00000000"}]}"#;

const EXCHANGE_INFO_JSON: &str = r#"{"symbols":[{"symbol":"TESTUSDT","status":"TRADING","filters":[{"filterType":"LOT_SIZE","stepSize":"0.00100000","minQty":"0.00100000"},{"filterType":"NOTIONAL","minNotional":"5.00000000"}]}]}"#;

async fn mount_account_filters(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v3/account"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ACCOUNT_JSON))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/exchangeInfo"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EXCHANGE_INFO_JSON))
        .mount(server)
        .await;
}

#[tokio::test]
async fn stale_data_refuses_without_any_order_calls() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    let strat = fixture_cfg().0;
    let mut candles = signal_series();
    // age the series: last candle opened 10h before "now"
    let now = 1_700_000_000_000i64;
    let shift = now - 10 * 3_600_000 - candles.last().unwrap().open_time;
    for c in &mut candles {
        c.open_time += shift;
    }
    db.upsert_klines("TESTUSDT", "1h", &candles).await.unwrap();
    let ex = Executor {
        sc: SignedClient::new(
            &server.uri(),
            Keys {
                api_key: "k".into(),
                secret: "s".into(),
            },
        )
        .unwrap(),
        db,
        strat,
        bt: fixture_cfg().1,
        hash: "deadbeef1234".into(),
        dry_run: false,
    };
    let res = ex.run_cycle(now).await;
    assert!(res.is_err());
    assert!(res.unwrap_err().to_string().contains("stale"));
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn flat_series_yields_no_signal_and_no_orders() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    let (strat, bt) = fixture_cfg();
    let candles: Vec<Candle> = (0..30).map(|t| candle(t as i64, 100.0, 100.0)).collect();
    db.upsert_klines("TESTUSDT", "1h", &candles).await.unwrap();
    let ex = Executor {
        sc: SignedClient::new(
            &server.uri(),
            Keys {
                api_key: "k".into(),
                secret: "s".into(),
            },
        )
        .unwrap(),
        db,
        strat,
        bt,
        hash: "deadbeef1234".into(),
        dry_run: false,
    };
    let now = candles.last().unwrap().open_time + 3_600_000;
    assert_eq!(ex.run_cycle(now).await.unwrap(), CycleOutcome::NoSignal);
    let orders = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/api/v3/order" || r.url.path() == "/api/v3/order/oco")
        .count();
    assert_eq!(orders, 0);
}

#[tokio::test]
async fn fresh_signal_places_limit_with_deterministic_client_id() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    let (strat, bt) = fixture_cfg();
    let candles = signal_series();
    db.upsert_klines("TESTUSDT", "1h", &candles).await.unwrap();
    // prove the fixture actually signals on the last closed candle
    let sigs = generate_signals(&candles, &strat);
    assert!(
        sigs[candles.len() - 2].is_some(),
        "fixture must signal on last closed candle"
    );
    mount_account_filters(&server).await;
    let expected_cid = entry_client_id("deadbeef1234", last_closed_candle_time(&candles));
    let order_mock = Mock::given(method("POST"))
        .and(path("/api/v3/order"))
        .and(query_param("type", "LIMIT"))
        .and(query_param("timeInForce", "GTC"))
        .and(query_param("newClientOrderId", expected_cid.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "symbol":"TESTUSDT","orderId":42,"clientOrderId":"cid","status":"NEW",
            "executedQty":"0.00000000","price":"105.00000000","origQty":"1.00000000"
        })))
        .mount_as_scoped(&server)
        .await;
    let ex = Executor {
        sc: SignedClient::new(
            &server.uri(),
            Keys {
                api_key: "k".into(),
                secret: "s".into(),
            },
        )
        .unwrap(),
        db,
        strat,
        bt,
        hash: "deadbeef1234".into(),
        dry_run: false,
    };
    let now = candles.last().unwrap().open_time + 3_600_000;
    let out = match ex.run_cycle(now).await {
        Ok(o) => o,
        Err(e) => {
            let reqs = server.received_requests().await.unwrap();
            let paths: Vec<String> = reqs
                .iter()
                .map(|r| format!("{} {}", r.method, r.url))
                .collect();
            panic!("cycle failed: {e} — requests={paths:?}");
        }
    };
    match out {
        CycleOutcome::PlacedEntry { client_id } => {
            assert_eq!(client_id, expected_cid);
        }
        other => panic!("expected PlacedEntry, got {other:?}"),
    }
    let pos_json = ex.db.config_get("exec_pos_deadbeef1234").await.unwrap();
    assert!(pos_json.unwrap().contains("\"phase\":\"entry_open\""));
    drop(order_mock); // verify the scoped mock was hit
}

fn open_order_json(status: &str) -> serde_json::Value {
    serde_json::json!({
        "symbol":"TESTUSDT","orderId":42,"clientOrderId":"tp-deadbee-0","price":"105.00000000",
        "origQty":"1.00000000","executedQty":"0.00000000","status":status,"side":"BUY","type":"LIMIT"
    })
}

async fn exec_with(server: &MockServer, db: Db) -> Executor {
    let (strat, bt) = fixture_cfg();
    Executor {
        sc: SignedClient::new(
            &server.uri(),
            Keys {
                api_key: "k".into(),
                secret: "s".into(),
            },
        )
        .unwrap(),
        db,
        strat,
        bt,
        hash: "deadbeef1234".into(),
        dry_run: false,
    }
}

async fn seed_entry_pos(db: &Db, opened_ts: i64) {
    seed_pos_phase(db, ENTRY_PHASE, opened_ts).await;
}

async fn seed_pos_phase(db: &Db, phase: &str, opened_ts: i64) {
    let p = PosState {
        phase: phase.into(),
        symbol: "TESTUSDT".into(),
        entry_client_id: "tp-deadbee-0".into(),
        qty: 1.0,
        entry_price: 105.0,
        stop: 103.0,
        target: 108.0,
        opened_ts,
        entry_order_id: Some(42),
        oco_list_id: None,
    };
    db.config_set(
        &pos_key("deadbeef1234"),
        &serde_json::to_string(&p).unwrap(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn filled_entry_places_oco_with_exact_legs() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    seed_entry_pos(&db, 1_700_000_000_000).await;
    mount_account_filters(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v3/order"))
        .respond_with(ResponseTemplate::new(200).set_body_json(open_order_json("FILLED")))
        .mount(&server)
        .await;
    let oco_mock = Mock::given(method("POST"))
        .and(path("/api/v3/order/oco"))
        .and(query_param("aboveType", "LIMIT_MAKER"))
        .and(query_param("abovePrice", fmt_price(108.0).as_str()))
        .and(query_param("belowStopPrice", fmt_price(103.0).as_str()))
        .and(query_param("belowPrice", fmt_price(103.0 * 0.995).as_str()))
        .and(query_param("belowType", "STOP_LOSS_LIMIT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "orderListId": 777
        })))
        .mount_as_scoped(&server)
        .await;
    let ex = exec_with(&server, db).await;
    let out = ex.run_cycle(1_700_003_600_000).await.unwrap();
    match out {
        CycleOutcome::PlacedOco { list_id } => assert_eq!(list_id, "777"),
        other => panic!("expected PlacedOco, got {other:?}"),
    }
    drop(oco_mock);
}

#[tokio::test]
async fn unfilled_entry_reports_awaiting_fill() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    seed_entry_pos(&db, 1_700_000_000_000).await;
    Mock::given(method("GET"))
        .and(path("/api/v3/order"))
        .respond_with(ResponseTemplate::new(200).set_body_json(open_order_json("NEW")))
        .mount(&server)
        .await;
    let ex = exec_with(&server, db).await;
    assert_eq!(
        ex.run_cycle(1_700_003_600_000).await.unwrap(),
        CycleOutcome::AwaitingFill
    );
}

#[tokio::test]
async fn oco_gone_and_exit_fills_register_stopout_and_clear_pos() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    seed_pos_phase(&db, OCO_PHASE, 1_700_000_000_000).await;
    db.config_set(
        "day_state_deadbeef1234",
        &serde_json::to_string(&crate::risk::DayState {
            day_key: "day-19672".into(),
            day_start_equity: 200.0,
            consecutive_stopouts: 0,
            halted: false,
            halt_reason: None,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    // exit fill at 102.9 (< stop*1.001 => stopout)
    Mock::given(method("GET"))
        .and(path("/api/v3/myTrades"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":1,"orderId":42,"price":"105.0","qty":"1.0","commission":"0.105","time":1700000100000u64},
            {"id":2,"orderId":99,"price":"102.9","qty":"1.0","commission":"0.103","time":1700002000000u64}
        ])))
        .mount(&server)
        .await;
    let ex = exec_with(&server, db).await;
    assert_eq!(
        ex.run_cycle(1_700_003_600_000).await.unwrap(),
        CycleOutcome::Flat
    );
    assert!(ex
        .db
        .config_get("exec_pos_deadbeef1234")
        .await
        .unwrap()
        .is_none());
    let day_raw = ex
        .db
        .config_get("day_state_deadbeef1234")
        .await
        .unwrap()
        .unwrap();
    let day: crate::risk::DayState = serde_json::from_str(&day_raw).unwrap();
    assert_eq!(day.consecutive_stopouts, 1);
}

#[tokio::test]
async fn live_oco_reports_position_live() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    seed_pos_phase(&db, OCO_PHASE, 1_700_000_000_000).await;
    let mut leg = open_order_json("NEW");
    leg["type"] = serde_json::json!("STOP_LOSS_LIMIT");
    leg["side"] = serde_json::json!("SELL");
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([leg])))
        .mount(&server)
        .await;
    let ex = exec_with(&server, db).await;
    assert_eq!(
        ex.run_cycle(1_700_003_600_000).await.unwrap(),
        CycleOutcome::PositionLive
    );
}

#[tokio::test]
async fn reconcile_cancels_orphan_entry_without_replacing_it() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    // DB thinks we are flat; exchange has an orphan BUY entry
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "symbol":"TESTUSDT","orderId":42,"clientOrderId":"tp-deadbee-1700000000000","price":"105.00000000",
            "origQty":"1.00000000","executedQty":"0.00000000","status":"NEW","side":"BUY","type":"LIMIT"
        }])))
        .mount(&server)
        .await;
    let cancel_mock = Mock::given(method("DELETE"))
        .and(path("/api/v3/order"))
        .and(query_param("origClientOrderId", "tp-deadbee-1700000000000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "symbol":"TESTUSDT","orderId":42,"status":"CANCELED"
        })))
        .mount_as_scoped(&server)
        .await;
    mount_account_filters(&server).await;
    // no POST /api/v3/order may happen (no re-entry during reconcile)
    Mock::given(method("POST"))
        .and(path("/api/v3/order"))
        .respond_with(ResponseTemplate::new(500).set_body_string("must not place orders"))
        .expect(0)
        .mount(&server)
        .await;
    let ex = exec_with(&server, db).await;
    let summary = ex.reconcile().await.unwrap();
    assert!(summary.contains("orphan"), "summary: {summary}");
    drop(cancel_mock);
}

#[tokio::test]
async fn reconcile_places_oco_for_filled_unprotected_entry() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    seed_entry_pos(&db, 1_700_000_000_000).await;
    // no open orders on exchange (entry filled, OCO never placed)
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    mount_account_filters(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v3/order"))
        .respond_with(ResponseTemplate::new(200).set_body_json(open_order_json("FILLED")))
        .mount(&server)
        .await;
    let oco_mock = Mock::given(method("POST"))
        .and(path("/api/v3/order/oco"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"orderListId": 888})),
        )
        .mount_as_scoped(&server)
        .await;
    let ex = exec_with(&server, db).await;
    let summary = ex.reconcile().await.unwrap();
    assert!(summary.contains("OCO placed"), "summary: {summary}");
    drop(oco_mock);
}

#[tokio::test]
async fn flatten_cancels_confirms_then_market_sells() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "symbol":"TESTUSDT","orderId":7,"clientOrderId":"tp-x","price":"108.0",
            "origQty":"1.0","executedQty":"0.0","status":"NEW","side":"SELL","type":"LIMIT_MAKER"
        }])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/account"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"canTrade":true,"balances":[{"asset":"USDT","free":"200.00000000","locked":"0.00000000"},{"asset":"TEST","free":"2.50000000","locked":"0.00000000"}]}"#,
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/account"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"canTrade":true,"balances":[{"asset":"USDT","free":"200.00000000","locked":"0.00000000"},{"asset":"TEST","free":"0.00000000","locked":"0.00000000"}]}"#,
        ))
        .mount(&server)
        .await;
    // exchangeInfo for filters
    Mock::given(method("GET"))
        .and(path("/api/v3/exchangeInfo"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EXCHANGE_INFO_JSON))
        .mount(&server)
        .await;
    // TEST balance free 2.5 -> market sell expected
    Mock::given(method("POST"))
        .and(path("/api/v3/order"))
        .and(query_param("type", "MARKET"))
        .and(query_param("side", "SELL"))
        .and(query_param("quantity", "2.5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "symbol":"TESTUSDT","orderId":9,"clientOrderId":"tp-flat-1","status":"FILLED","executedQty":"2.50000000"
        })))
        .mount(&server)
        .await;
    let ex = exec_with(&server, db).await;
    let summary = match ex.flatten_all().await {
        Ok(s) => s,
        Err(e) => {
            let reqs = server.received_requests().await.unwrap();
            let paths: Vec<String> = reqs
                .iter()
                .map(|r| format!("{} {}", r.method, r.url))
                .collect();
            panic!("flatten failed: {e} — requests={paths:?}");
        }
    };
    assert!(
        summary.contains("market-sold 2.5 TEST"),
        "summary: {summary}"
    );
    // ORDERING: cancel-all DELETE must precede the market SELL POST
    let reqs = server.received_requests().await.unwrap();
    let del_idx = reqs
        .iter()
        .position(|r| r.method == "DELETE" && r.url.path() == "/api/v3/openOrders")
        .expect("cancel-all happened");
    let sell_idx = reqs
        .iter()
        .position(|r| {
            r.method == "POST"
                && r.url.path() == "/api/v3/order"
                && r.url.query().unwrap_or("").contains("type=MARKET")
        })
        .expect("market sell happened");
    assert!(del_idx < sell_idx, "cancel must precede market sell");
}

#[tokio::test]
async fn flatten_with_nothing_open_is_a_clean_noop() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    mount_account_filters(&server).await;
    let ex = exec_with(&server, db).await;
    let summary = ex.flatten_all().await.unwrap();
    assert!(summary.contains("no TEST balance"), "summary: {summary}");
    assert!(summary.contains("halted"));
}

#[tokio::test]
async fn reconcile_preserves_tracked_fresh_pending_entry() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    seed_entry_pos(&db, 1_700_000_000_000).await; // pos tracks entry tp-deadbee-0
                                                  // exchange still shows that NEW entry order
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "symbol":"TESTUSDT","orderId":42,"clientOrderId":"tp-deadbee-0","price":"105.00000000",
            "origQty":"1.00000000","executedQty":"0.00000000","status":"NEW","side":"BUY","type":"LIMIT"
        }])))
        .mount(&server)
        .await;
    mount_account_filters(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v3/order"))
        .respond_with(ResponseTemplate::new(200).set_body_json(open_order_json("NEW")))
        .mount(&server)
        .await;
    // no DELETE may occur — the entry is tracked and fresh
    Mock::given(method("DELETE"))
        .and(path("/api/v3/order"))
        .respond_with(ResponseTemplate::new(500).set_body_string("must not cancel tracked"))
        .expect(0)
        .mount(&server)
        .await;
    let ex = exec_with(&server, db).await;
    let summary = ex.reconcile().await.unwrap();
    assert!(summary.contains("reconciled"), "summary: {summary}");
}

#[tokio::test]
async fn reconcile_cancels_partial_orphan_and_sells_acquired_base() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "symbol":"TESTUSDT","orderId":55,"clientOrderId":"tp-deadbee-1700000000000","price":"105.00000000",
            "origQty":"2.00000000","executedQty":"0.50000000","status":"PARTIALLY_FILLED","side":"BUY","type":"LIMIT"
        }])))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v3/order"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    mount_account_filters(&server).await; // step 0.001, minQty 0.001
    let sell_mock = Mock::given(method("POST"))
        .and(path("/api/v3/order"))
        .and(query_param("type", "MARKET"))
        .and(query_param("quantity", "0.5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "symbol":"TESTUSDT","orderId":56,"clientOrderId":"tp-flat-1","status":"FILLED","executedQty":"0.50000000"
        })))
        .mount_as_scoped(&server)
        .await;
    let ex = exec_with(&server, db).await;
    let summary = ex.reconcile().await.unwrap();
    assert!(summary.contains("orphan"), "summary: {summary}");
    assert!(summary.contains("market-sold 0.5"), "summary: {summary}");
    drop(sell_mock);
}

#[tokio::test]
async fn reconcile_keeps_fresh_untracked_entry_for_its_own_cycle() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    // candle time embedded = recent relative to SystemTime::now (offset small)
    let fresh_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        - 3_600_000;
    mount_account_filters(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "symbol":"TESTUSDT","orderId":60,"clientOrderId":format!("tp-fresh11-{fresh_ts}"),"price":"105.00000000",
            "origQty":"1.00000000","executedQty":"0.00000000","status":"NEW","side":"BUY","type":"LIMIT"
        }])))
        .mount(&server)
        .await;
    let ex = exec_with(&server, db).await;
    let summary = ex.reconcile().await.unwrap();
    assert!(summary.contains("kept fresh entry"), "summary: {summary}");
}

#[tokio::test]
async fn dry_run_never_places_recovery_oco() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    seed_pos_phase(&db, OCO_PHASE, 1_700_000_000_000).await;
    // no tp-* orders (OCO vanished), no exits yet
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/myTrades"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    mount_account_filters(&server).await;
    // any POST /order/oco would be a real order — forbid it
    Mock::given(method("POST"))
        .and(path("/api/v3/order/oco"))
        .respond_with(ResponseTemplate::new(500).set_body_string("dry-run must not place"))
        .expect(0)
        .mount(&server)
        .await;
    let mut ex = exec_with(&server, db).await;
    ex.dry_run = true;
    assert_eq!(
        ex.run_cycle(1_700_003_600_000).await.unwrap(),
        CycleOutcome::PositionLive
    );
}

#[tokio::test]
async fn protection_missing_re_arms_oco_for_remaining_size() {
    let server = MockServer::start().await;
    let db = memory_db().await;
    seed_pos_phase(&db, OCO_PHASE, 1_700_000_000_000).await;
    // pos qty is 1.0; exchange shows NO tp-* orders; myTrades shows a partial
    // manual exit of 0.4 -> remaining 0.6 must get a fresh protective OCO
    Mock::given(method("GET"))
        .and(path("/api/v3/openOrders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/myTrades"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":1,"orderId":42,"price":"105.0","qty":"0.6","commission":"0.06","time":1700000100000u64},
            {"id":2,"orderId":77,"price":"104.0","qty":"0.4","commission":"0.04","time":1700001000000u64}
        ])))
        .mount(&server)
        .await;
    mount_account_filters(&server).await;
    let oco_mock = Mock::given(method("POST"))
        .and(path("/api/v3/order/oco"))
        .and(query_param("quantity", "0.6"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"orderListId": 999})),
        )
        .mount_as_scoped(&server)
        .await;
    let ex = exec_with(&server, db).await;
    match ex.run_cycle(1_700_003_600_000).await.unwrap() {
        CycleOutcome::PlacedOco { list_id } => assert_eq!(list_id, "999"),
        other => panic!("expected re-arm PlacedOco, got {other:?}"),
    }
    drop(oco_mock);
}

#![allow(dead_code)] // live paths wire into the trade CLI (Task 6)

use crate::db::Db;
use crate::risk::{load_day_state, register_result, risk_pass, roll_day_if_new, store_day_state};
use crate::signed::{PlacedOrder, SignedClient, SymbolFilters};
use crate::strategy::{generate_signals, BacktestSection, StrategySection};
use crate::types::Candle;
use serde::{Deserialize, Serialize};

pub struct Executor {
    pub sc: SignedClient,
    pub db: Db,
    pub strat: StrategySection,
    pub bt: BacktestSection,
    pub hash: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CycleOutcome {
    NoSignal,
    PlacedEntry { client_id: String },
    AwaitingFill,
    PlacedOco { list_id: String },
    PositionLive,
    Flat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PosState {
    phase: String,
    symbol: String,
    entry_client_id: String,
    qty: f64,
    entry_price: f64,
    stop: f64,
    target: f64,
    opened_ts: i64,
    entry_order_id: Option<i64>,
    oco_list_id: Option<String>,
}

fn pos_key(hash: &str) -> String {
    format!("exec_pos_{hash}")
}

pub fn last_closed_candle_time(candles: &[Candle]) -> i64 {
    candles[candles.len() - 2].open_time
}

pub fn is_stale(last_open_time: i64, now_ms: i64, timeframe_ms: i64) -> bool {
    if timeframe_ms <= 0 || now_ms < last_open_time {
        return true;
    }
    (now_ms - last_open_time) / timeframe_ms > 2
}

pub fn entry_client_id(hash: &str, candle_open_time: i64) -> String {
    format!("tp-{}-{}", &hash[..8], candle_open_time)
}

pub fn timeframe_ms(tf: &str) -> i64 {
    let (num, unit) = tf.split_at(tf.len() - 1);
    let n: i64 = num.parse().unwrap_or(0);
    match unit {
        "m" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        _ => 0,
    }
}

const ENTRY_PHASE: &str = "entry_open";
const OCO_PHASE: &str = "oco_live";

impl Executor {
    async fn load_pos(&self) -> anyhow::Result<Option<PosState>> {
        match self.db.config_get(&pos_key(&self.hash)).await? {
            Some(json) if !json.is_empty() => Ok(Some(serde_json::from_str(&json)?)),
            _ => Ok(None),
        }
    }

    async fn store_pos(&self, p: &PosState) -> anyhow::Result<()> {
        self.db
            .config_set(&pos_key(&self.hash), &serde_json::to_string(p)?)
            .await
    }

    async fn clear_pos(&self) -> anyhow::Result<()> {
        self.db.config_del(&pos_key(&self.hash)).await
    }

    fn free_usdt(balances: &[crate::signed::Balance]) -> f64 {
        balances
            .iter()
            .find(|b| b.asset == "USDT")
            .map(|b| b.free)
            .unwrap_or(0.0)
    }

    async fn filters(&self, symbol: &str) -> anyhow::Result<SymbolFilters> {
        SignedClient::symbol_filters(self.sc.base(), symbol).await
    }

    pub async fn run_cycle(&self, now_ms: i64) -> anyhow::Result<CycleOutcome> {
        let tf = timeframe_ms(&self.strat.timeframe);
        if let Some(pos) = self.load_pos().await? {
            return self.manage_open_position(&pos).await;
        }
        for pair in &self.strat.pairs {
            let candles = self.db.load_klines(pair, &self.strat.timeframe).await?;
            if candles.len() < self.strat.ema_slow + 2 {
                continue;
            }
            if is_stale(candles.last().unwrap().open_time, now_ms, tf) {
                anyhow::bail!("stale data on {pair}: refusing to act (>2 missed candles)");
            }
            let sigs = generate_signals(&candles, &self.strat);
            let closed_idx = candles.len() - 2;
            let Some(plan) = sigs[closed_idx] else {
                continue;
            };
            let balances = self.sc.balances().await?;
            let equity = Self::free_usdt(&balances);
            let mut day = load_day_state(&self.db, &self.hash).await?;
            let equity_ref = equity.max(day.day_start_equity);
            roll_day_if_new(&mut day, &chrono_day(now_ms), equity_ref);
            let filters = self.filters(pair).await?;
            match risk_pass(&plan, equity, false, &self.bt, &filters, &day, false) {
                crate::risk::RiskDecision::Place { qty, entry_limit } => {
                    let cid = entry_client_id(&self.hash, last_closed_candle_time(&candles));
                    if self.dry_run {
                        tracing::info!("DRY-RUN intent: buy {qty} {pair} @ {entry_limit} id={cid}");
                        return Ok(CycleOutcome::PlacedEntry { client_id: cid });
                    }
                    let placed: PlacedOrder = self
                        .sc
                        .place_limit_buy(pair, qty, entry_limit, &cid)
                        .await?;
                    self.store_pos(&PosState {
                        phase: ENTRY_PHASE.to_string(),
                        symbol: pair.clone(),
                        entry_client_id: cid.clone(),
                        qty,
                        entry_price: entry_limit,
                        stop: plan.stop,
                        target: plan.target,
                        opened_ts: now_ms,
                        entry_order_id: Some(placed.order_id),
                        oco_list_id: None,
                    })
                    .await?;
                    return Ok(CycleOutcome::PlacedEntry { client_id: cid });
                }
                crate::risk::RiskDecision::Skip(reason) => {
                    tracing::info!("{pair}: skip — {reason}");
                    continue;
                }
                crate::risk::RiskDecision::Halt(reason) => {
                    day.halted = true;
                    day.halt_reason = Some(reason.clone());
                    store_day_state(&self.db, &self.hash, &day).await?;
                    tracing::warn!("HALT: {reason} — no entries until manual reset");
                    return Ok(CycleOutcome::Flat);
                }
            }
        }
        Ok(CycleOutcome::NoSignal)
    }

    async fn manage_open_position(&self, pos: &PosState) -> anyhow::Result<CycleOutcome> {
        let pair = pos.symbol.clone();
        match pos.phase.as_str() {
            ENTRY_PHASE => {
                let order = self.sc.get_order(&pair, &pos.entry_client_id).await?;
                if order.status == "FILLED" {
                    let list_id = self
                        .sc
                        .place_oco_sell(&pair, pos.qty, pos.target, pos.stop, &pos.entry_client_id)
                        .await?;
                    let mut p = pos.clone();
                    p.phase = OCO_PHASE.to_string();
                    p.oco_list_id = Some(list_id.clone());
                    self.store_pos(&p).await?;
                    Ok(CycleOutcome::PlacedOco { list_id })
                } else {
                    Ok(CycleOutcome::AwaitingFill)
                }
            }
            _ => {
                let open = self.sc.open_orders(&pair).await?;
                if open.iter().any(|o| o.client_order_id.starts_with("tp-")) {
                    return Ok(CycleOutcome::PositionLive);
                }
                let trades = self.sc.my_trades(&pair, 10).await?;
                let exits: Vec<&crate::signed::MyTrade> = trades
                    .iter()
                    .filter(|t| t.time >= pos.opened_ts)
                    .filter(|t| Some(t.order_id) != pos.entry_order_id)
                    .collect();
                let exit_qty: f64 = exits.iter().map(|t| t.qty).sum();
                if exit_qty + 1e-9 < pos.qty {
                    tracing::warn!("{pair}: position state unclear (exit_qty {exit_qty})");
                    return Ok(CycleOutcome::PositionLive);
                }
                let exit_notional: f64 = exits.iter().map(|t| t.price * t.qty).sum();
                let fees: f64 = exits.iter().map(|t| t.commission).sum();
                let avg_exit = exit_notional / exit_qty;
                let pnl = (avg_exit - pos.entry_price) * pos.qty - fees;
                let was_stopout = avg_exit <= pos.stop * 1.001;
                let mut day = load_day_state(&self.db, &self.hash).await?;
                register_result(&mut day, pnl, was_stopout);
                store_day_state(&self.db, &self.hash, &day).await?;
                self.clear_pos().await?;
                tracing::info!("{pair}: position closed pnl={pnl:.4} stopout={was_stopout}");
                Ok(CycleOutcome::Flat)
            }
        }
    }
}

fn chrono_day(now_ms: i64) -> String {
    let secs = now_ms / 1000;
    let days = secs.div_euclid(86_400);
    format!("day-{days}")
}

#[cfg(test)]
mod tests;

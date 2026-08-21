// Entirely consumed by the Phase 2 executor (Task 5); until then every item
// here is dead code from the binary crate's perspective.
#![allow(dead_code)]

use crate::db::Db;
use crate::signed::{round_qty_to_step, SymbolFilters};
use crate::strategy::{BacktestSection, TradePlan};
use serde::{Deserialize, Serialize};

const DAILY_HALT_LOSS: f64 = -0.02;
const FLATTEN_LOSS: f64 = -0.03;
const DAILY_HALT_REASON: &str = "daily halt";
pub const FLATTEN_REASON: &str = "flatten required";
const POSITION_OPEN_REASON: &str = "position open";
const DEGENERATE_PLAN_REASON: &str = "degenerate plan";
const BELOW_MIN_NOTIONAL_REASON: &str = "below min notional";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayState {
    pub day_key: String,
    pub day_start_equity: f64,
    pub consecutive_stopouts: u32,
    pub halted: bool,
    pub halt_reason: Option<String>,
}

impl Default for DayState {
    fn default() -> Self {
        Self {
            day_key: String::new(),
            day_start_equity: 0.0,
            consecutive_stopouts: 0,
            halted: false,
            halt_reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RiskDecision {
    Place { qty: f64, entry_limit: f64 },
    Skip(String),
    Halt(String),
}

pub async fn load_day_state(db: &Db, cfg_hash: &str) -> anyhow::Result<DayState> {
    match db.config_get(&format!("day_state_{cfg_hash}")).await? {
        Some(json) => Ok(serde_json::from_str(&json)?),
        None => Ok(DayState::default()),
    }
}

pub async fn store_day_state(db: &Db, cfg_hash: &str, s: &DayState) -> anyhow::Result<()> {
    db.config_set(&format!("day_state_{cfg_hash}"), &serde_json::to_string(s)?)
        .await
}

pub fn roll_day_if_new(s: &mut DayState, now_utc_date: &str, current_equity: f64) {
    if s.day_key == now_utc_date {
        return;
    }
    s.day_key = now_utc_date.to_string();
    s.day_start_equity = current_equity;
    s.consecutive_stopouts = 0;
    // daily halt clears at rollover; flatten-required waits for a manual reset
    if s.halted && s.halt_reason.as_deref() != Some(FLATTEN_REASON) {
        s.halted = false;
        s.halt_reason = None;
    }
}

pub fn risk_pass(
    plan: &TradePlan,
    equity: f64,
    has_open_position: bool,
    bt: &BacktestSection,
    filters: &SymbolFilters,
    day: &DayState,
    last_trade_was_stopout: bool,
) -> RiskDecision {
    if day.halted {
        return RiskDecision::Halt(
            day.halt_reason
                .clone()
                .unwrap_or_else(|| DAILY_HALT_REASON.into()),
        );
    }
    let day_loss = if day.day_start_equity > 0.0 {
        (equity - day.day_start_equity) / day.day_start_equity
    } else {
        0.0
    };
    let stopouts = day.consecutive_stopouts + u32::from(last_trade_was_stopout);
    if day_loss <= FLATTEN_LOSS {
        return RiskDecision::Halt(FLATTEN_REASON.into());
    }
    if day_loss <= DAILY_HALT_LOSS || stopouts >= 2 {
        return RiskDecision::Halt(DAILY_HALT_REASON.into());
    }
    if has_open_position {
        return RiskDecision::Skip(POSITION_OPEN_REASON.into());
    }
    if plan.stop >= plan.entry || plan.entry <= 0.0 {
        return RiskDecision::Skip(DEGENERATE_PLAN_REASON.into());
    }
    let mut qty = round_qty_to_step(
        bt.risk_per_trade_usd / (plan.entry - plan.stop),
        filters.step_size,
    );
    let cap_notional = bt.max_notional_pct_equity * equity;
    if qty * plan.entry > cap_notional {
        qty = cap_notional / plan.entry;
    }
    if qty * plan.entry < bt.min_notional_usd {
        return RiskDecision::Skip(BELOW_MIN_NOTIONAL_REASON.into());
    }
    if qty <= 0.0 {
        return RiskDecision::Skip(DEGENERATE_PLAN_REASON.into());
    }
    RiskDecision::Place {
        qty,
        entry_limit: plan.entry,
    }
}

pub fn register_result(day: &mut DayState, _pnl: f64, was_stopout: bool) {
    if was_stopout {
        day.consecutive_stopouts += 1;
    } else {
        day.consecutive_stopouts = 0;
    }
    if day.consecutive_stopouts >= 2 && !day.halted {
        day.halted = true;
        day.halt_reason = Some(DAILY_HALT_REASON.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bt() -> BacktestSection {
        BacktestSection {
            start_equity_usd: 200.0,
            risk_per_trade_usd: 2.0,
            max_notional_pct_equity: 0.5,
            min_notional_usd: 15.0,
        }
    }

    fn filters(step: f64) -> SymbolFilters {
        SymbolFilters {
            step_size: step,
            min_qty: 0.0,
            min_notional: 10.0,
        }
    }

    fn plan(entry: f64, stop: f64) -> TradePlan {
        TradePlan {
            entry,
            stop,
            target: entry + 3.0 * (entry - stop),
        }
    }

    fn day(start_equity: f64) -> DayState {
        DayState {
            day_key: "2026-08-22".into(),
            day_start_equity: start_equity,
            consecutive_stopouts: 0,
            halted: false,
            halt_reason: None,
        }
    }

    #[test]
    fn places_at_exactly_the_notional_cap() {
        // risk $2 / (100-98) = qty 1.0; notional 100 == cap 0.5*200 → allowed
        let d = risk_pass(
            &plan(100.0, 98.0),
            200.0,
            false,
            &bt(),
            &filters(0.5),
            &day(200.0),
            false,
        );
        assert_eq!(
            d,
            RiskDecision::Place {
                qty: 1.0,
                entry_limit: 100.0
            }
        );
    }

    #[test]
    fn shrinks_qty_when_notional_would_exceed_cap() {
        // entry=101 stop=99 → qty 1.0, notional 101 > cap 100 → shrink to 100/101
        let d = risk_pass(
            &plan(101.0, 99.0),
            200.0,
            false,
            &bt(),
            &filters(0.5),
            &day(200.0),
            false,
        );
        match d {
            RiskDecision::Place { qty, entry_limit } => {
                assert_eq!(entry_limit, 101.0);
                assert!(qty <= 100.0 / 101.0);
                assert!(
                    (qty * 101.0 - 100.0).abs() < 1e-6,
                    "shrunk notional off cap: {qty}"
                );
            }
            other => panic!("expected Place, got {other:?}"),
        }
    }

    #[test]
    fn skips_below_min_notional_on_wide_stop() {
        // risk $2 / (100-80) = qty 0.1; notional 10 < $15 → Skip
        let d = risk_pass(
            &plan(100.0, 80.0),
            200.0,
            false,
            &bt(),
            &filters(0.01),
            &day(200.0),
            false,
        );
        assert_eq!(d, RiskDecision::Skip("below min notional".into()));
    }

    #[test]
    fn ok_at_1_95_percent_day_loss() {
        // 196.1 vs 200 start = −1.95% → trade allowed
        let d = risk_pass(
            &plan(100.0, 98.0),
            196.1,
            false,
            &bt(),
            &filters(0.5),
            &day(200.0),
            false,
        );
        assert!(
            matches!(
                d,
                RiskDecision::Place {
                    entry_limit: 100.0,
                    ..
                }
            ),
            "{d:?}"
        );
    }

    #[test]
    fn halts_at_beyond_2_percent_day_loss() {
        // 195.9 vs 200 start = −2.05% → daily halt
        let d = risk_pass(
            &plan(100.0, 98.0),
            195.9,
            false,
            &bt(),
            &filters(0.5),
            &day(200.0),
            false,
        );
        assert_eq!(d, RiskDecision::Halt("daily halt".into()));
    }

    #[test]
    fn halts_after_two_registered_consecutive_stopouts() {
        let mut s = day(200.0);
        s.consecutive_stopouts = 2;
        let d = risk_pass(
            &plan(100.0, 98.0),
            200.0,
            false,
            &bt(),
            &filters(0.5),
            &s,
            false,
        );
        assert_eq!(d, RiskDecision::Halt("daily halt".into()));
    }

    #[test]
    fn unregistered_last_stopout_counts_toward_daily_halt() {
        // one registered stopout + last trade just stopped out → 2 total → halt
        let mut s = day(200.0);
        s.consecutive_stopouts = 1;
        let d = risk_pass(
            &plan(100.0, 98.0),
            200.0,
            false,
            &bt(),
            &filters(0.5),
            &s,
            true,
        );
        assert_eq!(d, RiskDecision::Halt("daily halt".into()));
    }

    #[test]
    fn flatten_required_at_3_percent_intraday_loss() {
        // 194 vs 200 = −3% → flatten-all halt until manual reset
        let d = risk_pass(
            &plan(100.0, 98.0),
            194.0,
            false,
            &bt(),
            &filters(0.5),
            &day(200.0),
            false,
        );
        assert_eq!(d, RiskDecision::Halt("flatten required".into()));
    }

    #[test]
    fn halted_day_blocks_with_stored_reason_even_if_equity_recovered() {
        let mut s = day(200.0);
        s.halted = true;
        s.halt_reason = Some("daily halt".into());
        let d = risk_pass(
            &plan(100.0, 98.0),
            250.0,
            false,
            &bt(),
            &filters(0.5),
            &s,
            false,
        );
        assert_eq!(d, RiskDecision::Halt("daily halt".into()));
    }

    #[test]
    fn open_position_blocks_new_entry() {
        let d = risk_pass(
            &plan(100.0, 98.0),
            200.0,
            true,
            &bt(),
            &filters(0.5),
            &day(200.0),
            false,
        );
        assert_eq!(d, RiskDecision::Skip("position open".into()));
    }

    #[test]
    fn degenerate_plan_is_skipped() {
        let d = risk_pass(
            &plan(100.0, 100.0),
            200.0,
            false,
            &bt(),
            &filters(0.5),
            &day(200.0),
            false,
        );
        assert_eq!(d, RiskDecision::Skip("degenerate plan".into()));

        let d = risk_pass(
            &plan(100.0, 102.0),
            200.0,
            false,
            &bt(),
            &filters(0.5),
            &day(200.0),
            false,
        );
        assert_eq!(d, RiskDecision::Skip("degenerate plan".into()));
    }

    #[test]
    fn qty_floors_to_exchange_step() {
        // risk $2 / (200-183.8) = 0.12345… → floored to step 0.001 → 0.123
        let d = risk_pass(
            &plan(200.0, 183.8),
            200.0,
            false,
            &bt(),
            &filters(0.001),
            &day(200.0),
            false,
        );
        assert_eq!(
            d,
            RiskDecision::Place {
                qty: 0.123,
                entry_limit: 200.0
            }
        );
    }

    #[test]
    fn register_result_tracks_consecutive_stopouts_and_halts_at_two() {
        let mut s = day(200.0);
        register_result(&mut s, -2.0, true);
        assert_eq!(s.consecutive_stopouts, 1);
        assert!(!s.halted);

        register_result(&mut s, 1.5, false); // winner resets the streak
        assert_eq!(s.consecutive_stopouts, 0);

        register_result(&mut s, -2.0, true);
        register_result(&mut s, -2.1, true);
        assert_eq!(s.consecutive_stopouts, 2);
        assert!(s.halted);
        assert_eq!(s.halt_reason.as_deref(), Some("daily halt"));
    }

    #[test]
    fn roll_day_if_new_resets_counters_and_clears_daily_halt() {
        let mut s = day(200.0);
        s.consecutive_stopouts = 2;
        s.halted = true;
        s.halt_reason = Some("daily halt".into());
        roll_day_if_new(&mut s, "2026-08-23", 205.0);
        assert_eq!(s.day_key, "2026-08-23");
        assert_eq!(s.day_start_equity, 205.0);
        assert_eq!(s.consecutive_stopouts, 0);
        assert!(!s.halted);
        assert_eq!(s.halt_reason, None);
    }

    #[test]
    fn roll_day_keeps_same_day_untouched_but_preserves_flatten_halt() {
        let mut s = day(200.0);
        s.consecutive_stopouts = 1;
        roll_day_if_new(&mut s, "2026-08-22", 190.0);
        assert_eq!(s.day_start_equity, 200.0, "same day key: state untouched");
        assert_eq!(s.consecutive_stopouts, 1);

        // −3% flatten halt persists across midnight until manual reset
        let mut f = day(200.0);
        f.halted = true;
        f.halt_reason = Some(FLATTEN_REASON.into());
        roll_day_if_new(&mut f, "2026-08-23", 190.0);
        assert_eq!(f.day_key, "2026-08-23");
        assert_eq!(f.day_start_equity, 190.0);
        assert!(f.halted, "flatten-required must survive rollover");
        assert_eq!(f.halt_reason.as_deref(), Some("flatten required"));
    }

    #[tokio::test]
    async fn day_state_roundtrips_through_config_state() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let fresh = load_day_state(&db, "h1").await.unwrap();
        assert_eq!(fresh, DayState::default());

        let mut s = day(200.0);
        s.consecutive_stopouts = 1;
        s.halted = true;
        s.halt_reason = Some(DAILY_HALT_REASON.into());
        store_day_state(&db, "h1", &s).await.unwrap();

        let back = load_day_state(&db, "h1").await.unwrap();
        assert_eq!(back, s);

        let other = load_day_state(&db, "h2").await.unwrap();
        assert_ne!(other, s, "states are keyed per config hash");
    }
}

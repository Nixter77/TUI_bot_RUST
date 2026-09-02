//! UTC daily loss halt (USDT and/or R). `r` does not lift it; the next UTC day does.
//!
//! 1R USDT = `day_start_equity × risk_pct` (same risk unit as S4 position sizing).
//! **OR layers** (not max): day equity PnL ≤ −DAILY_LOSS_USDT **or**
//! ≤ −(DAILY_LOSS_R × 1R) trips `daily_halt`.
//! `DAILY_LOSS_R=0` or `RISK_PCT=0` disables the R layer only.

use crate::models::EngineState;
use crate::sessions::utc_datetime;
use rust_decimal::Decimal;

pub fn default_daily_loss_usdt() -> Decimal {
    Decimal::from(20)
}

/// Default daily R budget. Account-scaled; sits on top of `DAILY_LOSS_USDT`.
pub fn default_daily_loss_r() -> Decimal {
    Decimal::from(3)
}

pub fn utc_day_key(now: f64) -> String {
    utc_datetime(now).format("%Y-%m-%d").to_string()
}

/// 1R in USDT from day-start equity and RISK_PCT. `None` when R layer is off.
pub fn one_r_usdt(start_equity: Decimal, risk_pct: Decimal) -> Option<Decimal> {
    if risk_pct <= Decimal::ZERO || start_equity <= Decimal::ZERO {
        None
    } else {
        let one = start_equity * risk_pct;
        if one > Decimal::ZERO {
            Some(one)
        } else {
            None
        }
    }
}

/// USDT size of the R budget: `start_equity * risk_pct * loss_r`.
pub fn r_budget_usdt(start_equity: Decimal, risk_pct: Decimal, loss_r: Decimal) -> Decimal {
    match one_r_usdt(start_equity, risk_pct) {
        Some(one) if loss_r > Decimal::ZERO => one * loss_r,
        _ => Decimal::ZERO,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRisk {
    pub day_utc: String,
    pub start_equity: Decimal,
    pub halt: bool,
    pub pnl: Decimal,
}

/// `(usdt_limit, loss_r, risk_pct)` — either layer may trip (OR, not max).
pub fn evaluate(
    day_utc: &str,
    start_equity: Option<Decimal>,
    halt: bool,
    now: f64,
    equity: Decimal,
    usdt_limit: Decimal,
    loss_r: Decimal,
    risk_pct: Decimal,
) -> DayRisk {
    let today = utc_day_key(now);
    if day_utc.is_empty() || day_utc != today || start_equity.is_none() {
        return DayRisk {
            day_utc: today,
            start_equity: equity,
            halt: false,
            pnl: Decimal::ZERO,
        };
    }
    let start = start_equity.unwrap();
    let pnl = equity - start;
    let usdt_trip = usdt_limit > Decimal::ZERO && pnl <= -usdt_limit;
    let r_budget = r_budget_usdt(start, risk_pct, loss_r);
    let r_trip = r_budget > Decimal::ZERO && pnl <= -r_budget;
    DayRisk {
        day_utc: day_utc.to_string(),
        start_equity: start,
        halt: halt || usdt_trip || r_trip,
        pnl,
    }
}

/// Args: `(usdt_limit, loss_r, risk_pct)`.
pub fn apply_day_risk(
    state: &mut EngineState,
    now: f64,
    equity: Decimal,
    usdt_limit: Decimal,
    loss_r: Decimal,
    risk_pct: Decimal,
) -> DayRisk {
    let risk = evaluate(
        &state.day_utc,
        state.day_start_equity,
        state.daily_halt,
        now,
        equity,
        usdt_limit,
        loss_r,
        risk_pct,
    );
    state.day_utc = risk.day_utc.clone();
    state.day_start_equity = Some(risk.start_equity);
    state.daily_halt = risk.halt;
    risk
}

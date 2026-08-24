//! UTC daily loss halt. `r` does not lift it; the next UTC day does.

use crate::models::EngineState;
use crate::sessions::utc_datetime;
use rust_decimal::Decimal;

pub fn default_daily_loss_usdt() -> Decimal {
    Decimal::from(20)
}

pub fn utc_day_key(now: f64) -> String {
    utc_datetime(now).format("%Y-%m-%d").to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRisk {
    pub day_utc: String,
    pub start_equity: Decimal,
    pub halt: bool,
    pub pnl: Decimal,
}

pub fn evaluate(
    day_utc: &str,
    start_equity: Option<Decimal>,
    halt: bool,
    now: f64,
    equity: Decimal,
    limit: Decimal,
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
    let tripped = halt || (limit > Decimal::ZERO && pnl <= -limit);
    DayRisk {
        day_utc: day_utc.to_string(),
        start_equity: start,
        halt: tripped,
        pnl,
    }
}

pub fn apply_day_risk(state: &mut EngineState, now: f64, equity: Decimal, limit: Decimal) -> DayRisk {
    let risk = evaluate(
        &state.day_utc,
        state.day_start_equity,
        state.daily_halt,
        now,
        equity,
        limit,
    );
    state.day_utc = risk.day_utc.clone();
    state.day_start_equity = Some(risk.start_equity);
    state.daily_halt = risk.halt;
    risk
}

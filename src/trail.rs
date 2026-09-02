//! Take-profit attach and upward-only stop-loss trail (longs: SL only выше).

use crate::money::{require_positive, round_trip_taker_pct};
use rust_decimal::Decimal;

pub fn take_profit_price(entry: Decimal, side: &str, tp_pct: Decimal) -> Result<Decimal, String> {
    let entry_d = require_positive(entry, "entry").map_err(|e| e.to_string())?;
    let pct = require_positive(tp_pct, "tp_pct").map_err(|e| e.to_string())?;
    match side.to_ascii_uppercase().as_str() {
        "BUY" | "LONG" => Ok(entry_d * (Decimal::ONE + pct)),
        "SELL" | "SHORT" => Ok(entry_d * (Decimal::ONE - pct)),
        other => Err(format!("unknown side: {other}")),
    }
}

/// TP so that `tp_pct` remains after taker in + taker out.
pub fn take_profit_price_net(entry: Decimal, side: &str, tp_pct: Decimal) -> Result<Decimal, String> {
    take_profit_price(entry, side, tp_pct + round_trip_taker_pct())
}

pub fn candidate_stop(price: Decimal, side: &str, trail_pct: Decimal) -> Result<Decimal, String> {
    let price_d = require_positive(price, "price").map_err(|e| e.to_string())?;
    let pct = require_positive(trail_pct, "trail_pct").map_err(|e| e.to_string())?;
    match side.to_ascii_uppercase().as_str() {
        "BUY" | "LONG" => Ok(price_d * (Decimal::ONE - pct)),
        "SELL" | "SHORT" => Ok(price_d * (Decimal::ONE + pct)),
        other => Err(format!("unknown side: {other}")),
    }
}

pub fn trail_stop_upward(current_sl: Option<Decimal>, candidate: Decimal, side: &str) -> Result<Decimal, String> {
    let cand = require_positive(candidate, "candidate").map_err(|e| e.to_string())?;
    let Some(current) = current_sl else {
        return Ok(cand);
    };
    let current = require_positive(current, "current_sl").map_err(|e| e.to_string())?;
    match side.to_ascii_uppercase().as_str() {
        "BUY" | "LONG" => Ok(if cand > current { cand } else { current }),
        "SELL" | "SHORT" => Ok(if cand < current { cand } else { current }),
        other => Err(format!("unknown side: {other}")),
    }
}

pub fn long_stop_is_valid(stop: Decimal, mark: Decimal) -> bool {
    stop > Decimal::ZERO && mark > Decimal::ZERO && stop < mark
}

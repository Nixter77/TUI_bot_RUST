//! Decimal helpers for prices and quantities.

use rust_decimal::Decimal;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MoneyError {
    #[error("missing numeric value")]
    Missing,
    #[error("not a decimal: {0}")]
    Invalid(String),
    #[error("{0} must be positive, got {1}")]
    NotPositive(&'static str, Decimal),
    #[error("step must be positive")]
    NonPositiveStep,
}

pub fn dec(value: &str) -> Result<Decimal, MoneyError> {
    let t = value.trim();
    if t.is_empty() {
        return Err(MoneyError::Missing);
    }
    t.parse::<Decimal>()
        .map_err(|_| MoneyError::Invalid(value.to_string()))
}

pub fn require_positive(value: Decimal, name: &'static str) -> Result<Decimal, MoneyError> {
    if value <= Decimal::ZERO {
        return Err(MoneyError::NotPositive(name, value));
    }
    Ok(value)
}

pub fn quantize_to_step(value: Decimal, step: Decimal, round_up: bool) -> Result<Decimal, MoneyError> {
    if step <= Decimal::ZERO {
        return Err(MoneyError::NonPositiveStep);
    }
    let units = value / step;
    let rounded = if round_up { units.ceil() } else { units.floor() };
    Ok(rounded * step)
}

pub fn fmt_fixed(value: Decimal) -> String {
    let n = value.normalize();
    let s = n.to_string();
    if s.contains('e') || s.contains('E') {
        format!("{n}")
    } else {
        s
    }
}

/// Binance USDT-M taker, one side. Domain policy — not journal I/O.
pub fn taker_fee() -> Decimal {
    Decimal::new(4, 4) // 0.0004
}

pub fn round_trip_taker_pct() -> Decimal {
    taker_fee() + taker_fee()
}

pub fn long_pnl(entry: Decimal, exit_price: Decimal, qty: Decimal, fee_rate: Decimal) -> (Decimal, Decimal) {
    let fee = (entry + exit_price) * qty * fee_rate;
    ((exit_price - entry) * qty - fee, fee)
}

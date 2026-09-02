//! Daily USDT + R halt layers (OR: either trips).

mod common;
use common::*;
use rust_decimal::Decimal;
use tui_bot::dayrisk::{
    apply_day_risk, default_daily_loss_r, default_daily_loss_usdt, evaluate, one_r_usdt, r_budget_usdt,
    utc_day_key,
};
use tui_bot::models::EngineState;
use tui_bot::sessions::make_utc_ts;

fn now() -> f64 {
    make_utc_ts(2026, 8, 17, 12, 0, 0)
}

#[test]
fn one_r_is_start_equity_times_risk_pct() {
    assert_eq!(one_r_usdt(d("10000"), d("0.0025")), Some(d("25")));
    assert_eq!(r_budget_usdt(d("10000"), d("0.0025"), d("3")), d("75"));
    assert!(one_r_usdt(d("10000"), Decimal::ZERO).is_none());
    assert_eq!(r_budget_usdt(d("10000"), Decimal::ZERO, d("3")), Decimal::ZERO);
}

#[test]
fn usdt_halt_trips_when_pnl_hits_budget() {
    let day = utc_day_key(now());
    let risk = evaluate(&day, Some(d("10000")), false, now(), d("9979"), d("20"), d("99"), d("0.0025"));
    assert!(risk.halt);
    assert_eq!(risk.pnl, d("-21"));
}

#[test]
fn usdt_halt_does_not_trip_inside_budget() {
    let day = utc_day_key(now());
    let risk = evaluate(&day, Some(d("10000")), false, now(), d("9981"), d("20"), d("99"), d("0.0025"));
    assert!(!risk.halt);
}

#[test]
fn r_halt_trips_independently_when_usdt_budget_huge() {
    let day = utc_day_key(now());
    let risk = evaluate(
        &day,
        Some(d("10000")),
        false,
        now(),
        d("9920"),
        d("10000"),
        d("3"),
        d("0.0025"),
    );
    assert!(risk.halt, "R layer must trip alone");
    assert_eq!(risk.pnl, d("-80"));
}

#[test]
fn usdt_still_trips_even_when_r_budget_is_larger() {
    // Critical OR vs max: −50 hits USDT=20 even though R budget is 75.
    let day = utc_day_key(now());
    let risk = evaluate(
        &day,
        Some(d("10000")),
        false,
        now(),
        d("9950"),
        d("20"),
        d("3"),
        d("0.0025"),
    );
    assert!(risk.halt, "USDT layer must still trip under a larger R budget (OR not max)");
}

#[test]
fn r_halt_off_when_limit_r_or_risk_pct_zero() {
    let day = utc_day_key(now());
    let deep = evaluate(
        &day,
        Some(d("10000")),
        false,
        now(),
        d("9000"),
        d("10000"),
        Decimal::ZERO,
        d("0.0025"),
    );
    assert!(!deep.halt);
    let no_pct = evaluate(
        &day,
        Some(d("10000")),
        false,
        now(),
        d("9000"),
        d("10000"),
        d("3"),
        Decimal::ZERO,
    );
    assert!(!no_pct.halt);
}

#[test]
fn either_layer_latches_halt_until_next_utc_day() {
    let day = utc_day_key(now());
    let mut state = EngineState::new(4);
    state.day_utc = day;
    state.day_start_equity = Some(d("10000"));
    apply_day_risk(&mut state, now(), d("9920"), d("10000"), d("3"), d("0.0025"));
    assert!(state.daily_halt);
    apply_day_risk(&mut state, now(), d("10000"), d("10000"), d("3"), d("0.0025"));
    assert!(state.daily_halt);
    let tomorrow = make_utc_ts(2026, 8, 18, 0, 1, 0);
    apply_day_risk(&mut state, tomorrow, d("10000"), d("10000"), d("3"), d("0.0025"));
    assert!(!state.daily_halt);
    assert_eq!(state.day_utc, "2026-08-18");
}

#[test]
fn defaults_match_config_story() {
    assert_eq!(default_daily_loss_usdt(), Decimal::from(20));
    assert_eq!(default_daily_loss_r(), Decimal::from(3));
}

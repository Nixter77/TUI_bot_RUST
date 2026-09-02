//! Swing lows and EMA helpers used by Continuation.

use rust_decimal::Decimal;
use tui_bot::indicators::{last_ema, last_two_swing_lows};
use tui_bot::models::Bar;

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn bar(i: i64, low: &str, close: &str) -> Bar {
    let lo = d(low);
    Bar {
        open_time: 1_700_000_000_000 + i * 300_000,
        open: lo + d("1"),
        high: lo + d("2"),
        low: lo,
        close: d(close),
        volume: d("10"),
    }
}

#[test]
fn last_two_swing_lows_need_higher_second() {
    let bars = vec![
        bar(0, "10", "11"),
        bar(1, "9", "10"),
        bar(2, "8", "9"), // swing 8
        bar(3, "9", "10"),
        bar(4, "10", "11"),
        bar(5, "9.5", "10"),
        bar(6, "8.5", "9"), // swing 8.5 > 8
        bar(7, "9", "10"),
        bar(8, "10", "11"),
    ];
    let (a, b) = last_two_swing_lows(&bars).expect("two swings");
    assert_eq!(a, d("8"));
    assert_eq!(b, d("8.5"));
}

#[test]
fn ema20_needs_twenty_closes() {
    let closes: Vec<Decimal> = (0..19).map(|i| Decimal::from(i + 1)).collect();
    assert!(last_ema(&closes, 20).is_none());
    let mut closes = closes;
    closes.push(d("20"));
    assert!(last_ema(&closes, 20).is_some());
}

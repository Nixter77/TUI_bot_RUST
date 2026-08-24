//! Drive shipped trend_decision: Donchian 20 breakout.

mod common;
use common::*;
use rust_decimal::Decimal;
use tui_bot::indicators::sma;
use tui_bot::models::{Decision, Position, Side};
use tui_bot::trend::trend_decision;

#[test]
fn sma_matches_window_mean() {
    let values = vec![d("1"), d("2"), d("3"), d("4")];
    assert_eq!(sma(&values, 3), Some(d("3")));
    assert_eq!(sma(&values, 5), None);
}

#[test]
fn enters_on_donchian_breakout() {
    let bars = range_then_breakout();
    let decision = trend_decision(&bars, None, "ETHUSDT", Some(&trend_loose()));
    match decision {
        Decision::EnterLong {
            symbol,
            take_profit,
            stop_loss,
            reason,
        } => {
            assert_eq!(symbol, "ETHUSDT");
            let mark = bars.last().unwrap().close;
            assert!(stop_loss < mark);
            assert!(take_profit > mark);
            assert!(reason.contains("Donchian"));
        }
        other => panic!("expected enter, got {:?} {}", other, other.reason()),
    }
}

#[test]
fn does_not_buy_the_box() {
    let decision = trend_decision(&range_only(), None, "ETHUSDT", Some(&trend_loose()));
    assert!(matches!(decision, Decision::Hold { .. }));
}

#[test]
fn holds_in_downtrend() {
    let decision = trend_decision(&grind_down(), None, "ETHUSDT", Some(&trend_loose()));
    assert!(matches!(decision, Decision::Hold { .. }));
}

#[test]
fn exits_when_close_loses_exit_channel() {
    let mut extra = range_then_breakout();
    let last_i = extra.len() as i64;
    let mut px = extra.last().unwrap().close.to_string().parse::<f64>().unwrap();
    for j in 0..12 {
        let nxt = px - 1.5;
        extra.push(trend_bar(last_i + j, px, px + 0.1, nxt - 0.1, nxt));
        px = nxt;
    }
    let pos = Position {
        symbol: "ETHUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: d("102"),
        stop_loss: Some(d("1")),
        take_profit: Some(d("10000")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(extra[0].open_time),
        leverage: 0,
    };
    let decision = trend_decision(&extra, Some(&pos), "ETHUSDT", Some(&trend_loose()));
    match decision {
        Decision::ExitPosition { reason, .. } => assert!(reason.contains("Donchian 10")),
        other => panic!("{:?} {}", other, other.reason()),
    }
}

#[test]
fn exits_on_stop() {
    let bars = range_then_breakout();
    let mark = bars.last().unwrap().close;
    let pos = Position {
        symbol: "ETHUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: mark,
        stop_loss: Some(mark + Decimal::ONE),
        take_profit: Some(mark + d("50")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: None,
        leverage: 0,
    };
    let decision = trend_decision(&bars, Some(&pos), "ETHUSDT", Some(&trend_loose()));
    match decision {
        Decision::ExitPosition { reason, .. } => assert!(reason.contains("stop")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn not_enough_bars() {
    assert!(matches!(
        trend_decision(&[trend_bar(0, 1.0, 1.1, 0.9, 1.0)], None, "X", None),
        Decision::Hold { .. }
    ));
}

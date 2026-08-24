//! Drive shipped scalp_decision: VWAP/EMA pullback, not impulse chase.

mod common;
use common::*;
use rust_decimal::Decimal;
use tui_bot::models::{Decision, Position, Side};
use tui_bot::scalp::scalp_decision;

#[test]
fn enters_on_vwap_ema_pullback() {
    let bars = grind_then_pullback(london_ms());
    let decision = scalp_decision(&bars, None, "BTCUSDT", Some(&scalp_loose()), None);
    match decision {
        Decision::EnterLong {
            symbol,
            take_profit,
            stop_loss,
            ..
        } => {
            assert_eq!(symbol, "BTCUSDT");
            let mark = bars.last().unwrap().close;
            assert!(stop_loss < mark);
            assert!(take_profit > mark);
            let risk = mark - stop_loss;
            assert!(take_profit - mark >= risk * d("1.9"));
        }
        other => panic!("expected enter, got {:?} {}", other, other.reason()),
    }
}

#[test]
fn does_not_chase_straight_impulse() {
    let decision = scalp_decision(&stair(), None, "BTCUSDT", Some(&scalp_loose()), None);
    assert!(matches!(decision, Decision::Hold { .. }));
    assert!(!decision.reason().contains("take profit"));
}

#[test]
fn hours_are_opt_in_not_strategy1_windows() {
    let bars = grind_then_pullback(night_ms());
    let open_all = scalp_decision(&bars, None, "BTCUSDT", Some(&scalp_loose()), None);
    assert!(
        matches!(open_all, Decision::EnterLong { .. }),
        "{}",
        open_all.reason()
    );
    let mut gated = scalp_loose();
    gated.entry_windows = vec![(7, 10), (13, 16)];
    let blocked = scalp_decision(&bars, None, "BTCUSDT", Some(&gated), None);
    assert!(matches!(blocked, Decision::Hold { .. }));
    assert!(blocked.reason().contains("сессии"));
}

#[test]
fn holds_when_not_enough_or_red() {
    assert!(matches!(
        scalp_decision(&[], None, "X", None, None),
        Decision::Hold { .. }
    ));
    let mut bars = grind_then_pullback(london_ms());
    let last = bars.last().unwrap().clone();
    let n = bars.len();
    bars[n - 1].open = last.close;
    bars[n - 1].close = last.open;
    assert!(matches!(
        scalp_decision(&bars, None, "X", Some(&scalp_loose()), None),
        Decision::Hold { .. }
    ));
}

#[test]
fn exits_on_take_profit() {
    let bars = grind_then_pullback(london_ms());
    let mark = bars.last().unwrap().close;
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: mark - Decimal::ONE,
        stop_loss: Some(mark - d("2")),
        take_profit: Some(mark - d("0.01")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(bars[0].open_time),
        leverage: 0,
    };
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&scalp_loose()), None);
    match decision {
        Decision::ExitPosition { reason, .. } => assert!(reason.contains("take profit")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn exits_on_stop() {
    let bars = grind_then_pullback(london_ms());
    let mark = bars.last().unwrap().close;
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: mark + d("5"),
        stop_loss: Some(mark + Decimal::ONE),
        take_profit: Some(mark + d("10")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(bars[0].open_time),
        leverage: 0,
    };
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&scalp_loose()), None);
    match decision {
        Decision::ExitPosition { reason, .. } => assert!(reason.contains("stop")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn trails_stop_up_once_in_profit() {
    let bars = grind_then_pullback(london_ms());
    let mark = bars.last().unwrap().close;
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: mark * d("0.98"),
        stop_loss: Some(mark * d("0.975")),
        take_profit: Some(mark * d("1.05")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(bars[bars.len() - 3].open_time),
        leverage: 0,
    };
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&scalp_loose()), None);
    match decision {
        Decision::AmendStop { stop_loss, .. } => {
            assert!(stop_loss > mark * d("0.95"));
            assert!(stop_loss < mark);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn does_not_trail_until_in_profit() {
    let bars = grind_then_pullback(london_ms());
    let mark = bars.last().unwrap().close;
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: mark,
        stop_loss: Some(mark * d("0.997")),
        take_profit: Some(mark * d("1.02")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(bars[bars.len() - 2].open_time),
        leverage: 0,
    };
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&scalp_loose()), None);
    assert!(matches!(decision, Decision::Hold { .. }));
}

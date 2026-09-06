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
    gated.always_enter = false;
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
    // ~1R at mark, below 1.5R bank so AmendStop still fires.
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: mark * d("0.995"),
        stop_loss: Some(mark * d("0.990")),
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
        other => panic!("{} {:?}", other.reason(), other),
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


#[test]
fn default_max_hold_is_eight() {
    assert_eq!(tui_bot::scalp::ScalpParams::default().max_hold_bars, 8);
}

#[test]
fn exits_at_end_of_session() {
    let bars = grind_then_pullback(london_ms());
    let mark = bars.last().unwrap().close;
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: mark * d("0.99"),
        stop_loss: Some(mark * d("0.985")),
        take_profit: Some(mark * d("1.05")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(bars[bars.len() - 3].open_time),
        leverage: 0,
    };
    let mut p = scalp_loose();
    p.always_enter = false;
    p.entry_windows = vec![(7, 10), (13, 16)];
    let night = night_ms() as f64 / 1000.0;
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&p), Some(night));
    match decision {
        Decision::ExitPosition { reason, .. } => assert!(reason.contains("конец сессии"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn peak_giveback_locks_or_exits_pre_be() {
    let mut bars = grind_then_pullback(london_ms());
    let mark0 = bars.last().unwrap().close;
    let entry = mark0 * d("0.99");
    let risk = mark0 * d("0.01");
    let sl = entry - risk;
    let n = bars.len();
    let open_t = bars[n - 5].open_time;
    let peak_px = entry + risk * d("0.9");
    bars[n - 3].high = peak_px;
    bars[n - 3].close = peak_px;
    bars[n - 1].close = entry + risk * d("0.1");
    bars[n - 1].high = entry + risk * d("0.15");
    bars[n - 1].low = entry;
    bars[n - 1].open = entry + risk * d("0.12");
    let mark = bars[n - 1].close;
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: entry,
        stop_loss: Some(sl),
        take_profit: Some(entry + risk * d("2")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(open_t),
        leverage: 0,
    };
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&scalp_loose()), None);
    match decision {
        Decision::AmendStop { reason, stop_loss, .. } => {
            assert!(reason.contains("откат с пика"), "{reason}");
            assert!(stop_loss > sl);
            assert!(stop_loss < mark || stop_loss >= entry);
        }
        Decision::ExitPosition { reason, .. } => {
            assert!(reason.contains("откат с пика"), "{reason}");
        }
        other => panic!("expected peak giveback, got {:?} {}", other, other.reason()),
    }
}

#[test]
fn time_stop_uses_max_hold_eight() {
    let bars = grind_then_pullback(london_ms());
    let mark = bars.last().unwrap().close;
    let opened = bars[bars.len() - 12].open_time;
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: mark,
        stop_loss: Some(mark * d("0.99")),
        take_profit: Some(mark * d("1.05")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(opened),
        leverage: 0,
    };
    let mut p = scalp_loose();
    p.max_hold_bars = 8;
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&p), None);
    match decision {
        Decision::ExitPosition { reason, .. } => assert!(reason.contains("time stop"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn banks_at_one_and_half_r() {
    let bars = grind_then_pullback(london_ms());
    let mark = bars.last().unwrap().close;
    let entry = mark * d("0.98");
    let risk = mark * d("0.01");
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: entry,
        stop_loss: Some(entry - risk),
        take_profit: Some(mark * d("1.10")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(bars[bars.len() - 3].open_time),
        leverage: 0,
    };
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&scalp_loose()), None);
    match decision {
        Decision::ExitPosition { reason, .. } => assert!(reason.contains("1.5R"), "{reason}"),
        other => panic!("{} {:?}", other.reason(), other),
    }
}

#[test]
fn fee_aware_breakeven_beats_raw_entry() {
    use tui_bot::money::round_trip_taker_pct;
    let bars = grind_then_pullback(london_ms());
    let mark = bars.last().unwrap().close;
    let entry = mark * d("0.995");
    let risk = mark * d("0.004");
    let mut params = scalp_loose();
    params.trail_atr = d("10");
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: Decimal::ONE,
        entry_price: entry,
        stop_loss: Some(entry - risk),
        take_profit: Some(mark * d("1.10")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: Some(bars[bars.len() - 2].open_time),
        leverage: 0,
    };
    let decision = scalp_decision(&bars, Some(&pos), "BTCUSDT", Some(&params), None);
    match decision {
        Decision::AmendStop { stop_loss, .. } => {
            let fee_be = entry * (Decimal::ONE + round_trip_taker_pct());
            assert!(stop_loss >= fee_be, "sl={stop_loss} fee_be={fee_be}");
            assert!(stop_loss > entry);
        }
        other => panic!("{} {:?}", other.reason(), other),
    }
}

#[test]
fn entry_tp_is_fee_padded() {
    use tui_bot::money::round_trip_taker_pct;
    let bars = grind_then_pullback(london_ms());
    let decision = scalp_decision(&bars, None, "BTCUSDT", Some(&scalp_loose()), None);
    match decision {
        Decision::EnterLong {
            take_profit,
            stop_loss,
            ..
        } => {
            let mark = bars.last().unwrap().close;
            let risk = mark - stop_loss;
            let padded = (mark + risk * d("2")) * (Decimal::ONE + round_trip_taker_pct());
            assert!(take_profit >= padded * d("0.999"), "tp={take_profit} padded={padded}");
        }
        other => panic!("{} {:?}", other.reason(), other),
    }
}

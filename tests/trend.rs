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
    let decision = trend_decision(&bars, None, "ETHUSDT", Some(&trend_loose()), None);
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
    let decision = trend_decision(&range_only(), None, "ETHUSDT", Some(&trend_loose()), None);
    assert!(matches!(decision, Decision::Hold { .. }));
}

#[test]
fn holds_in_downtrend() {
    let decision = trend_decision(&grind_down(), None, "ETHUSDT", Some(&trend_loose()), None);
    assert!(matches!(decision, Decision::Hold { .. }));
}

#[test]
fn exits_when_close_loses_exit_channel() {
    let mut extra = range_then_breakout();
    let last_i = extra.len() as i64;
    let mut px = extra
        .last()
        .unwrap()
        .close
        .to_string()
        .parse::<f64>()
        .unwrap();
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
    let decision = trend_decision(&extra, Some(&pos), "ETHUSDT", Some(&trend_loose()), None);
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
    let decision = trend_decision(&bars, Some(&pos), "ETHUSDT", Some(&trend_loose()), None);
    match decision {
        Decision::ExitPosition { reason, .. } => assert!(reason.contains("stop")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn not_enough_bars() {
    assert!(matches!(
        trend_decision(&[trend_bar(0, 1.0, 1.1, 0.9, 1.0)], None, "X", None, None),
        Decision::Hold { .. }
    ));
}

#[test]
fn default_is_donchian_40_ema50() {
    let p = tui_bot::trend::TrendParams::default();
    assert_eq!(p.channel, 40);
    assert_eq!(p.exit_channel, 20);
    assert_eq!(p.ema_filter, 50);
    assert_eq!(p.entry_grace_sec, tui_bot::trend::ENTRY_GRACE_SEC);
    assert_eq!(p.max_stop_pct, tui_bot::trend::max_stop_pct());
    // 40 bars: not enough for EMA50 (51) or Donchian 40 (42).
    let short = range_then_breakout()[..40].to_vec();
    match trend_decision(&short, None, "ETHUSDT", None, None) {
        Decision::Hold { reason } => assert!(reason.contains("not enough bars"), "{reason}"),
        other => panic!("short history must hold: {other:?}"),
    }
}

#[test]
fn breakout_extension_is_close_over_prior_high() {
    let bars = range_then_breakout();
    let ext = tui_bot::trend::breakout_extension(&bars, 20).expect("ext");
    // last close 102.1, prior 20-high 100.5
    let expect = (d("102.1") - d("100.5")) / d("100.5");
    assert_eq!(ext, expect);
    assert!(ext > Decimal::ZERO);
}

#[test]
fn exit_and_amend_carry_symbol() {
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
    match trend_decision(&bars, Some(&pos), "ETHUSDT", Some(&trend_loose()), None) {
        Decision::ExitPosition { symbol, .. } => assert_eq!(symbol, "ETHUSDT"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn does_not_chase_breakout_hours_after_daily_close() {
    let bars = range_then_breakout();
    let mut p = trend_loose();
    p.entry_grace_sec = tui_bot::trend::ENTRY_GRACE_SEC;
    let close_ts = tui_bot::trend::daily_bar_close_ts(bars.last().unwrap().open_time);
    let fresh = trend_decision(&bars, None, "ETHUSDT", Some(&p), Some(close_ts + 60.0));
    assert!(
        matches!(fresh, Decision::EnterLong { .. }),
        "just after the daily close must still enter: {} ",
        fresh.reason()
    );
    let stale = trend_decision(
        &bars,
        None,
        "ETHUSDT",
        Some(&p),
        Some(close_ts + 10.0 * 3_600.0),
    );
    match stale {
        Decision::Hold { reason } => assert!(reason.contains("не на закрытии"), "{reason}"),
        other => panic!("stale daily breakout must hold, got {other:?}"),
    }
}

#[test]
fn live_left_daily_close_rejects_pump_chase() {
    let close = d("0.127");
    assert_eq!(
        tui_bot::trend::live_left_daily_close(
            close,
            d("0.168"),
            tui_bot::trend::max_close_extension_pct()
        ),
        Some("цена ушла от закрытия дня")
    );
    assert_eq!(
        tui_bot::trend::live_left_daily_close(
            close,
            d("0.120"),
            tui_bot::trend::max_close_extension_pct()
        ),
        Some("цена ниже закрытия пробоя")
    );
    assert_eq!(
        tui_bot::trend::live_left_daily_close(
            close,
            d("0.128"),
            tui_bot::trend::max_close_extension_pct()
        ),
        None
    );
}

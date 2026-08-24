//! Round-trip taker fee is deducted from PnL; TP is placed net of both sides.

use rust_decimal::Decimal;
use tui_bot::errors::COOLDOWN_SEC;
use tui_bot::journal::{
    cooldowns_from_events, desk_cooldown_from_events, journal_symbol, long_pnl, taker_fee, TradeEvent,
};
use tui_bot::trail::{take_profit_price, take_profit_price_net};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

#[test]
fn long_pnl_subtracts_taker_both_sides() {
    let entry = d("100");
    let exit = d("102.5");
    let qty = d("1");
    let (pnl, fee) = long_pnl(entry, exit, qty, taker_fee());
    let gross = (exit - entry) * qty;
    let expect_fee = (entry + exit) * qty * taker_fee();
    assert_eq!(fee, expect_fee);
    assert_eq!(pnl, gross - expect_fee);
    assert!(pnl < gross);
    assert!(pnl > Decimal::ZERO);
}

#[test]
fn take_profit_net_stays_green_after_fees() {
    let entry = d("100");
    let tp_pct = d("0.025");
    let qty = d("1");
    let gross_tp = take_profit_price(entry, "LONG", tp_pct).unwrap();
    let net_tp = take_profit_price_net(entry, "LONG", tp_pct).unwrap();
    assert!(net_tp > gross_tp);
    let (pnl_at_net, _) = long_pnl(entry, net_tp, qty, taker_fee());
    let (pnl_at_gross, _) = long_pnl(entry, gross_tp, qty, taker_fee());
    assert!(pnl_at_net >= entry * tp_pct - d("0.01"));
    assert!(pnl_at_gross < entry * tp_pct);
    assert!(pnl_at_net > pnl_at_gross);
}

#[test]
fn journal_symbol_strips_side_prefix() {
    assert_eq!(journal_symbol("SHORT BTCUSDT"), "BTCUSDT");
    assert_eq!(journal_symbol("long ethusdt"), "ETHUSDT");
    assert_eq!(journal_symbol("SUPERUSDT"), "SUPERUSDT");
}

#[test]
fn recent_closes_seed_cooldown_so_restart_does_not_rebuy() {
    let events = vec![
        TradeEvent {
            ts: "2026-08-24T01:55:18Z".into(),
            event: "close".into(),
            symbol: "SUPERUSDT".into(),
            ..TradeEvent::default()
        },
        TradeEvent {
            ts: "2026-08-24T00:24:19Z".into(),
            event: "close".into(),
            symbol: "MORPHOUSDT".into(),
            ..TradeEvent::default()
        },
        TradeEvent {
            ts: "2026-08-23T20:58:24Z".into(),
            event: "flatten".into(),
            symbol: "SHORT BTCUSDT".into(),
            ..TradeEvent::default()
        },
    ];
    let now = tui_bot::sessions::make_utc_ts(2026, 8, 24, 2, 10, 12);
    let map = cooldowns_from_events(&events, now, COOLDOWN_SEC);
    assert!(
        map.get("SUPERUSDT").copied().unwrap_or(0.0) > now,
        "{map:?}"
    );
    assert!(!map.contains_key("MORPHOUSDT"), "{map:?}");
    assert!(!map.contains_key("BTCUSDT"), "{map:?}");
}

#[test]
fn losing_close_keeps_whole_desk_paused() {
    let events = vec![
        TradeEvent {
            ts: "2026-08-24T07:00:38Z".into(),
            event: "close".into(),
            symbol: "LAUSDT".into(),
            pnl: Some("-0.12".into()),
            ..TradeEvent::default()
        },
        TradeEvent {
            ts: "2026-08-24T07:25:17Z".into(),
            event: "close".into(),
            symbol: "KNCUSDT".into(),
            pnl: Some("0.17".into()),
            ..TradeEvent::default()
        },
    ];
    let now = tui_bot::sessions::make_utc_ts(2026, 8, 24, 7, 10, 0);
    let until = desk_cooldown_from_events(&events, now, COOLDOWN_SEC);
    assert!(until > now, "{until}");
    let after_win = tui_bot::sessions::make_utc_ts(2026, 8, 24, 7, 40, 0);
    let later = desk_cooldown_from_events(&events, after_win, COOLDOWN_SEC);
    assert!(
        later > after_win,
        "losing close still sits out the London window: {later}"
    );
    let after_window = tui_bot::sessions::make_utc_ts(2026, 8, 24, 10, 0, 1);
    let done = desk_cooldown_from_events(&events, after_window, COOLDOWN_SEC);
    assert_eq!(done, 0.0, "pause lifts when the window ends");
}

#[test]
fn london_window_ends_at_ten_utc() {
    let ts = tui_bot::sessions::make_utc_ts(2026, 8, 24, 7, 1, 0);
    let end = tui_bot::sessions::window_end_ts(ts, &tui_bot::sessions::DEFAULT_ENTRY_WINDOWS).unwrap();
    let expect = tui_bot::sessions::make_utc_ts(2026, 8, 24, 10, 0, 0);
    assert_eq!(end, expect);
    let pause = tui_bot::sessions::pause_until_after_loss(ts, &tui_bot::sessions::DEFAULT_ENTRY_WINDOWS, 1800.0);
    assert_eq!(pause, expect);
}

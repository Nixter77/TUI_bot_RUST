//! Round-trip taker fee is deducted from PnL; TP is placed net of both sides.

use rust_decimal::Decimal;
use std::fs;
use std::thread;
use tui_bot::errors::{COOLDOWN_SEC, LOSS_SYMBOL_COOLDOWN_SEC};
use tui_bot::journal::{
    cooldowns_from_events, desk_cooldown_from_events, journal_symbol, long_pnl, set_active,
    symbol_pause_sec, taker_fee, unmatched_open_positions, unmatched_open_positions_from, TradeEvent,
    TradeJournal,
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
fn unmatched_opens_keep_sl_until_close() {
    let events = vec![
        TradeEvent {
            event: "open".into(),
            symbol: "VVVUSDT".into(),
            qty: "22.60".into(),
            price: "17.055".into(),
            stop_loss: Some("16.7139".into()),
            take_profit: Some("17.7514".into()),
            ..TradeEvent::default()
        },
        TradeEvent {
            event: "open".into(),
            symbol: "ETHUSDT".into(),
            qty: "0.01".into(),
            price: "3000".into(),
            stop_loss: Some("2940".into()),
            take_profit: Some("3120".into()),
            ..TradeEvent::default()
        },
        TradeEvent {
            event: "close".into(),
            symbol: "ETHUSDT".into(),
            ..TradeEvent::default()
        },
    ];
    let open = unmatched_open_positions_from(&events);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].symbol, "VVVUSDT");
    assert_eq!(open[0].stop_loss, Some(d("16.7139")));
    assert_eq!(open[0].take_profit, Some(d("17.7514")));
}

#[test]
fn unmatched_opens_apply_later_amend_stop() {
    let events = vec![
        TradeEvent {
            ts: "2026-08-31T15:47:10Z".into(),
            event: "open".into(),
            symbol: "VVVUSDT".into(),
            qty: "22.60".into(),
            price: "17.055".into(),
            stop_loss: Some("16.7139".into()),
            take_profit: Some("17.7514".into()),
            ..TradeEvent::default()
        },
        TradeEvent {
            ts: "2026-08-31T18:00:00Z".into(),
            event: "amend".into(),
            symbol: "VVVUSDT".into(),
            stop_loss: Some("17.0686".into()),
            take_profit: Some("17.7514".into()),
            reason: "безубыток на 1R".into(),
            ..TradeEvent::default()
        },
    ];
    let open = unmatched_open_positions_from(&events);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].stop_loss, Some(d("17.0686")));
    assert_eq!(open[0].opened_bar_time, Some(1_788_191_230_000));
}

#[test]
fn unmatched_partial_close_keeps_remainder_and_later_be() {
    let events = vec![
        TradeEvent {
            event: "open".into(),
            symbol: "AVAXUSDT".into(),
            qty: "0.02".into(),
            price: "100".into(),
            stop_loss: Some("98.5".into()),
            take_profit: Some("103.1".into()),
            ..TradeEvent::default()
        },
        TradeEvent {
            event: "close".into(),
            symbol: "AVAXUSDT".into(),
            qty: "0.01".into(),
            price: "101.5".into(),
            reason: "частичная фиксация 1R".into(),
            stop_loss: Some("98.5".into()),
            take_profit: Some("103.1".into()),
            ..TradeEvent::default()
        },
        TradeEvent {
            event: "amend".into(),
            symbol: "AVAXUSDT".into(),
            stop_loss: Some("100.08".into()),
            take_profit: Some("103.1".into()),
            reason: "безубыток на 1R".into(),
            ..TradeEvent::default()
        },
    ];
    let open = unmatched_open_positions_from(&events);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].qty, d("0.01"));
    assert_eq!(open[0].stop_loss, Some(d("100.08")));
    assert_eq!(open[0].take_profit, Some(d("103.1")));

    let mut closed = events;
    closed.push(TradeEvent {
        event: "close".into(),
        symbol: "AVAXUSDT".into(),
        qty: "0.01".into(),
        price: "100.08".into(),
        ..TradeEvent::default()
    });
    assert!(unmatched_open_positions_from(&closed).is_empty());
}

#[test]
fn recent_closes_seed_cooldown_so_restart_does_not_rebuy() {
    let events = vec![
        TradeEvent {
            ts: "2026-08-24T01:55:18Z".into(),
            event: "close".into(),
            symbol: "SUPERUSDT".into(),
            pnl: Some("-0.14".into()),
            ..TradeEvent::default()
        },
        TradeEvent {
            ts: "2026-08-24T00:24:19Z".into(),
            event: "close".into(),
            symbol: "MORPHOUSDT".into(),
            pnl: Some("0.20".into()),
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
fn losing_close_keeps_symbol_off_book_for_twelve_hours() {
    let events = vec![
        TradeEvent {
            ts: "2026-08-24T08:11:42Z".into(),
            event: "close".into(),
            symbol: "TAKEUSDT".into(),
            pnl: Some("-0.04".into()),
            ..TradeEvent::default()
        },
        TradeEvent {
            ts: "2026-08-24T07:00:00Z".into(),
            event: "close".into(),
            symbol: "BLESSUSDT".into(),
            pnl: Some("0.99".into()),
            ..TradeEvent::default()
        },
    ];
    // Close at T (London). 4h would free the name for NY the same UTC day.
    let eight_h = tui_bot::sessions::make_utc_ts(2026, 8, 24, 16, 15, 0);
    let map = cooldowns_from_events(&events, eight_h, COOLDOWN_SEC);
    assert!(
        map.get("TAKEUSDT").copied().unwrap_or(0.0) > eight_h,
        "loser still cooling ~8h later same UTC day: {map:?}"
    );
    assert!(!map.contains_key("BLESSUSDT"), "winner uses 30m pause: {map:?}");
    let thirteen_h = tui_bot::sessions::make_utc_ts(2026, 8, 24, 21, 15, 0);
    let later = cooldowns_from_events(&events, thirteen_h, COOLDOWN_SEC);
    assert!(!later.contains_key("TAKEUSDT"), "loser free after 12h+: {later:?}");
    assert_eq!(LOSS_SYMBOL_COOLDOWN_SEC, 43_200.0);
    assert_eq!(symbol_pause_sec(false, COOLDOWN_SEC), LOSS_SYMBOL_COOLDOWN_SEC);
    assert_eq!(symbol_pause_sec(true, COOLDOWN_SEC), COOLDOWN_SEC);
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

#[test]
fn parallel_appends_do_not_tear_jsonl_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trades.jsonl");
    let a_path = path.clone();
    let b_path = path.clone();
    let a = thread::spawn(move || {
        let j = TradeJournal::new(Some(&a_path));
        for i in 0..40 {
            j.append(&TradeEvent {
                event: "open".into(),
                symbol: format!("A{i}USDT"),
                ..TradeEvent::default()
            });
        }
    });
    let b = thread::spawn(move || {
        let j = TradeJournal::new(Some(&b_path));
        for i in 0..40 {
            j.append(&TradeEvent {
                event: "close".into(),
                symbol: format!("B{i}USDT"),
                ..TradeEvent::default()
            });
        }
    });
    a.join().unwrap();
    b.join().unwrap();
    let events = TradeJournal::new(Some(&path)).read_events();
    assert_eq!(events.len(), 80, "torn or dropped JSONL lines: {events:?}");
    let opens = events.iter().filter(|e| e.event == "open").count();
    let closes = events.iter().filter(|e| e.event == "close").count();
    assert_eq!(opens, 40);
    assert_eq!(closes, 40);
}

#[test]
fn unmatched_without_active_journal_is_empty() {
    set_active(None);
    assert!(unmatched_open_positions().is_empty());
}

#[test]
fn unmatched_reads_active_path_not_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trades.jsonl");
    let j = TradeJournal::new(Some(&path));
    j.record_open(
        4,
        "VVVUSDT",
        d("22.60"),
        d("17.055"),
        "test",
        false,
        Some(d("16.7139")),
        Some(d("17.751")),
    );
    set_active(Some(path));
    let open = unmatched_open_positions();
    set_active(None);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].symbol, "VVVUSDT");
    assert_eq!(open[0].stop_loss, Some(d("16.7139")));
}

#[cfg(unix)]
#[test]
fn journal_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trades.jsonl");
    let j = TradeJournal::new(Some(&path));
    j.append(&TradeEvent {
        event: "open".into(),
        symbol: "BTCUSDT".into(),
        ..TradeEvent::default()
    });
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "journal mode {mode:#o}");
}

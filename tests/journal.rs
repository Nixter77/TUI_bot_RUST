//! Round-trip taker fee is deducted from PnL; TP is placed net of both sides.

use rust_decimal::Decimal;
use std::fs;
use std::thread;
use tui_bot::errors::{COOLDOWN_SEC, LOSS_SYMBOL_COOLDOWN_SEC, S5_LOSS_SYMBOL_COOLDOWN_SEC};
use tui_bot::journal::{
    cooldowns_from_events, cooldowns_from_events_for, desk_cooldown_from_events, journal_symbol,
    long_close_was_win, long_pnl, set_active, symbol_pause_sec, symbol_pause_sec_for, taker_fee,
    unmatched_open_positions, unmatched_open_positions_from, TradeEvent, TradeJournal,
};
use tui_bot::trail::{take_profit_price, take_profit_price_net};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// Process-global journal ACTIVE path is shared across tests in this binary.
static JOURNAL_ACTIVE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
fn scratch_above_entry_is_not_a_win_after_fees() {
    assert!(!long_close_was_win(d("100"), d("100.05"), None));
    assert!(long_close_was_win(d("100"), d("100.20"), None));
    assert!(long_close_was_win(d("100"), d("99"), Some(d("99"))));
}

#[test]
fn s5_losing_close_cools_twenty_four_hours() {
    let events = vec![TradeEvent {
        ts: "2026-09-06T21:00:33Z".into(),
        event: "close".into(),
        strategy_id: 5,
        symbol: "ZECUSDT".into(),
        pnl: Some("-0.207".into()),
        ..Default::default()
    }];
    assert_eq!(S5_LOSS_SYMBOL_COOLDOWN_SEC, 86_400.0);
    assert_eq!(
        symbol_pause_sec_for(5, false, COOLDOWN_SEC),
        S5_LOSS_SYMBOL_COOLDOWN_SEC
    );
    assert_eq!(symbol_pause_sec_for(4, false, COOLDOWN_SEC), LOSS_SYMBOL_COOLDOWN_SEC);
    let t0 = tui_bot::sessions::make_utc_ts(2026, 9, 6, 21, 0, 33);
    let plus_13h = t0 + 13.0 * 3600.0;
    let s5 = cooldowns_from_events_for(&events, plus_13h, COOLDOWN_SEC, Some(5));
    assert!(
        s5.get("ZECUSDT").copied().unwrap_or(0.0) > plus_13h,
        "S5 loser still cooling 13h later: {s5:?}"
    );
    let s4 = cooldowns_from_events_for(&events, plus_13h, COOLDOWN_SEC, Some(4));
    assert!(s4.is_empty(), "S4 must not inherit S5 cooldown: {s4:?}");
    let plus_25h = t0 + 25.0 * 3600.0;
    let later = cooldowns_from_events_for(&events, plus_25h, COOLDOWN_SEC, Some(5));
    assert!(!later.contains_key("ZECUSDT"), "S5 loser free after 24h+: {later:?}");
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
    let _guard = JOURNAL_ACTIVE_TEST.lock().unwrap_or_else(|e| e.into_inner());
    set_active(None);
    assert!(unmatched_open_positions().is_empty());
}

#[test]
fn unmatched_reads_active_path_not_default() {
    let _guard = JOURNAL_ACTIVE_TEST.lock().unwrap_or_else(|e| e.into_inner());
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
        None,
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

#[test]
fn trade_event_parses_phase1_r_fields() {
    let line = r#"{"ts":"2026-09-06T01:00:00Z","event":"close","strategy_id":4,"symbol":"AAAUSDT","qty":"1","price":"102","reason":"test","pnl":"1.5","fee":"0.08","stop_loss":"98","take_profit":"104","live":true,"initial_risk_usdt":"2","final_r":"0.75","mfe_r":"1.2","mae_r":"0.3","mfe_usdt":"2.4","mae_usdt":"0.6","hold_sec":120,"time_to_mfe_sec":40,"time_to_1r_sec":55,"scaled_at_1r":true,"ret_24h":"5.2"}"#;
    let ev: TradeEvent = serde_json::from_str(line).unwrap();
    assert_eq!(ev.final_r.as_deref(), Some("0.75"));
    assert_eq!(ev.mfe_r.as_deref(), Some("1.2"));
    assert_eq!(ev.mae_usdt.as_deref(), Some("0.6"));
    assert_eq!(ev.hold_sec, Some(120));
    assert_eq!(ev.scaled_at_1r, Some(true));
    assert_eq!(ev.ret_24h.as_deref(), Some("5.2"));
}

#[test]
fn trade_event_old_line_still_parses() {
    let line = r#"{"ts":"2026-08-24T00:24:19Z","event":"close","strategy_id":1,"symbol":"MORPHOUSDT","qty":"13.8","price":"2.844","reason":"биржа закрыла лонг","pnl":"-0.74","fee":"0.03","stop_loss":null,"take_profit":null,"live":true,"leverage":null,"notional":null,"code":null}"#;
    let ev: TradeEvent = serde_json::from_str(line).unwrap();
    assert!(ev.final_r.is_none());
    assert!(ev.mfe_r.is_none());
    assert_eq!(ev.symbol, "MORPHOUSDT");
}

#[test]
fn open_meta_r_math_and_persist_roundtrip() {
    let _guard = JOURNAL_ACTIVE_TEST.lock().unwrap_or_else(|e| e.into_inner());
    use tui_bot::openmeta::{
        apply_mark_excursion, initial_risk_usdt, metrics_for_close, on_open, r_multiple, set_active_path,
        update_mark,
    };
    let dir = tempfile::tempdir().unwrap();
    let meta_path = dir.path().join("open_meta.json");
    set_active_path(Some(meta_path.clone()));
    let risk = initial_risk_usdt(d("100"), d("98"), d("2")).unwrap();
    assert_eq!(risk, d("4"));
    assert_eq!(r_multiple(d("6"), risk).unwrap(), d("1.5"));
    on_open(4, "TESTUSDT", d("100"), d("98"), d("2"), 1_000.0, Some("bull".into()));
    update_mark("TESTUSDT", d("103"), 1_010.0);
    let m = metrics_for_close("TESTUSDT", Some(d("5")), 1_100.0, false);
    assert_eq!(m.initial_risk_usdt.as_deref(), Some("4"));
    assert_eq!(m.final_r.as_deref(), Some("1.25"));
    assert!(m.mfe_usdt.is_some());
    assert_eq!(m.hold_sec, Some(100));
    assert_eq!(m.btc_regime.as_deref(), Some("bull"));
    // full close removed meta
    assert!(tui_bot::openmeta::get("TESTUSDT").is_none());
    let (mfe, mae, _, _) = apply_mark_excursion(
        d("0"),
        d("0"),
        None,
        None,
        d("100"),
        d("99"),
        d("2"),
        d("4"),
        5.0,
    );
    assert_eq!(mfe, d("0"));
    assert_eq!(mae, d("2")); // (100-99)*2
    set_active_path(None);
}

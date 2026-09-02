//! `--monitor` radar: waiting names, 24h tape, open/closed P&L.

use rust_decimal::Decimal;
use std::collections::HashMap;
use tui_bot::app::{dump_monitor_offline_strategy, help_text, parse_args, run};
use tui_bot::config::load_config;
use tui_bot::journal::TradeEvent;
use tui_bot::models::{EngineState, MarketSnapshot, Position, Side, Ticker};
use tui_bot::monitor::{build_monitor, classify_waiting, render_monitor, WaitKind};
use tui_bot::sessions::{make_utc_ts, utc_datetime};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn cfg_always() -> tui_bot::config::Config {
    let mut env = HashMap::new();
    env.insert("STRATEGY4_ALWAYS_ENTER".into(), "1".into());
    env.insert("STRATEGY1_ALWAYS_ENTER".into(), "1".into());
    load_config(false, None, Some(&env)).unwrap()
}

fn tape() -> Vec<Ticker> {
    vec![
        Ticker::new("BTCUSDT", d("50000"), d("1.0"), d("10000000")),
        Ticker::new("LINKUSDT", d("15"), d("3.2"), d("5000000")),
        Ticker::new("AAVEUSDT", d("200"), d("1.2"), d("4000000")),
        Ticker::new("APTUSDT", d("8"), d("20.0"), d("3000000")),
        Ticker::new("SKRUSDT", d("0.02"), d("82.1"), d("200000")),
        Ticker::new("DOGEUSDT", d("0.12"), d("-4.5"), d("800000")),
    ]
}

#[test]
fn dump_monitor_exit_zero_and_surfaces() {
    let (code, text, _) = dump_monitor_offline_strategy("4");
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("MONITOR"), "{text}");
    assert!(text.contains("Топ роста"), "{text}");
    assert!(text.contains("Топ падения"), "{text}");
    assert!(text.contains("В ожидании входа"), "{text}");
    assert!(text.contains("Открытые позиции"), "{text}");
    assert!(text.contains("Закрытые сегодня"), "{text}");
    assert!(text.contains("ордера не отправляются"), "{text}");
    assert!(!text.contains("x закрыть все"), "{text}");
    assert!(text.contains("Continuation: откат ликвидных"), "{text}");
}

#[test]
fn help_and_parse_monitor_flag() {
    let h = help_text();
    assert!(h.contains("--monitor"), "help missing --monitor:\n{h}");
    let args = parse_args(["--monitor", "--strategy", "4"]).unwrap();
    assert!(args.monitor);
    assert_eq!(args.strategy, "4");
    assert!(!args.live);
}

#[test]
fn monitor_plus_live_without_keys_is_watch() {
    let args = parse_args(["--monitor", "--live", "--dump-frame", "--offline"]).unwrap();
    assert!(args.monitor && args.live);
    let env = HashMap::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, Some(&env), &mut out, &mut err);
    let text = String::from_utf8_lossy(&out);
    assert_eq!(code, 0, "{}{}", String::from_utf8_lossy(&err), text);
    assert!(text.contains("MONITOR"), "{text}");
}

#[test]
fn waiting_and_growth_and_pnl() {
    let cfg = cfg_always();
    let state = EngineState::new(4);
    let mut snap = MarketSnapshot::empty(d("1000"));
    snap.tickers = tape();
    snap.account.wallet_balance = d("1000");
    snap.account.starting_equity = d("1000");
    snap.open_positions = vec![Position {
        symbol: "LINKUSDT".into(),
        side: Side::Long,
        qty: d("10"),
        entry_price: d("14"),
        stop_loss: Some(d("13.5")),
        take_profit: Some(d("15.5")),
        unrealized_pnl: d("8"),
        opened_bar_time: None,
        leverage: 0,
    }];
    snap.live_book = true;
    snap.account.unrealized_pnl = d("8");

    let now = make_utc_ts(2026, 9, 2, 10, 0, 0);
    let ts = utc_datetime(now).to_rfc3339();
    let events = vec![TradeEvent {
        ts: ts.clone(),
        event: "close".into(),
        strategy_id: 4,
        symbol: "VVVUSDT".into(),
        pnl: Some("-8.05".into()),
        reason: "stop".into(),
        ..TradeEvent::default()
    }];

    let waiting = classify_waiting(&cfg, &state, &snap, &snap.open_positions, now);
    assert!(
        waiting.iter().all(|w| w.symbol != "LINKUSDT"),
        "held name leaked into wait: {waiting:?}"
    );
    assert!(
        waiting.iter().all(|w| w.symbol != "SKRUSDT"),
        "24h leader must stay on tape, not in S4 book: {waiting:?}"
    );
    let apt = waiting.iter().find(|w| w.symbol == "APTUSDT").expect("APT in wait");
    assert_eq!(apt.kind, WaitKind::Setup, "{apt:?}");
    assert!(apt.reason.contains("улетело") || apt.reason.contains("не догоняю"), "{}", apt.reason);
    assert!(
        waiting.iter().any(|w| w.symbol == "AAVEUSDT"),
        "liquid mild-gain belongs in the book: {waiting:?}"
    );

    let view = build_monitor(&cfg, &state, &snap, &events, now);
    let wait_syms: Vec<_> = view.waiting.iter().map(|w| w.symbol.as_str()).collect();
    let rise_syms: Vec<_> = view.rising.iter().map(|t| t.symbol.as_str()).collect();
    assert_ne!(wait_syms, rise_syms, "wait book must not clone the 24h tape");
    let frame = render_monitor(&view);
    assert!(frame.contains("Топ роста"), "{frame}");
    assert!(frame.contains("не список покупок") || frame.contains("не топ 24h"), "{frame}");
    assert!(frame.contains("SKRUSDT"), "{frame}");
    assert!(frame.contains("+82.1%"), "{frame}");
    assert!(frame.contains("LINKUSDT"), "{frame}");
    assert!(frame.contains("[в плюсе]"), "{frame}");
    assert!(frame.contains("uPnL=+8.0000") || frame.contains("uPnL=+8"), "{frame}");
    assert!(frame.contains("VVVUSDT"), "{frame}");
    assert!(frame.contains("нетто=-8.0500") || frame.contains("нетто=-8.05"), "{frame}");
    assert!(frame.contains("APTUSDT"), "{frame}");
    assert!(frame.contains("[сетап]") || frame.contains("улетело"), "{frame}");
}

#[test]
fn s1_wait_lists_book_not_held() {
    let cfg = cfg_always();
    let state = EngineState::new(1);
    let mut snap = MarketSnapshot::empty(d("1000"));
    snap.tickers = tape();
    let now = make_utc_ts(2026, 9, 2, 10, 0, 0);
    let waiting = classify_waiting(&cfg, &state, &snap, &[], now);
    assert!(
        waiting.iter().any(|w| w.symbol == "LINKUSDT" || w.symbol == "AAVEUSDT" || w.symbol == "BTCUSDT"),
        "S1 book is eligible rising names, not the 24h blow-off: {waiting:?}"
    );
    assert!(
        waiting.iter().all(|w| w.symbol != "SKRUSDT"),
        "S1 max-change filter must drop SKR from the wait book: {waiting:?}"
    );
}

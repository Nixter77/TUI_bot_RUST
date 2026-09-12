//! Telegram trade pings: format + journal hook. No live Bot API.

use rust_decimal::Decimal;
use tui_bot::journal::{set_active, TradeEvent, TradeJournal};
use tui_bot::telegram::{
    begin_capture, format_message, install, should_notify, take_captured, TelegramDest,
};

const TG_FAKE_TOKEN: &str = "123456789:AAFakeTokenForUnitTestsOnly01234567890";

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn dest() -> TelegramDest {
    TelegramDest::parse(TG_FAKE_TOKEN, "111222333").unwrap()
}

fn live_open() -> TradeEvent {
    TradeEvent {
        event: "open".into(),
        strategy_id: 5,
        symbol: "LINKUSDT".into(),
        qty: "2".into(),
        price: "12.5".into(),
        reason: "pullback resume".into(),
        stop_loss: Some("12.0".into()),
        take_profit: Some("13.5".into()),
        live: true,
        ..TradeEvent::default()
    }
}

#[test]
fn parse_rejects_junk_without_echoing_token() {
    let err = TelegramDest::parse("short", "111222333").unwrap_err();
    assert!(err.contains("TELEGRAM_BOT_TOKEN"));
    assert!(!err.contains("short"));
    assert!(TelegramDest::parse(TG_FAKE_TOKEN, "abc").is_err());
    assert!(TelegramDest::parse(TG_FAKE_TOKEN, "12").is_err());
}

#[test]
fn only_live_open_close_flatten() {
    let mut ev = live_open();
    assert!(should_notify(&ev));
    let text = format_message(&ev).unwrap();
    assert!(text.contains("LIVE S5 Verify · LONG LINKUSDT"), "{text}");
    assert!(text.contains("вход  12.5"), "{text}");
    assert!(text.contains("SL    12.0"), "{text}");

    ev.event = "amend".into();
    assert!(!should_notify(&ev));
    assert!(format_message(&ev).is_none());

    ev.event = "close".into();
    ev.pnl = Some("-0.21".into());
    ev.final_r = Some("-0.4".into());
    let close = format_message(&ev).unwrap();
    assert!(close.contains("CLOSE LINKUSDT"), "{close}");
    assert!(close.contains("PnL -0.21 USDT"), "{close}");
    assert!(close.contains("(-0.4R)"), "{close}");

    ev.event = "flatten".into();
    let flat = format_message(&ev).unwrap();
    assert!(flat.contains("FLATTEN LINKUSDT"), "{flat}");

    ev.live = false;
    ev.event = "open".into();
    assert!(!should_notify(&ev));
}

#[test]
fn journal_live_open_pings_capture_not_amend() {
    begin_capture();
    install(Some(dest()));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trades.jsonl");
    set_active(Some(path.clone()));
    let j = TradeJournal::new(Some(&path));
    j.record_open(
        4,
        "ETHUSDT",
        d("0.01"),
        d("2500"),
        "test enter",
        true,
        Some(d("2400")),
        Some(d("2700")),
        None,
    );
    tui_bot::journal::record_amend(4, "ETHUSDT", d("2410"), Some(d("2700")), true, "trail");
    j.record_open(
        4,
        "BTCUSDT",
        d("0.01"),
        d("60000"),
        "paper",
        false,
        None,
        None,
        None,
    );
    let sent = take_captured();
    install(None);
    set_active(None);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("S4 Continuation · LONG ETHUSDT"),
        "{}",
        sent[0]
    );
    assert!(!sent
        .iter()
        .any(|s| s.contains("amend") || s.contains("BTCUSDT")));
}

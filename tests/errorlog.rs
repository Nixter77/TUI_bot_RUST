//! Drive error JSONL + secret redaction. No HTTP.

use std::fs;
use tui_bot::errorlog::{extract_code, ErrorLog};
use tui_bot::errors::{describe_exchange_error, redact_secrets};

#[test]
fn redact_strips_signature_from_transport_errors() {
    let raw = "HTTP /fapi/v1/order: https://testnet.binancefuture.com/fapi/v1/order?symbol=BTCUSDT&signature=abcDEF123";
    let clean = redact_secrets(raw);
    assert!(clean.contains("signature=***"), "{clean}");
    assert!(!clean.contains("abcDEF123"), "{clean}");
    let shown = describe_exchange_error(raw);
    assert!(!shown.contains("abcDEF123"), "{shown}");
}

#[test]
fn describe_does_not_panic_on_multibyte_truncation() {
    let raw = "ж".repeat(200);
    let shown = describe_exchange_error(&raw);
    assert!(shown.ends_with('…'), "{shown}");
    assert!(shown.chars().count() <= 160);
}

#[test]
fn extract_code_from_json_and_http() {
    assert_eq!(
        extract_code(r#"HTTP 400 /order: {"code":-2010,"msg":"x"}"#),
        "-2010"
    );
    assert_eq!(extract_code("HTTP 408 gateway"), "HTTP 408");
}

#[test]
fn observe_writes_shown_still_and_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("errors.jsonl");
    let mut log = ErrorLog::new(Some(&path));
    log.observe(
        Some("Ошибка: TestNet не ответил вовремя (−1007)."),
        "HTTP 408",
        "poll",
        4,
        true,
        "BTCUSDT",
        Some("2026-08-29T00:00:00Z"),
        Some(0.0),
    );
    for i in 1..=12 {
        log.observe(
            Some("Ошибка: TestNet не ответил вовремя (−1007)."),
            "HTTP 408",
            "poll",
            4,
            true,
            "BTCUSDT",
            Some("2026-08-29T00:00:05Z"),
            Some(i as f64 * 5.0),
        );
    }
    log.observe(
        None,
        "",
        "poll",
        4,
        true,
        "BTCUSDT",
        Some("2026-08-29T00:01:00Z"),
        Some(60.0),
    );
    let text = fs::read_to_string(&path).unwrap();
    let events: Vec<_> = text.lines().collect();
    assert!(events[0].contains("\"event\":\"shown\""), "{text}");
    assert!(events.iter().any(|l| l.contains("\"event\":\"still\"")), "{text}");
    assert!(events.last().unwrap().contains("\"event\":\"cleared\""), "{text}");
    assert!(!text.contains("signature="));
}

use std::collections::HashMap;
use tui_bot::config::{load_config, load_dotenv_file, ConfigError, STRATEGY1_POLL_SECONDS, MAINNET_BASE};

#[test]
fn poll_allowed_values() {
    assert!(STRATEGY1_POLL_SECONDS == 60 || STRATEGY1_POLL_SECONDS == 120);
}

#[test]
fn missing_keys_watch_ok_live_refused() {
    let cfg = load_config(false, None, Some(&HashMap::new())).unwrap();
    assert!(cfg.credentials.is_none());
    assert!(!cfg.live);
    assert!(cfg.poll_seconds == 60 || cfg.poll_seconds == 120);
    assert!(matches!(load_config(true, None, Some(&HashMap::new())), Err(ConfigError(_))));
}

#[test]
fn reads_keys_from_environ_only() {
    let mut env = HashMap::new();
    env.insert("BINANCE_API_KEY".into(), "A".repeat(32));
    env.insert("BINANCE_API_SECRET".into(), "B".repeat(32));
    env.insert("STRATEGY1_POLL_SECONDS".into(), "60".into());
    let cfg = load_config(true, None, Some(&env)).unwrap();
    assert_eq!(cfg.credentials.as_ref().unwrap().api_key, "A".repeat(32));
    assert_eq!(cfg.poll_seconds, 60);
}

#[test]
fn refuses_mainnet_without_override() {
    let mut env = HashMap::new();
    env.insert("BINANCE_FAPI_BASE".into(), MAINNET_BASE.into());
    assert!(load_config(false, None, Some(&env)).is_err());
    env.insert("BINANCE_ALLOW_MAINNET".into(), "1".into());
    let cfg = load_config(false, None, Some(&env)).unwrap();
    assert!(cfg.base_url.starts_with(MAINNET_BASE));
}

#[test]
fn rejects_bad_poll_and_http_base() {
    let mut env = HashMap::new();
    env.insert("STRATEGY1_POLL_SECONDS".into(), "30".into());
    assert!(load_config(false, None, Some(&env)).is_err());
    let mut env = HashMap::new();
    env.insert("BINANCE_FAPI_BASE".into(), "http://example.com".into());
    assert!(load_config(false, None, Some(&env)).is_err());
    let mut env = HashMap::new();
    env.insert("BINANCE_API_KEY".into(), "only-one-side-present-here".into());
    assert!(load_config(false, None, Some(&env)).is_err());
}

#[test]
fn dotenv_parser_ignores_junk() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("_sample.env");
    std::fs::write(&path, "# comment\nexport FOO=bar\nBINANCE_API_KEY=\"abcd\"\nnot a line\n").unwrap();
    let vals = load_dotenv_file(&path);
    assert_eq!(vals.get("FOO").map(String::as_str), Some("bar"));
    assert_eq!(vals.get("BINANCE_API_KEY").map(String::as_str), Some("abcd"));
}

#[test]
fn default_base_is_testnet() {
    let cfg = load_config(false, None, Some(&HashMap::new())).unwrap();
    assert_eq!(cfg.base_url, tui_bot::config::DEFAULT_TESTNET_BASE);
}

#[test]
fn entry_hours_default_and_always_enter() {
    use rust_decimal::Decimal;
    use tui_bot::sessions::DEFAULT_ENTRY_WINDOWS;

    let cfg = load_config(false, None, Some(&HashMap::new())).unwrap();
    assert_eq!(cfg.entry_windows, DEFAULT_ENTRY_WINDOWS.to_vec());
    assert!(!cfg.always_enter);
    assert_eq!(cfg.s4_entry_windows, DEFAULT_ENTRY_WINDOWS.to_vec());
    assert!(!cfg.s4_always_enter);

    let mut env = HashMap::new();
    env.insert("STRATEGY1_ALWAYS_ENTER".into(), "1".into());
    let always = load_config(false, None, Some(&env)).unwrap();
    assert!(always.always_enter);
    assert!(always.entry_windows.is_empty());

    let mut env = HashMap::new();
    env.insert("STRATEGY1_ENTRY_HOURS".into(), "13-16".into());
    let custom = load_config(false, None, Some(&env)).unwrap();
    assert_eq!(custom.entry_windows, vec![(13, 16)]);

    let mut env = HashMap::new();
    env.insert("STRATEGY1_ENTRY_HOURS".into(), "nope".into());
    assert!(load_config(false, None, Some(&env)).is_err());

    let mut env = HashMap::new();
    env.insert("STRATEGY1_ALWAYS_ENTER".into(), "1".into());
    let s1_only = load_config(false, None, Some(&env)).unwrap();
    assert!(s1_only.always_enter);
    assert!(s1_only.entry_windows.is_empty());
    assert!(!s1_only.s4_always_enter);
    assert_eq!(s1_only.s4_entry_windows, DEFAULT_ENTRY_WINDOWS.to_vec());

    env.insert("STRATEGY4_ALWAYS_ENTER".into(), "1".into());
    let both = load_config(false, None, Some(&env)).unwrap();
    assert!(both.s4_always_enter);
    assert!(both.s4_entry_windows.is_empty());

    let mut env = HashMap::new();
    env.insert("STRATEGY4_ENTRY_HOURS".into(), "7-10".into());
    let s4_custom = load_config(false, None, Some(&env)).unwrap();
    assert_eq!(s4_custom.s4_entry_windows, vec![(7, 10)]);
    assert_eq!(s4_custom.entry_windows, DEFAULT_ENTRY_WINDOWS.to_vec());
    assert!(!s4_custom.s4_always_enter);

    let mut env = HashMap::new();
    env.insert("ORDER_NOTIONAL_USDT".into(), "40".into());
    let sized = load_config(false, None, Some(&env)).unwrap();
    assert_eq!(sized.order_notional, Decimal::from(40));
    assert!(!sized.notional_from_exchange);
}

#[test]
fn leverage_and_notional_options() {
    use rust_decimal::Decimal;

    let unset = load_config(false, None, Some(&HashMap::new())).unwrap();
    assert!(unset.leverage.is_none());
    assert!(!unset.notional_from_exchange);
    assert_eq!(unset.max_positions, 1);
    assert_eq!(unset.daily_loss_usdt, Decimal::from(20));
    assert_eq!(unset.order_notional, Decimal::from(20));

    let mut env = HashMap::new();
    env.insert("FUTURES_LEVERAGE".into(), "5".into());
    let set_lev = load_config(false, None, Some(&env)).unwrap();
    assert_eq!(set_lev.leverage, Some(5));

    let mut env = HashMap::new();
    env.insert("ORDER_NOTIONAL_USDT".into(), "binance".into());
    let exchange = load_config(false, None, Some(&env)).unwrap();
    assert!(exchange.notional_from_exchange);

    let mut env = HashMap::new();
    env.insert("FUTURES_LEVERAGE".into(), "999".into());
    assert!(load_config(false, None, Some(&env)).is_err());

    let mut env = HashMap::new();
    env.insert("STRATEGY1_MAX_POSITIONS".into(), "0".into());
    assert!(load_config(false, None, Some(&env)).is_err());

    let mut env = HashMap::new();
    env.insert("STRATEGY1_MAX_POSITIONS".into(), "3".into());
    let three = load_config(false, None, Some(&env)).unwrap();
    assert_eq!(three.max_positions, 3);
}

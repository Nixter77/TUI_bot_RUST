use rust_decimal::Decimal;
use serde_json::json;
use tui_bot::models::Ticker;
use tui_bot::ranking::{
    is_tradable_symbol, parse_tickers, pick_strategy1_book, pick_strategy3_book, rank_most_rising,
};

#[test]
fn picks_highest_percent_usdt() {
    let raw = json!([
        {"symbol":"ETHUSDT","lastPrice":"3000","priceChangePercent":"3.1","quoteVolume":"2000"},
        {"symbol":"BTCUSDT","lastPrice":"60000","priceChangePercent":"8.5","quoteVolume":"9000"},
        {"symbol":"DOGEUSDT","lastPrice":"0.1","priceChangePercent":"8.5","quoteVolume":"100"},
        {"symbol":"BTCBUSD","lastPrice":"1","priceChangePercent":"50","quoteVolume":"1"},
        {"symbol":"BAD","lastPrice":"oops","priceChangePercent":"1","quoteVolume":"1"},
    ]);
    let tickers = parse_tickers(&raw);
    let winner = rank_most_rising(&tickers, &[]).unwrap();
    assert_eq!(winner.symbol, "BTCUSDT");
    assert_eq!(winner.price_change_percent, Decimal::new(85, 1));
}

#[test]
fn empty_and_invalid_yield_none() {
    assert!(parse_tickers(&json!({"not":"a list"})).is_empty());
    assert!(rank_most_rising(&[], &[]).is_none());
    assert!(rank_most_rising(
        &[Ticker::new(
            "BTCUSDT",
            Decimal::ZERO,
            Decimal::from(10),
            Decimal::ONE
        )],
        &[]
    )
    .is_none());
}

#[test]
fn skips_non_ascii_and_non_usdt_junk() {
    assert!(!is_tradable_symbol("测试测试USDT"));
    assert!(!is_tradable_symbol("BTCBUSD"));
    assert!(is_tradable_symbol("BTCUSDT"));
    assert!(!is_tradable_symbol("FARTCOINUSDT"));
    assert!(!is_tradable_symbol("1000PEPEUSDT"));
}

#[test]
fn momentum_book_is_majors_only_under_24h_cap() {
    let tickers = vec![
        Ticker::new(
            "GPSUSDT",
            "0.01".parse().unwrap(),
            Decimal::from(50),
            Decimal::from(90_000),
        ),
        Ticker::new(
            "MORPHOUSDT",
            "2.8".parse().unwrap(),
            Decimal::from(26),
            Decimal::from(80_000),
        ),
        Ticker::new(
            "STORJUSDT",
            "0.05".parse().unwrap(),
            Decimal::new(95, 1),
            Decimal::from(70_000),
        ),
        Ticker::new(
            "BTCUSDT",
            Decimal::from(50000),
            Decimal::new(9, 0),
            Decimal::from(800_000),
        ),
        Ticker::new(
            "ETHUSDT",
            Decimal::from(3000),
            Decimal::new(4, 0),
            Decimal::from(100_000),
        ),
        Ticker::new(
            "SOLUSDT",
            Decimal::from(140),
            Decimal::from(15),
            Decimal::from(200_000),
        ),
        Ticker::new(
            "XAUUSDT",
            Decimal::from(2400),
            Decimal::from(80),
            Decimal::from(100_000),
        ),
    ];
    let book = pick_strategy1_book(&tickers, 3, &["XAUUSDT".into()]);
    let syms: Vec<_> = book.iter().map(|t| t.symbol.as_str()).collect();
    assert_eq!(syms, vec!["BTCUSDT", "ETHUSDT"]);
}

#[test]
fn already_pumped_and_near_high_are_not_the_buy_list() {
    let mut spk = Ticker::new(
        "SPKUSDT",
        "0.022".parse().unwrap(),
        "25.4".parse().unwrap(),
        Decimal::from(200_000),
    );
    spk.high_price = "0.022".parse().unwrap();
    let tickers = vec![
        spk,
        Ticker::new(
            "BTCUSDT",
            Decimal::from(50000),
            Decimal::new(8, 1),
            Decimal::from(800_000),
        ),
    ];
    let book = pick_strategy1_book(&tickers, 3, &[]);
    assert_eq!(
        book.iter().map(|t| t.symbol.as_str()).collect::<Vec<_>>(),
        vec!["BTCUSDT"]
    );
}

#[test]
fn strategy3_book_is_liquid_usdt_not_three_majors() {
    let tickers = vec![
        Ticker::new(
            "1000PEPEUSDT",
            "0.01".parse().unwrap(),
            Decimal::from(40),
            Decimal::from(9_000_000),
        ),
        Ticker::new(
            "AVAXUSDT",
            Decimal::from(20),
            Decimal::from(2),
            Decimal::from(50_000_000),
        ),
        Ticker::new(
            "BTCUSDT",
            Decimal::from(50000),
            Decimal::from(1),
            Decimal::from(800_000),
        ),
        Ticker::new(
            "LINKUSDT",
            Decimal::from(15),
            Decimal::from(3),
            Decimal::from(5_000_000),
        ),
        Ticker::new(
            "FARTCOINUSDT",
            Decimal::from(1),
            Decimal::from(80),
            Decimal::from(2_000_000),
        ),
        Ticker::new(
            "XAUUSDT",
            Decimal::from(2400),
            Decimal::from(5),
            Decimal::from(3_000_000),
        ),
    ];
    let book = pick_strategy3_book(&tickers, &["XAUUSDT".into()]);
    let syms: Vec<_> = book.iter().map(|t| t.symbol.as_str()).collect();
    // Majors first (BTC), then volume (AVAX 50M > LINK 5M). TestNet volume
    // must not bury BTC behind alts.
    assert_eq!(syms, vec!["BTCUSDT", "AVAXUSDT", "LINKUSDT"]);
    assert!(!syms
        .iter()
        .any(|s| s.contains("PEPE") || s.contains("FART")));
    assert!(!syms.contains(&"XAUUSDT"));
}

#[test]
fn strategy3_book_skips_penny_pumps() {
    let tickers = vec![
        Ticker::new(
            "AVAXUSDT",
            Decimal::from(20),
            Decimal::from(2),
            Decimal::from(5_000_000),
        ),
        Ticker::new(
            "AINUSDT",
            "0.17".parse().unwrap(),
            Decimal::from(34),
            Decimal::from(8_000_000_000u64),
        ),
        Ticker::new(
            "POWERUSDT",
            "0.18".parse().unwrap(),
            Decimal::from(29),
            Decimal::from(7_000_000_000u64),
        ),
        Ticker::new(
            "BTCUSDT",
            Decimal::from(50000),
            Decimal::from(1),
            Decimal::from(800_000),
        ),
    ];
    let book = pick_strategy3_book(&tickers, &[]);
    let syms: Vec<_> = book.iter().map(|t| t.symbol.as_str()).collect();
    assert_eq!(syms, vec!["BTCUSDT", "AVAXUSDT"]);
    assert!(!syms.contains(&"AINUSDT"));
    assert!(!syms.contains(&"POWERUSDT"));
}

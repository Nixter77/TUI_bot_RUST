//! Phase-2 BTC regime: classifier + S4/S1 entry gate.
use rust_decimal::Decimal;
use tui_bot::continuation::{s4_setup_skip, ContinuationParams};
use tui_bot::models::{Bar, MarketSnapshot, Ticker};
use tui_bot::regime::{block_alt_entry, classify_btc, classify_snapshot, effective_risk_pct, BtcRegime};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn bar_at(i: i64, close: f64) -> Bar {
    let c = Decimal::from_str_exact(&format!("{close:.2}")).unwrap();
    Bar {
        open_time: 1_700_000_000_000 + i * 4 * 3_600_000,
        open: c,
        high: c + d("80"),
        low: c - d("80"),
        close: c,
        volume: d("100"),
    }
}

fn rising(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| bar_at(i as i64, 40_000.0 + 90.0 * i as f64))
        .collect()
}

fn falling(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| bar_at(i as i64, 55_000.0 - 140.0 * i as f64))
        .collect()
}

#[test]
fn classifier_risk_multipliers() {
    assert_eq!(BtcRegime::Bull.risk_multiplier(), Decimal::ONE);
    assert_eq!(BtcRegime::Neutral.risk_multiplier(), d("0.5"));
    assert_eq!(BtcRegime::Bear.risk_multiplier(), Decimal::ZERO);
}

#[test]
fn missing_data_neutral_allows_half_risk() {
    let snap = MarketSnapshot::empty(d("10000"));
    assert_eq!(classify_snapshot(&snap), BtcRegime::Neutral);
    assert!(block_alt_entry(&snap).is_none());
    assert_eq!(effective_risk_pct(d("0.0025"), &snap), d("0.00125"));
}

#[test]
fn bear_htf_blocks_s4_setup_skip() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    let htf = falling(40);
    let px = htf.last().unwrap().close;
    snap.htf_bars.insert("BTCUSDT".into(), htf);
    snap.tickers.push(Ticker::new("BTCUSDT", px, d("-3"), d("1e9")));
    // Dummy liquid alt so setup_skip has something to evaluate past regime.
    snap.tickers.push(Ticker::new("AVAXUSDT", d("100"), d("4"), d("5e8")));
    let p = ContinuationParams::default();
    let alt = snap.tickers.iter().find(|t| t.symbol == "AVAXUSDT").unwrap();
    let skip = s4_setup_skip(&snap, alt, &p, &[]);
    assert!(
        skip.as_deref() == Some("BTC regime bear — не вхожу"),
        "{skip:?}"
    );
}

#[test]
fn bull_htf_does_not_regime_block() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    let htf = rising(40);
    let px = htf.last().unwrap().close;
    snap.htf_bars.insert("BTCUSDT".into(), htf);
    snap.tickers.push(Ticker::new("BTCUSDT", px, d("2"), d("1e9")));
    assert!(block_alt_entry(&snap).is_none());
    let reg = classify_snapshot(&snap);
    assert!(
        matches!(reg, BtcRegime::Bull | BtcRegime::StrongBull | BtcRegime::Neutral),
        "{reg:?}"
    );
    assert_eq!(effective_risk_pct(d("0.01"), &snap), d("0.01"));
}

#[test]
fn panic_reason_distinct() {
    let htf = falling(40);
    let px = htf.last().unwrap().close;
    let reg = classify_btc(&htf, Some(px), Some(d("-2.5")));
    assert_eq!(reg, BtcRegime::Panic);
    assert_eq!(reg.block_reason(), Some("BTC regime panic — не вхожу"));
}

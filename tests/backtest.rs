//! Drive shipped simulate_bars / strategy 4 report fields.

mod common;
use common::*;
use rust_decimal::Decimal;
use tui_bot::sim::simulate_bars;

#[test]
fn strategy4_simulate_bars_emits_pnl_fields() {
    let bars = stair();
    let result = simulate_bars(
        4,
        &bars,
        "BTCUSDT",
        "cont BTCUSDT",
        Decimal::from(20),
        Decimal::new(4, 4),
        Decimal::new(1, 4),
        Some(40),
        Decimal::from(1000),
        None,
        None,
        None,
    );
    assert_eq!(result.strategy_id, 4);
    let line = result.summary_line();
    assert!(line.contains("cont BTCUSDT"), "{line}");
    assert!(line.contains("n="), "{line}");
    assert!(line.contains("wr="), "{line}");
    assert!(line.contains("pnl="), "{line}");
    assert!(line.contains("pf="), "{line}");
    if result.trades.is_empty() {
        assert!(line.contains("n=   0") || line.contains("n=0"), "{line}");
    }
}

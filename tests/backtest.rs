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

fn load_cached_klines(symbol: &str, interval: &str) -> Option<Vec<tui_bot::models::Bar>> {
    use serde_json::Value;
    use std::fs;
    use tui_bot::models::bar_from_kline;
    let paths = [
        format!(".state/klines/{symbol}_{interval}.json"),
        format!(".state/klines_measure/{symbol}_{interval}_500.json"),
        format!(".state/klines_measure/{symbol}_{interval}_200.json"),
    ];
    for path in paths {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(list) = v.as_array() else {
            continue;
        };
        let bars: Vec<_> = list.iter().filter_map(|r| bar_from_kline(r).ok()).collect();
        if !bars.is_empty() {
            return Some(bars);
        }
    }
    None
}

#[test]
fn s5_cached_1h_sim_metric_if_present() {
    use rust_decimal::Decimal;
    use tui_bot::engine::MomentumParams;
    use tui_bot::sim::{simulate_bars_opts, SimOpts};
    // Core desk + crate-report alts already on disk (no fapi fetch). Skip ZEC/DASH privacy.
    let names = [
        "LINKUSDT",
        "AVAXUSDT",
        "DOGEUSDT",
        "ADAUSDT",
        "NEARUSDT",
        "AAVEUSDT",
        "SUIUSDT",
        "UNIUSDT",
        "LTCUSDT",
        "TAOUSDT",
    ];
    let mut n = 0usize;
    let mut wins = 0usize;
    let mut pnl = Decimal::ZERO;
    let mom = MomentumParams {
        s4_always_enter: true,
        s4_interval: tui_bot::config::TradeInterval::Minute15,
        cooldown_sec: 0.0,
        ..MomentumParams::default()
    };
    let btc = load_cached_klines("BTCUSDT", "4h");
    for symbol in names {
        let Some(bars) = load_cached_klines(symbol, "1h") else {
            continue;
        };
        if bars.len() < 50 {
            continue;
        }
        let htf = load_cached_klines(symbol, "4h");
        let opts = SimOpts {
            htf: htf.as_deref(),
            btc_htf: btc.as_deref(),
        };
        let row = simulate_bars_opts(
            5,
            &bars,
            symbol,
            &format!("S5 {symbol} 1h"),
            Decimal::from(20),
            Decimal::new(4, 4),
            Decimal::new(1, 4),
            Some(40),
            Decimal::from(1000),
            Some(&mom),
            None,
            None,
            opts,
        );
        n += row.trades.len();
        wins += row.wins();
        pnl += row.pnl();
        eprintln!("{}", row.summary_line());
    }
    if n == 0 {
        return;
    }
    let wr = wins as f64 / n as f64 * 100.0;
    eprintln!("S5 cache totals n={n} wr={wr:.1}% pnl={pnl}");
}

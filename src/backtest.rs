//! Public-klines profitability report. No keys, no orders.

use crate::engine::MomentumParams;
use crate::models::{bar_from_kline, Bar};
use crate::scalp::ScalpParams;
use crate::sim::{simulate_bars, SimResult};
use crate::trend::TrendParams;
use rust_decimal::Decimal;
use serde_json::Value;
use std::fs;
use std::path::Path;

const PUBLIC_FAPI: &str = "https://fapi.binance.com";
const CACHE_DIR: &str = ".state/klines";

fn fixture_bars(n: usize, start: f64, step: f64, interval_ms: i64) -> Vec<Bar> {
    let mut bars = Vec::new();
    let mut price = start;
    for i in 0..n {
        let nxt = price + step;
        let o = Decimal::from_str_exact(&format!("{price:.4}")).unwrap_or(Decimal::from(100));
        let c = Decimal::from_str_exact(&format!("{nxt:.4}")).unwrap_or(Decimal::from(100));
        let high = o.max(c) + Decimal::new(4, 2);
        let low = o.min(c) - Decimal::new(4, 2);
        bars.push(Bar {
            open_time: 1_700_000_000_000 + i as i64 * interval_ms,
            open: o,
            high,
            low,
            close: c,
            volume: Decimal::from(20),
        });
        price = nxt;
    }
    bars
}

fn bars_from_raw(raw: &Value) -> Vec<Bar> {
    let Some(list) = raw.as_array() else {
        return Vec::new();
    };
    let mut bars = Vec::new();
    for row in list {
        if let Ok(b) = bar_from_kline(row) {
            bars.push(b);
        }
    }
    bars
}

fn load_cached(symbol: &str, interval: &str) -> Option<Vec<Bar>> {
    let path = format!("{CACHE_DIR}/{symbol}_{interval}.json");
    let text = fs::read_to_string(path).ok()?;
    let raw: Value = serde_json::from_str(&text).ok()?;
    let bars = bars_from_raw(&raw);
    if bars.is_empty() {
        None
    } else {
        Some(bars)
    }
}

fn fetch_klines(symbol: &str, interval: &str) -> Option<Vec<Bar>> {
    if let Some(bars) = load_cached(symbol, interval) {
        return Some(bars);
    }
    let url = format!("{PUBLIC_FAPI}/fapi/v1/klines?symbol={symbol}&interval={interval}&limit=500");
    let resp = ureq::get(&url)
        .set("User-Agent", "tui-bot-rust/backtest")
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .ok()?;
    let raw: Value = resp.into_json().ok()?;
    let bars = bars_from_raw(&raw);
    if bars.is_empty() {
        return None;
    }
    let _ = fs::create_dir_all(CACHE_DIR);
    if let Ok(text) = serde_json::to_string(&raw) {
        let _ = fs::write(format!("{CACHE_DIR}/{symbol}_{interval}.json"), text);
    }
    Some(bars)
}

fn format_packed(rows: &[SimResult]) -> String {
    let mut lines = vec![
        "home-economic backtest (Binance USDT-M public klines)".to_string(),
        "это НЕ TestNet: свечи без ордеров, fee=0.04% taker/side, notional=20 USDT.".into(),
        "momentum/scalp = 5m; trend = Donchian 20/10; continuation = liquid 5m, sessions ON.".into(),
        String::new(),
        "=== L4 shipped defaults ===".into(),
    ];
    for row in rows {
        lines.push(format!("  {}", row.summary_line()));
    }
    let pnl: Decimal = rows.iter().map(|r| r.pnl()).sum();
    let n: usize = rows.iter().map(|r| r.trades.len()).sum();
    lines.push(String::new());
    lines.push(format!("  totals trades={n}  pnl={pnl:+.4}"));
    lines.push(String::new());
    lines.join("\n")
}

pub fn run_cli() -> i32 {
    eprintln!("fetching public klines (cached under .state/klines/)…");
    let mut universe: Vec<(String, Vec<Bar>)> = Vec::new();
    for symbol in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        if let Some(bars) = fetch_klines(symbol, "5m") {
            universe.push((symbol.into(), bars));
        }
    }
    if universe.is_empty() {
        eprintln!("network/cache empty — walking in-process fixture klines (no orders)");
        universe.push((
            "BTCUSDT".into(),
            fixture_bars(200, 100.0, 0.05, 300_000),
        ));
    }
    let mut rows = Vec::new();
    let mom = MomentumParams {
        always_enter: true,
        cooldown_sec: 0.0,
        ..MomentumParams::default()
    };
    for (symbol, bars) in &universe {
        rows.push(simulate_bars(
            1,
            bars,
            symbol,
            &format!("mom {symbol}"),
            Decimal::from(20),
            Decimal::new(4, 4),
            Decimal::new(1, 4),
            Some(40),
            Decimal::from(1000),
            Some(&mom),
            None,
            None,
        ));
        rows.push(simulate_bars(
            2,
            bars,
            symbol,
            &format!("scalp {symbol}"),
            Decimal::from(20),
            Decimal::new(4, 4),
            Decimal::new(1, 4),
            Some(80),
            Decimal::from(1000),
            None,
            Some(&ScalpParams::default()),
            None,
        ));
        rows.push(simulate_bars(
            3,
            bars,
            symbol,
            &format!("trend {symbol}"),
            Decimal::from(20),
            Decimal::new(4, 4),
            Decimal::new(1, 4),
            Some(70),
            Decimal::from(1000),
            None,
            None,
            Some(&TrendParams::default()),
        ));
        rows.push(simulate_bars(
            4,
            bars,
            symbol,
            &format!("cont {symbol}"),
            Decimal::from(20),
            Decimal::new(4, 4),
            Decimal::new(1, 4),
            Some(40),
            Decimal::from(1000),
            None,
            None,
            None,
        ));
    }
    let text = format_packed(&rows);
    print!("{text}");
    if let Some(parent) = Path::new(".state").to_str() {
        let _ = fs::create_dir_all(parent);
        let _ = fs::write(".state/backtest-report.txt", &text);
    }
    0
}

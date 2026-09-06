//! Public-klines profitability report. No keys, no orders.

use crate::engine::MomentumParams;
use crate::models::{bar_from_kline, Bar};
use crate::scalp::ScalpParams;
use crate::sim::{simulate_bars, SimResult};
use crate::trend::TrendParams;
use rust_decimal::Decimal;
use serde_json::Value;
use std::fs;

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
    let url = format!("{PUBLIC_FAPI}/fapi/v1/klines?symbol={symbol}&interval={interval}&limit=1500");
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
        "momentum/scalp = 5m; trend = Donchian 20/10; continuation = STRATEGY4_INTERVAL (5m/15m/30m/1h).".into(),
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

fn dump_chart_json(rows: &[SimResult]) {
    use serde_json::json;
    let mut series = Vec::new();
    for row in rows {
        let mut eq = row.start_equity;
        let mut curve = vec![json!({"i": 0, "equity": eq.to_string(), "pnl": "0"})];
        for (i, t) in row.trades.iter().enumerate() {
            eq += t.pnl;
            curve.push(json!({
                "i": i + 1,
                "equity": eq.to_string(),
                "pnl": t.pnl.to_string(),
                "symbol": t.symbol,
                "reason": t.reason,
            }));
        }
        series.push(json!({
            "name": row.name,
            "strategy_id": row.strategy_id,
            "trades": row.trades.len(),
            "pnl": row.pnl().to_string(),
            "max_dd": row.max_drawdown.to_string(),
            "curve": curve,
        }));
    }
    let payload = json!({ "series": series });
    let _ = fs::create_dir_all(".state");
    let _ = fs::write(".state/backtest-chart.json", payload.to_string());
}

pub fn run_cli() -> i32 {
    use crate::config::TradeInterval;
    use crate::sim::{simulate_bars_opts, SimOpts};

    eprintln!("fetching public klines (cached under .state/klines/)…");
    // Majors for S1–S3; alts for S4/S5 (continuation skips BTC/ETH/SOL/BNB/XRP/BCH).
    let majors = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];
    let alts = ["LINKUSDT", "AVAXUSDT", "DOGEUSDT", "ADAUSDT", "NEARUSDT"];
    for symbol in majors.iter().chain(alts.iter()) {
        for iv in ["5m", "15m", "1h", "4h"] {
            let _ = fs::remove_file(format!("{CACHE_DIR}/{symbol}_{iv}.json"));
        }
    }

    let mut univ_5m: Vec<(String, Vec<Bar>)> = Vec::new();
    let mut univ_15m: Vec<(String, Vec<Bar>)> = Vec::new();
    let mut univ_1h: Vec<(String, Vec<Bar>)> = Vec::new();
    let mut htf_4h: std::collections::HashMap<String, Vec<Bar>> = std::collections::HashMap::new();

    for symbol in majors {
        if let Some(bars) = fetch_klines(symbol, "5m") {
            univ_5m.push((symbol.into(), bars));
        }
    }
    for symbol in alts {
        if let Some(bars) = fetch_klines(symbol, "15m") {
            univ_15m.push((symbol.into(), bars));
        }
        if let Some(bars) = fetch_klines(symbol, "1h") {
            univ_1h.push((symbol.into(), bars));
        }
        if let Some(bars) = fetch_klines(symbol, "4h") {
            htf_4h.insert(symbol.into(), bars);
        }
    }
    let btc_htf = fetch_klines("BTCUSDT", "4h");

    if univ_5m.is_empty() && univ_15m.is_empty() && univ_1h.is_empty() {
        eprintln!("network/cache empty — walking in-process fixture klines (no orders)");
        let fx = fixture_bars(200, 100.0, 0.05, 300_000);
        univ_5m.push(("BTCUSDT".into(), fx.clone()));
        univ_15m.push(("LINKUSDT".into(), fx.clone()));
        univ_1h.push(("LINKUSDT".into(), fx));
    }

    let mut rows = Vec::new();
    let mom = MomentumParams {
        always_enter: true,
        cooldown_sec: 0.0,
        ..MomentumParams::default()
    };
    let cont_mom = MomentumParams {
        s4_always_enter: true,
        s4_interval: TradeInterval::Minute15,
        cooldown_sec: 0.0,
        ..MomentumParams::default()
    };

    for (symbol, bars) in &univ_5m {
        rows.push(simulate_bars(
            1, bars, symbol, &format!("mom {symbol} 5m"),
            Decimal::from(20), Decimal::new(4, 4), Decimal::new(1, 4),
            Some(40), Decimal::from(1000), Some(&mom), None, None,
        ));
        rows.push(simulate_bars(
            2, bars, symbol, &format!("scalp {symbol} 5m"),
            Decimal::from(20), Decimal::new(4, 4), Decimal::new(1, 4),
            Some(80), Decimal::from(1000), None, Some(&ScalpParams::default()), None,
        ));
        rows.push(simulate_bars(
            3, bars, symbol, &format!("trend {symbol} 5m"),
            Decimal::from(20), Decimal::new(4, 4), Decimal::new(1, 4),
            Some(70), Decimal::from(1000), None, None, Some(&TrendParams::default()),
        ));
    }

    for (symbol, bars) in &univ_15m {
        let htf = htf_4h.get(symbol).map(|v| v.as_slice());
        let opts = SimOpts {
            htf,
            btc_htf: btc_htf.as_deref(),
        };
        rows.push(simulate_bars_opts(
            4, bars, symbol, &format!("S4 cont {symbol} 15m"),
            Decimal::from(20), Decimal::new(4, 4), Decimal::new(1, 4),
            Some(40), Decimal::from(1000), Some(&cont_mom), None, None, opts,
        ));
    }
    for (symbol, bars) in &univ_1h {
        let htf = htf_4h.get(symbol).map(|v| v.as_slice());
        let opts = SimOpts {
            htf,
            btc_htf: btc_htf.as_deref(),
        };
        rows.push(simulate_bars_opts(
            5, bars, symbol, &format!("S5 verify {symbol} 1h"),
            Decimal::from(20), Decimal::new(4, 4), Decimal::new(1, 4),
            Some(40), Decimal::from(1000), Some(&cont_mom), None, None, opts,
        ));
    }

    let text = format_packed(&rows);
    print!("{text}");
    let _ = fs::create_dir_all(".state");
    let _ = fs::write(".state/backtest-report.txt", &text);
    dump_chart_json(&rows);
    0
}

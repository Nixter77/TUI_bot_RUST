//! Public-klines profitability report. No keys, no orders.

use crate::engine::MomentumParams;
use crate::models::{bar_from_kline, Bar};
use crate::scalp::ScalpParams;
use crate::sim::{simulate_bars, SimResult};
use crate::trend::TrendParams;
use crate::continuation::ContinuationParams;
use std::env;
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
    // Write a sentinel file to indicate backtest started (useful for debugging
    // cases where redirected stdout is missing). This helps confirm the binary
    // ran and which env flags were set.
    let _ = std::fs::create_dir_all(".state");
    let _ = std::fs::write(
        format!(".state/run-sentinel-{}.txt", crate::sessions::unix_now() as i64),
        format!("DUMP_S5={} SWEEP_S4S5={}", std::env::var("DUMP_S5").is_ok(), std::env::var("SWEEP_S4S5").is_ok()),
    );
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
    // Quick per-symbol S5 dump for debugging: write trade lists to .state
    if env::var("DUMP_S5").is_ok() {
        let cont_mom = MomentumParams {
            s4_always_enter: true,
            s4_interval: TradeInterval::Minute15,
            cooldown_sec: 0.0,
            ..MomentumParams::default()
        };
        let _ = fs::create_dir_all(".state");
        for (symbol, bars) in &univ_1h {
            let htf = htf_4h.get(symbol).map(|v| v.as_slice());
            let opts = SimOpts { htf, btc_htf: btc_htf.as_deref() };
            let res = simulate_bars_opts(
                5, bars, symbol, &format!("S5 debug {symbol} 1h"),
                Decimal::from(20), Decimal::new(4,4), Decimal::new(1,4),
                Some(40), Decimal::from(1000), Some(&cont_mom), None, None, opts, None,
            );
            let mut s = String::new();
            s.push_str(&format!("{}\n", res.summary_line()));
            for t in &res.trades {
                s.push_str(&format!("{} {} -> {} qty={} pnl={} reason={}\n", t.symbol, t.entry, t.exit, t.qty, t.pnl, t.reason));
            }
            let _ = fs::write(format!(".state/s5-{}-trades.txt", symbol), s);
        }
        return 0;
    }
    // Sweep mode: run grid search for Continuation (S4/S5) params when env set.
    if env::var("SWEEP_S4S5").is_ok() {
        use rust_decimal::Decimal;
        let mins = vec![Decimal::new(5, 1), Decimal::new(10, 1), Decimal::new(20, 1), Decimal::new(40, 1)]; // 0.5%,1%,2%,4%
        let atrks = vec![Decimal::new(15, 1), Decimal::from(2), Decimal::new(25, 1)]; // 1.5,2,2.5
        let rewards = vec![Decimal::new(15, 1), Decimal::from(2), Decimal::new(25, 1)]; // 1.5,2,2.5 R
        #[derive(Debug)]
        struct SweepRow { min: Decimal, atrk: Decimal, reward: Decimal, pnl: Decimal, trades: usize }
        let mut out: Vec<SweepRow> = Vec::new();
        let mut debug_lines: Vec<String> = Vec::new();
        debug_lines.push(format!("univ_15m_len={} univ_1h_len={}", univ_15m.len(), univ_1h.len()));
        for min_c in &mins {
            for atr_k in &atrks {
                for r in &rewards {
                    let mut total_pnl = Decimal::ZERO;
                    let mut total_trades = 0usize;
                    for (symbol, bars) in &univ_15m {
                        debug_lines.push(format!("S4 sweep symbol={symbol} bars={} min={}% atr_k={} reward={}", bars.len(), min_c, atr_k, r));
                        let mut p = ContinuationParams::default().with_interval(crate::config::TradeInterval::Minute15);
                        p.min_change_percent = *min_c;
                        p.atr_k = *atr_k;
                        p.reward_r = *r;
                        // relaxed debug mode: force entry windows and lower liquidity to see if
                        // filters are the reason for zero trades
                        p.always_enter = true;
                        p.min_quote_volume = Decimal::ZERO;
                        p.volume_confirm_frac = Decimal::ZERO;
                        p.min_pullback_pct = Decimal::ZERO;
                        p.week_leader_pct = Decimal::ZERO;
                        p.near_high_frac = Decimal::from(1000);
                        p.min_price = Decimal::ZERO;
                        p.max_change_percent = None;
                        p.liquid_frac = Decimal::ZERO;
                        p.liquid_n = 100;
                        p.entry_windows = Vec::new();
                        p.max_positions = 100;
                        let htf = htf_4h.get(symbol).map(|v| v.as_slice());
                        let opts = crate::sim::SimOpts { htf, btc_htf: btc_htf.as_deref() };
                        let res = simulate_bars_opts(
                            4, bars, symbol, &format!("S4 sweep {symbol} 15m"),
                            Decimal::from(20), Decimal::new(4, 4), Decimal::new(1, 4),
                            Some(40), Decimal::from(1000), None, None, None, opts,
                            Some(&p),
                        );
                        // write per-symbol sweep debug file
                        let _ = std::fs::create_dir_all(".state");
                        let fname = format!(".state/sweep-15m-{}-min{}-atrk{}-r{}.txt",
                            symbol,
                            min_c.to_string().replace('.', "p"),
                            atr_k.to_string().replace('.', "p"),
                            r.to_string().replace('.', "p"),
                        );
                        let mut dump = String::new();
                        dump.push_str(&format!("{}\n", res.summary_line()));
                        for t in &res.trades {
                            dump.push_str(&format!("{} {} -> {} qty={} pnl={} reason={}\n", t.symbol, t.entry, t.exit, t.qty, t.pnl, t.reason));
                        }
                        let _ = std::fs::write(fname, dump);
                        total_pnl += res.pnl();
                        total_trades += res.trades.len();
                    }
                    for (symbol, bars) in &univ_1h {
                        debug_lines.push(format!("S5 sweep symbol={symbol} bars={} min={}% atr_k={} reward={}", bars.len(), min_c, atr_k, r));
                        let mut p = ContinuationParams::default().with_interval(crate::config::TradeInterval::Hour1);
                        p.min_change_percent = *min_c;
                        p.atr_k = *atr_k;
                        p.reward_r = *r;
                        p.always_enter = true;
                        p.min_quote_volume = Decimal::ZERO;
                        p.volume_confirm_frac = Decimal::ZERO;
                        p.min_pullback_pct = Decimal::ZERO;
                        p.week_leader_pct = Decimal::ZERO;
                        p.near_high_frac = Decimal::from(1000);
                        p.min_price = Decimal::ZERO;
                        p.max_change_percent = None;
                        p.liquid_frac = Decimal::ZERO;
                        p.liquid_n = 100;
                        p.entry_windows = Vec::new();
                        p.max_positions = 100;
                        let htf = htf_4h.get(symbol).map(|v| v.as_slice());
                        let opts = crate::sim::SimOpts { htf, btc_htf: btc_htf.as_deref() };
                        let res = simulate_bars_opts(
                            5, bars, symbol, &format!("S5 sweep {symbol} 1h"),
                            Decimal::from(20), Decimal::new(4, 4), Decimal::new(1, 4),
                            Some(40), Decimal::from(1000), None, None, None, opts,
                            Some(&p),
                        );
                        // write per-symbol sweep debug file
                        let _ = std::fs::create_dir_all(".state");
                        let fname = format!(".state/sweep-1h-{}-min{}-atrk{}-r{}.txt",
                            symbol,
                            min_c.to_string().replace('.', "p"),
                            atr_k.to_string().replace('.', "p"),
                            r.to_string().replace('.', "p"),
                        );
                        let mut dump = String::new();
                        dump.push_str(&format!("{}\n", res.summary_line()));
                        for t in &res.trades {
                            dump.push_str(&format!("{} {} -> {} qty={} pnl={} reason={}\n", t.symbol, t.entry, t.exit, t.qty, t.pnl, t.reason));
                        }
                        let _ = std::fs::write(fname, dump);
                        total_pnl += res.pnl();
                        total_trades += res.trades.len();
                    }
                    out.push(SweepRow { min: *min_c, atrk: *atr_k, reward: *r, pnl: total_pnl, trades: total_trades });
                }
            }
        }
        out.sort_by(|a,b| b.pnl.cmp(&a.pnl));
        let mut s = String::new();
        s.push_str("S4/S5 sweep results:\n");
        for row in out.iter().take(20) {
            s.push_str(&format!(" min={}% atr_k={} reward={} pnl={} trades={}\n", row.min, row.atrk, row.reward, row.pnl, row.trades));
        }
        let _ = std::fs::write(".state/sweep-report.txt", &s);
        let _ = std::fs::write(".state/sweep-debug.txt", debug_lines.join("\n"));
        print!("{s}");
        return 0;
    }
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
            Some(40), Decimal::from(1000), Some(&cont_mom), None, None, opts, None,
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
            Some(40), Decimal::from(1000), Some(&cont_mom), None, None, opts, None,
        ));
    }

    let text = format_packed(&rows);
    print!("{text}");
    let _ = fs::create_dir_all(".state");
    let _ = fs::write(".state/backtest-report.txt", &text);
    dump_chart_json(&rows);
    0
}

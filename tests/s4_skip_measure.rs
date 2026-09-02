//! Measurement-only: S4 book skip mix (near_high vs tape). Does not change thresholds.
mod common;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tui_bot::continuation::{
    liquid_universe, pick_strategy4_book, s4_skip_stats_top, ContinuationParams,
};
use tui_bot::s4stats::flush_s4_skip_stats;
use tui_bot::models::{near_24h_high, Ticker};

fn mk(sym: &str, last: &str, chg: &str, vol: &str, high: &str) -> Ticker {
    let mut t = Ticker::new(
        sym,
        last.parse().unwrap(),
        chg.parse().unwrap(),
        vol.parse().unwrap(),
    );
    t.high_price = high.parse().unwrap();
    t
}

#[test]
fn measure_near_high_skip_rate_on_fixture_universe() {
    // Production defaults: near_high_frac=2%, stretch=4%, min_change=0.5%, max_change=12%.
    let p = ContinuationParams {
        liquid_n: 20,
        ..ContinuationParams::default()
    };
    // 30 liquid-ish alts: mix of near-high, stretched, weak, max_change, and eligible.
    let mut tickers = Vec::new();
    for i in 0..30 {
        let sym = format!("ALT{i:02}USDT");
        let vol = format!("{}", 50_000_000 - i * 100_000);
        match i % 5 {
            0 => {
                // near 24h high (within 2%), healthy mid change
                tickers.push(mk(&sym, "10.0", "3.0", &vol, "10.05"));
            }
            1 => {
                // stretch_pct (>=4%)
                tickers.push(mk(&sym, "10.0", "5.0", &vol, "12.0"));
            }
            2 => {
                // weak (< min_change 0.5%)
                tickers.push(mk(&sym, "10.0", "0.2", &vol, "11.0"));
            }
            3 => {
                // eligible: >2% off high, mid change under max 12
                tickers.push(mk(&sym, "9.5", "3.0", &vol, "10.0"));
            }
            _ => {
                // max_change (>12)
                tickers.push(mk(&sym, "9.7", "15.0", &vol, "12.0"));
            }
        }
    }
    // majors should be excluded by liquid_universe
    tickers.push(mk("BTCUSDT", "60000", "2.0", "900000000", "61000"));
    tickers.push(mk("ETHUSDT", "3000", "2.0", "400000000", "3100"));

    let uni = liquid_universe(&tickers, &[], &p);
    let n_uni = uni.len();
    assert!(n_uni > 0, "fixture universe empty");

    let mut tape_skip = 0u64;
    let mut near_high_skip = 0u64;
    let mut pass = 0u64;
    // Mirror pick_strategy4_book filter (measurement only; thresholds untouched).
    for t in &uni {
        let c = t.price_change_percent;
        let tape = c >= p.stretch_pct
            || c <= -p.stretch_pct
            || c < Decimal::ZERO
            || c < p.min_change_percent
            || p.max_change_percent.map(|m| c > m).unwrap_or(false);
        if tape {
            tape_skip += 1;
            continue;
        }
        if near_24h_high(t, p.near_high_frac) {
            near_high_skip += 1;
            continue;
        }
        pass += 1;
    }
    let book = pick_strategy4_book(&tickers, p.liquid_n, &[], Some(&p));
    let denom_after_tape = n_uni as u64 - tape_skip;
    let near_rate = if denom_after_tape == 0 {
        0.0
    } else {
        near_high_skip as f64 / denom_after_tape as f64
    };
    let near_of_uni = near_high_skip as f64 / n_uni as f64;

    eprintln!(
        "S4 skip-rate fixture: liquid_universe={n_uni} tape_skip={tape_skip} \
         near_high_skip={near_high_skip} pass={pass} book_len={} \
         near_high/(uni-tape)={:.1}% near_high/uni={:.1}%",
        book.len(),
        near_rate * 100.0,
        near_of_uni * 100.0
    );
    // Sanity: fixture construction guarantees some near_high skips among tape-passers.
    assert!(near_high_skip > 0, "fixture should include near_high skips");
    assert_eq!(book.len() as u64, pass.min(p.liquid_n as u64));

    flush_s4_skip_stats();
    let top = s4_skip_stats_top(8);
    eprintln!("S4 skip tally top: {top:?}");
}

#[test]
fn measure_near_high_skip_rate_from_public_tape_if_reachable() {
    // Best-effort live tape; skip quietly if network blocked (offline CI).
    let body = match std::process::Command::new("curl")
        .args([
            "-fsS",
            "--max-time",
            "8",
            "https://fapi.binance.com/fapi/v1/ticker/24hr",
        ])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            eprintln!("public tape unreachable — skip live near_high measure");
            return;
        }
    };
    let raw: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tape json parse fail: {e}");
            return;
        }
    };
    let tickers = tui_bot::ranking::parse_tickers(&raw);
    if tickers.len() < 50 {
        eprintln!("tape too small ({}) — skip", tickers.len());
        return;
    }
    let p = ContinuationParams::default();
    let uni = liquid_universe(&tickers, &[], &p);
    let n_uni = uni.len();
    let mut tape_skip = 0u64;
    let mut near_high_skip = 0u64;
    let mut pass = 0u64;
    let mut reasons: HashMap<&'static str, u64> = HashMap::new();
    for t in &uni {
        let c = t.price_change_percent;
        if c >= p.stretch_pct || c <= -p.stretch_pct {
            tape_skip += 1;
            *reasons.entry("stretch").or_default() += 1;
            continue;
        }
        if c < Decimal::ZERO || c < p.min_change_percent {
            tape_skip += 1;
            *reasons.entry("weak_24h").or_default() += 1;
            continue;
        }
        if let Some(max_c) = p.max_change_percent {
            if c > max_c {
                tape_skip += 1;
                *reasons.entry("max_change").or_default() += 1;
                continue;
            }
        }
        if near_24h_high(t, p.near_high_frac) {
            near_high_skip += 1;
            *reasons.entry("near_high").or_default() += 1;
            continue;
        }
        pass += 1;
    }
    let book = pick_strategy4_book(&tickers, p.liquid_n, &[], Some(&p));
    let denom = n_uni as u64 - tape_skip;
    eprintln!(
        "S4 skip-rate LIVE tape: tickers={} liquid_universe={n_uni} \
         tape_skip={tape_skip} near_high_skip={near_high_skip} pass={pass} book_len={} \
         near_high/(uni-tape)={:.1}% near_high/uni={:.1}% reasons={reasons:?}",
        tickers.len(),
        book.len(),
        if denom == 0 { 0.0 } else { near_high_skip as f64 / denom as f64 * 100.0 },
        if n_uni == 0 { 0.0 } else { near_high_skip as f64 / n_uni as f64 * 100.0 },
    );
}

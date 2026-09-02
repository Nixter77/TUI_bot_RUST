#![allow(dead_code)]

use rust_decimal::Decimal;
use tui_bot::models::{Account, Bar, Ticker};
use tui_bot::scalp::ScalpParams;
use tui_bot::sessions::make_utc_ts;
use tui_bot::trend::TrendParams;

pub fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

pub fn tickers() -> Vec<Ticker> {
    vec![
        Ticker::new("ETHUSDT", d("3000"), d("2.0"), d("100000")),
        Ticker::new("BTCUSDT", d("50000"), d("9.5"), d("800000")),
        Ticker::new("SOLUSDT", d("140"), d("4.0"), d("200000")),
        Ticker::new("XRPUSDT", d("0.5"), d("1.0"), d("40000")),
    ]
}

pub fn account() -> Account {
    Account {
        wallet_balance: d("10000"),
        unrealized_pnl: Decimal::ZERO,
        available_balance: d("10000"),
        starting_equity: d("10000"),
    }
}

pub fn scalp_loose() -> ScalpParams {
    ScalpParams {
        entry_windows: Vec::new(),
        rsi_min: Decimal::ZERO,
        rsi_max: Decimal::from(100),
        min_atr_pct: Decimal::ZERO,
        max_atr_pct: Decimal::ONE,
        min_volume_frac: Decimal::ZERO,
        extend_atr: Decimal::from(3),
        min_stop_pct: Decimal::ZERO,
        cooldown_sec: 0.0,
        ..ScalpParams::default()
    }
}

pub fn trend_loose() -> TrendParams {
    TrendParams {
        adx_min: Decimal::ZERO,
        ema_filter: 0,
        min_stop_pct: Decimal::ZERO,
        entry_windows: Vec::new(),
        cooldown_sec: 0.0,
        ..TrendParams::default()
    }
}

pub fn bar_oc(i: i64, open: f64, close: f64, start: i64, interval: i64) -> Bar {
    let o = Decimal::from_str_exact(&format!("{open:.4}")).unwrap();
    let c = Decimal::from_str_exact(&format!("{close:.4}")).unwrap();
    let high = o.max(c) + d("0.04");
    let low = o.min(c) - d("0.04");
    Bar {
        open_time: start + i * interval,
        open: o,
        high,
        low,
        close: c,
        volume: d("20"),
    }
}

pub fn grind_then_pullback(start_ms: i64) -> Vec<Bar> {
    let mut bars = Vec::new();
    let mut price = 100.0;
    let mut i = 0i64;
    for _ in 0..50 {
        let nxt = price + 0.08;
        bars.push(bar_oc(i, price, nxt, start_ms, 60_000));
        price = nxt;
        i += 1;
    }
    let peak = price;
    for _ in 0..4 {
        let nxt = price - 0.35;
        bars.push(bar_oc(i, price, nxt, start_ms, 60_000));
        price = nxt;
        i += 1;
    }
    let bounce = peak - 0.08 * 0.3;
    bars.push(bar_oc(i, price, bounce, start_ms, 60_000));
    bars
}

pub fn stair() -> Vec<Bar> {
    let mut bars = Vec::new();
    let mut price = 100.0;
    for i in 0..60 {
        let nxt = price + 0.15;
        bars.push(bar_oc(i, price, nxt, 0, 60_000));
        price = nxt;
    }
    bars
}

pub fn scalp_down() -> Vec<Bar> {
    let mut bars = Vec::new();
    let mut price = 120.0;
    for i in 0..60 {
        let nxt = price - 0.2;
        bars.push(bar_oc(i, price, nxt, 0, 60_000));
        price = nxt;
    }
    bars
}

pub fn trend_bar(i: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        open_time: 1_700_000_000_000 + i * 3_600_000,
        open: Decimal::from_str_exact(&format!("{open:.4}")).unwrap(),
        high: Decimal::from_str_exact(&format!("{high:.4}")).unwrap(),
        low: Decimal::from_str_exact(&format!("{low:.4}")).unwrap(),
        close: Decimal::from_str_exact(&format!("{close:.4}")).unwrap(),
        volume: d("20"),
    }
}

pub fn range_then_breakout() -> Vec<Bar> {
    let mut bars = Vec::new();
    for i in 0..60 {
        if i % 2 == 0 {
            bars.push(trend_bar(i, 100.0, 100.5, 99.6, 100.2));
        } else {
            bars.push(trend_bar(i, 100.2, 100.4, 99.5, 99.8));
        }
    }
    bars.push(trend_bar(60, 100.3, 102.4, 100.2, 102.1));
    bars
}

pub fn range_only() -> Vec<Bar> {
    let mut b = range_then_breakout();
    b.pop();
    b
}

pub fn grind_down() -> Vec<Bar> {
    let mut bars = Vec::new();
    let mut price = 120.0;
    for i in 0..70 {
        let nxt = price - 0.4;
        bars.push(trend_bar(i, price, price + 0.1, nxt - 0.1, nxt));
        price = nxt;
    }
    bars
}

pub fn majors() -> Vec<Ticker> {
    vec![
        Ticker::new("BTCUSDT", d("100"), d("3.0"), d("9000")),
        Ticker::new("ETHUSDT", d("100"), d("1.0"), d("4000")),
        Ticker::new("SOLUSDT", d("100"), d("0.5"), d("2000")),
    ]
}

fn bars_from_fracs(mark: f64, rows: &[(f64, f64, f64, f64)]) -> Vec<Bar> {
    let t0 = 1_700_000_000_000i64;
    let dt = 300_000i64;
    rows.iter()
        .enumerate()
        .map(|(i, (o, h, l, c))| {
            let fmt = |x: f64| Decimal::from_str_exact(&format!("{:.8}", mark * x)).unwrap();
            Bar {
                open_time: t0 + i as i64 * dt,
                open: fmt(*o),
                high: fmt(*h),
                low: fmt(*l),
                close: fmt(*c),
                volume: d("20"),
            }
        })
        .collect()
}

/// Grind up, two higher swing lows, then a 5m pullback-resume. Last bar is the green signal.
/// Long enough for EMA20.
pub fn pullback_5m_at(mark: f64) -> Vec<Bar> {
    let mut rows: Vec<(f64, f64, f64, f64)> = (0..10)
        .map(|i| {
            let o = 0.900 + i as f64 * 0.006;
            (o, o + 0.008, o - 0.001, o + 0.005)
        })
        .collect();
    rows.extend_from_slice(&[
        (0.960, 0.962, 0.954, 0.955),
        (0.955, 0.956, 0.946, 0.948),
        (0.948, 0.950, 0.938, 0.941),
        (0.941, 0.952, 0.940, 0.950),
        (0.950, 0.962, 0.948, 0.960),
    ]);
    rows.extend((0..6).map(|i| {
        let o = 0.960 + i as f64 * 0.006;
        (o, o + 0.008, o - 0.001, o + 0.005)
    }));
    rows.extend_from_slice(&[
        (0.990, 0.998, 0.988, 0.996),
        (0.996, 1.002, 0.986, 0.990),
        (0.990, 0.994, 0.978, 0.982),
        (0.982, 1.005, 0.980, 0.997),
        (0.997, 1.010, 0.996, 1.006),
    ]);
    bars_from_fracs(mark, &rows)
}

/// Multi-hour grind down, then a green 5m bounce — must not be bought as continuation.
pub fn downtrend_then_bounce_5m_at(mark: f64) -> Vec<Bar> {
    let mut rows: Vec<(f64, f64, f64, f64)> = (0..22)
        .map(|i| {
            let o = 1.080 - i as f64 * 0.004;
            (o, o + 0.003, o - 0.005, o - 0.003)
        })
        .collect();
    rows.push((0.990, 1.010, 0.988, 1.006));
    bars_from_fracs(mark, &rows)
}

pub fn pullback_last_at(mark: f64) -> Bar {
    pullback_5m_at(mark).pop().expect("pullback bars")
}

/// ~25 closed 4h bars drifting up; last close is above EMA20.
pub fn htf_up_4h_at(mark: f64) -> Vec<Bar> {
    htf_drift_4h_at(mark, true)
}

/// ~25 closed 4h bars drifting down; last close is below EMA20.
pub fn htf_down_4h_at(mark: f64) -> Vec<Bar> {
    htf_drift_4h_at(mark, false)
}

fn htf_drift_4h_at(mark: f64, up: bool) -> Vec<Bar> {
    let t0 = 1_700_000_000_000i64;
    let dt = 4 * 3_600_000i64;
    (0..25)
        .map(|i| {
            let frac = if up {
                0.88 + i as f64 * 0.005
            } else {
                1.12 - i as f64 * 0.005
            };
            let close = mark * frac;
            let open = if up { close * 0.998 } else { close * 1.002 };
            let high = close.max(open) * 1.001;
            let low = close.min(open) * 0.999;
            let fmt = |x: f64| Decimal::from_str_exact(&format!("{x:.8}")).unwrap();
            Bar {
                open_time: t0 + i as i64 * dt,
                open: fmt(open),
                high: fmt(high),
                low: fmt(low),
                close: fmt(close),
                volume: d("20"),
            }
        })
        .collect()
}

pub fn london_ts() -> f64 {
    make_utc_ts(2026, 8, 17, 7, 1, 0)
}

pub fn dead_ts() -> f64 {
    make_utc_ts(2026, 8, 17, 4, 12, 0)
}

pub fn london_ms() -> i64 {
    make_utc_ts(2026, 8, 17, 7, 5, 0) as i64 * 1000
}

pub fn night_ms() -> i64 {
    make_utc_ts(2026, 8, 17, 22, 5, 0) as i64 * 1000
}

//! BTC regime layer for alt-long risk gating (Phase 2).
//!
//! # Rules (existing snapshot data only — no indicator soup / no new strategies)
//!
//! Inputs from [`MarketSnapshot`]:
//! - BTCUSDT closed **4h** bars via `htf_bars` — EMA20, slope, ATR(14) rank
//! - optional **1h return** from `bars_for("BTCUSDT")` (signal / 1h series) when present
//!
//! Classification (evaluated in order):
//! 1. **Panic** — close < EMA20_4h AND (1h ret ≤ −2% OR (ATR% ≥ p85 of recent ATR
//!    samples AND last 4h bar down ≥ 1.5%))
//! 2. **Neutral (near EMA)** — `|close − EMA20| / EMA20 ≤ 0.5%` (chop; not Bear)
//! 3. **Bear** — close < EMA20_4h AND EMA20 slope ≤ 0  
//!    (`slope = EMA_now / EMA_{now-3} − 1`, ≈12h lookback on 4h)
//! 4. **StrongBull** — close > EMA20_4h × 1.005 AND slope > +0.15% AND 1h ret ≥ 0
//!    (if 1h ret missing, StrongBull still allowed when price/slope clear)
//! 5. **Bull** — close ≥ EMA20_4h AND slope ≥ 0
//! 6. **Neutral** — mixed / everything else  
//!    **also when BTC 4h / EMA is missing** — fail-open so Ready⟺enter is not
//!    halted by a brief BTC history gap (desk still sizes Neutral = 0.5× on S4)
//!
//! # Gate (new alt longs; S4 primary, S1 when wired)
//! | Regime     | Entries        | S4 `RISK_PCT` |
//! |------------|----------------|---------------|
//! | StrongBull | allow          | 1.0×          |
//! | Bull       | allow          | 1.0×          |
//! | Neutral    | allow          | 0.5×          |
//! | Bear       | block          | —             |
//! | Panic      | block only     | —             |
//!
//! Panic / Bear do **not** force-flatten open positions — block new entries only
//! (avoid surprise liquidations). Protective manage / fail-closed stops unchanged.

use crate::indicators::{ema_series, last_atr, last_ema};
use crate::models::{Bar, MarketSnapshot};
use rust_decimal::Decimal;

const BTC: &str = "BTCUSDT";
const EMA_PERIOD: usize = 20;
const SLOPE_BARS: usize = 3;
const ATR_PERIOD: usize = 14;
/// StrongBull: price must clear EMA by this fraction.
const STRONG_CLEAR: &str = "0.005";
/// StrongBull min EMA slope (fraction).
const STRONG_SLOPE: &str = "0.0015";
/// Panic 1h return threshold (percent points, same units as ret_over_secs ×100).
const PANIC_1H_PCT: &str = "-2";
/// Panic same-bar drop vs prior close (fraction).
const PANIC_BAR_DROP: &str = "0.015";
/// |price−EMA|/EMA at-or-below this → Neutral (chop), not Bear.
const NEAR_EMA: &str = "0.005";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BtcRegime {
    StrongBull,
    Bull,
    Neutral,
    Bear,
    Panic,
}

impl BtcRegime {
    pub fn as_str(self) -> &'static str {
        match self {
            BtcRegime::StrongBull => "STRONG_BULL",
            BtcRegime::Bull => "BULL",
            BtcRegime::Neutral => "NEUTRAL",
            BtcRegime::Bear => "BEAR",
            BtcRegime::Panic => "PANIC",
        }
    }

    /// Journal / snapshot wire form (stable lowercase snake for grep).
    pub fn journal_tag(self) -> &'static str {
        match self {
            BtcRegime::StrongBull => "strong_bull",
            BtcRegime::Bull => "bull",
            BtcRegime::Neutral => "neutral",
            BtcRegime::Bear => "bear",
            BtcRegime::Panic => "panic",
        }
    }

    pub fn blocks_alt_entry(self) -> bool {
        matches!(self, BtcRegime::Bear | BtcRegime::Panic)
    }

    /// Multiplier applied to configured `RISK_PCT` for S4 sizing.
    pub fn risk_multiplier(self) -> Decimal {
        match self {
            BtcRegime::StrongBull | BtcRegime::Bull => Decimal::ONE,
            BtcRegime::Neutral => Decimal::new(5, 1), // 0.5
            BtcRegime::Bear | BtcRegime::Panic => Decimal::ZERO,
        }
    }

    pub fn block_reason(self) -> Option<&'static str> {
        match self {
            BtcRegime::Bear => Some("BTC regime bear — не вхожу"),
            BtcRegime::Panic => Some("BTC regime panic — не вхожу"),
            _ => None,
        }
    }
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap_or(Decimal::ZERO)
}

fn ema_slope(closes: &[Decimal]) -> Option<Decimal> {
    if closes.len() < EMA_PERIOD + SLOPE_BARS {
        return None;
    }
    let series = ema_series(closes, EMA_PERIOD);
    let now = series.last().copied().flatten()?;
    let prev_i = series.len().checked_sub(1 + SLOPE_BARS)?;
    let prev = series.get(prev_i).copied().flatten()?;
    if prev <= Decimal::ZERO {
        return None;
    }
    Some((now - prev) / prev)
}

fn atr_percentile_rank(bars: &[Bar]) -> Option<Decimal> {
    if bars.len() < ATR_PERIOD + 5 {
        return None;
    }
    let last = last_atr(bars, ATR_PERIOD)?;
    // Collect trailing ATR samples (skip early None).
    let series = crate::indicators::atr_series(bars, ATR_PERIOD);
    let mut vals: Vec<Decimal> = series.into_iter().flatten().collect();
    if vals.len() < 5 {
        return None;
    }
    // Cap lookback for cheap percentile.
    if vals.len() > 50 {
        vals = vals.split_off(vals.len() - 50);
    }
    let n = vals.len() as u64;
    let below = vals.iter().filter(|v| **v <= last).count() as u64;
    Some(Decimal::from(below) * Decimal::from(100) / Decimal::from(n))
}

fn ret_over_secs(bars: &[Bar], last_price: Decimal, secs: i64) -> Option<Decimal> {
    if bars.is_empty() || last_price <= Decimal::ZERO {
        return None;
    }
    let tip = bars.last()?;
    let target = tip.open_time.saturating_sub(secs.saturating_mul(1000));
    let past = bars.iter().rev().find(|b| b.open_time <= target)?;
    if past.close <= Decimal::ZERO {
        return None;
    }
    Some((last_price - past.close) / past.close * Decimal::from(100))
}

fn btc_price(snapshot: &MarketSnapshot, htf: &[Bar]) -> Option<Decimal> {
    snapshot
        .tickers
        .iter()
        .find(|t| t.symbol.eq_ignore_ascii_case(BTC))
        .map(|t| t.last_price)
        .filter(|p| *p > Decimal::ZERO)
        .or_else(|| htf.last().map(|b| b.close).filter(|c| *c > Decimal::ZERO))
}

fn btc_1h_ret_pct(snapshot: &MarketSnapshot, px: Decimal) -> Option<Decimal> {
    let bars = snapshot.bars_for(BTC);
    if bars.len() >= 2 {
        if let Some(r) = ret_over_secs(bars, px, 3600) {
            return Some(r);
        }
    }
    None
}

/// Classify BTCUSDT from snapshot bars/tickers. Missing HTF → [`BtcRegime::Neutral`].
pub fn classify_snapshot(snapshot: &MarketSnapshot) -> BtcRegime {
    let htf = snapshot.htf_bars_for(BTC);
    let px = btc_price(snapshot, htf);
    let ret = px.and_then(|p| btc_1h_ret_pct(snapshot, p));
    classify_btc(htf, px, ret)
}

/// Pure classifier (unit-tested). `ret_1h_pct` is percent points (e.g. `-2.5` = −2.5%).
pub fn classify_btc(
    htf_4h: &[Bar],
    price: Option<Decimal>,
    ret_1h_pct: Option<Decimal>,
) -> BtcRegime {
    let Some(px) = price.filter(|p| *p > Decimal::ZERO) else {
        return BtcRegime::Neutral;
    };
    if htf_4h.len() < EMA_PERIOD + SLOPE_BARS {
        return BtcRegime::Neutral;
    }
    let closes: Vec<Decimal> = htf_4h.iter().map(|b| b.close).collect();
    let Some(ema) = last_ema(&closes, EMA_PERIOD) else {
        return BtcRegime::Neutral;
    };
    if ema <= Decimal::ZERO {
        return BtcRegime::Neutral;
    }
    let slope = ema_slope(&closes).unwrap_or(Decimal::ZERO);
    let below = px < ema;
    let bar_drop = htf_4h
        .len()
        .checked_sub(2)
        .and_then(|i| {
            let prev = htf_4h[i].close;
            let last = htf_4h[i + 1].close;
            if prev > Decimal::ZERO {
                Some((prev - last) / prev)
            } else {
                None
            }
        })
        .unwrap_or(Decimal::ZERO);
    let atr_hot = atr_percentile_rank(htf_4h)
        .map(|p| p >= Decimal::from(85))
        .unwrap_or(false);
    let panic_1h = ret_1h_pct.map(|r| r <= d(PANIC_1H_PCT)).unwrap_or(false);
    let panic_vol = atr_hot && bar_drop >= d(PANIC_BAR_DROP);

    if below && (panic_1h || panic_vol) {
        return BtcRegime::Panic;
    }
    let dist = (px - ema).abs() / ema;
    if dist <= d(NEAR_EMA) {
        return BtcRegime::Neutral;
    }
    if below && slope <= Decimal::ZERO {
        return BtcRegime::Bear;
    }
    let clear = ema * (Decimal::ONE + d(STRONG_CLEAR));
    let ret_ok = ret_1h_pct.map(|r| r >= Decimal::ZERO).unwrap_or(true);
    if px > clear && slope > d(STRONG_SLOPE) && ret_ok {
        return BtcRegime::StrongBull;
    }
    if px >= ema && slope >= Decimal::ZERO {
        return BtcRegime::Bull;
    }
    BtcRegime::Neutral
}

/// `Some(reason)` when new alt longs must not open.
pub fn block_alt_entry(snapshot: &MarketSnapshot) -> Option<String> {
    classify_snapshot(snapshot)
        .block_reason()
        .map(str::to_string)
}

/// Effective S4 risk fraction: `cfg_risk * multiplier` (0 when blocked).
pub fn effective_risk_pct(cfg_risk: Decimal, snapshot: &MarketSnapshot) -> Decimal {
    let reg = classify_snapshot(snapshot);
    if cfg_risk <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    cfg_risk * reg.risk_multiplier()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Bar;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn bar_at(i: i64, close: f64) -> Bar {
        let c = Decimal::from_str_exact(&format!("{close:.2}")).unwrap();
        let o = c;
        Bar {
            open_time: 1_700_000_000_000 + i * 4 * 3_600_000,
            open: o,
            high: c + d("50"),
            low: c - d("50"),
            close: c,
            volume: d("100"),
        }
    }

    /// Rising closes → EMA up, price above.
    fn rising_htf(n: usize, start: f64, step: f64) -> Vec<Bar> {
        (0..n)
            .map(|i| bar_at(i as i64, start + step * i as f64))
            .collect()
    }

    /// Falling closes → EMA down, price below.
    fn falling_htf(n: usize, start: f64, step: f64) -> Vec<Bar> {
        (0..n)
            .map(|i| bar_at(i as i64, start - step * i as f64))
            .collect()
    }

    #[test]
    fn bull_and_strong_bull() {
        let htf = rising_htf(40, 40_000.0, 80.0);
        let px = htf.last().unwrap().close;
        let reg = classify_btc(&htf, Some(px), Some(d("0.5")));
        assert!(
            matches!(reg, BtcRegime::Bull | BtcRegime::StrongBull),
            "{reg:?}"
        );
        assert!(!reg.blocks_alt_entry());
        assert_eq!(reg.risk_multiplier(), Decimal::ONE);
    }

    #[test]
    fn bear_blocks() {
        let htf = falling_htf(40, 50_000.0, 120.0);
        let px = htf.last().unwrap().close;
        let reg = classify_btc(&htf, Some(px), Some(d("-0.5")));
        assert_eq!(reg, BtcRegime::Bear, "{reg:?}");
        assert!(reg.blocks_alt_entry());
        assert_eq!(reg.block_reason(), Some("BTC regime bear — не вхожу"));
        assert_eq!(reg.risk_multiplier(), Decimal::ZERO);
    }

    #[test]
    fn panic_on_sharp_1h_drop() {
        let htf = falling_htf(40, 50_000.0, 80.0);
        let px = htf.last().unwrap().close;
        let reg = classify_btc(&htf, Some(px), Some(d("-3.2")));
        assert_eq!(reg, BtcRegime::Panic);
        assert_eq!(reg.block_reason(), Some("BTC regime panic — не вхожу"));
    }

    #[test]
    fn missing_htf_is_neutral_half_risk() {
        let reg = classify_btc(&[], Some(d("50000")), None);
        assert_eq!(reg, BtcRegime::Neutral);
        assert!(!reg.blocks_alt_entry());
        assert_eq!(reg.risk_multiplier(), d("0.5"));
    }

    #[test]
    fn gate_helpers() {
        assert_eq!(BtcRegime::Bull.journal_tag(), "bull");
        assert_eq!(BtcRegime::StrongBull.as_str(), "STRONG_BULL");
        let mut snap = MarketSnapshot::empty(d("10000"));
        assert_eq!(block_alt_entry(&snap), None); // neutral
        snap.htf_bars
            .insert(BTC.into(), falling_htf(40, 50_000.0, 120.0));
        let px = snap.htf_bars_for(BTC).last().unwrap().close;
        snap.tickers
            .push(crate::models::Ticker::new(BTC, px, d("0"), d("1")));
        assert!(block_alt_entry(&snap).unwrap().contains("bear"));
    }

    #[test]
    fn strong_bull_full_risk() {
        let htf = rising_htf(50, 40_000.0, 200.0);
        let ema_proxy = htf.last().unwrap().close;
        let px = ema_proxy * (Decimal::ONE + d("0.01"));
        let reg = classify_btc(&htf, Some(px), Some(d("0.3")));
        assert_eq!(reg, BtcRegime::StrongBull, "{reg:?}");
        assert_eq!(reg.risk_multiplier(), Decimal::ONE);
        assert!(!reg.blocks_alt_entry());
    }

    #[test]
    fn neutral_half_risk_recovering() {
        // Rising HTF (slope > 0) but mark below EMA20 → Neutral (not Bear, not Bull).
        let htf = rising_htf(40, 40_000.0, 100.0);
        let closes: Vec<Decimal> = htf.iter().map(|b| b.close).collect();
        let ema = last_ema(&closes, EMA_PERIOD).expect("ema");
        let px = ema * d("0.99");
        assert!(px < ema);
        let reg = classify_btc(&htf, Some(px), Some(d("0.2")));
        assert_eq!(reg, BtcRegime::Neutral, "{reg:?}");
        assert_eq!(reg.risk_multiplier(), d("0.5"));
        assert!(!reg.blocks_alt_entry());
        assert!(reg.block_reason().is_none());
    }

    #[test]
    fn risk_multiplier_table() {
        assert_eq!(BtcRegime::StrongBull.risk_multiplier(), Decimal::ONE);
        assert_eq!(BtcRegime::Bull.risk_multiplier(), Decimal::ONE);
        assert_eq!(BtcRegime::Neutral.risk_multiplier(), d("0.5"));
        assert_eq!(BtcRegime::Bear.risk_multiplier(), Decimal::ZERO);
        assert_eq!(BtcRegime::Panic.risk_multiplier(), Decimal::ZERO);
        assert!(BtcRegime::Bear.blocks_alt_entry());
        assert!(BtcRegime::Panic.blocks_alt_entry());
        assert!(!BtcRegime::Bull.blocks_alt_entry());
    }
}

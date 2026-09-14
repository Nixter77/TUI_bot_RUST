//! S4 SetupScore — hard/soft split over **existing** live gates only.
//!
//! Soft features (no new indicator soup, no HL):
//! - 4h EMA distance (close vs EMA20 on HTF)
//! - pullback depth vs ATR on signal TF
//! - 24h dump/tape quality (already hard-rejects dumps)
//! - session window membership
//!
//! Soft-reject when projected `costR ≥ X% of 1R`. X = 4 from 15m fee/stop
//! reality: RT taker ~0.08% / min stop 2% → costR ≈ 0.04R.
//!
//! BTC regime: Bear/Panic stay hard (via `regime::block_alt_entry`); Neutral
//! soft-penalizes the score (desk already sizes 0.5×).

use crate::continuation::ContinuationParams;
use crate::indicators::{last_atr, last_ema};
use crate::models::{Bar, MarketSnapshot, Ticker};
use crate::money::round_trip_taker_pct;
use crate::regime::{classify_snapshot, BtcRegime};
use crate::sessions::in_entry_window;
use rust_decimal::Decimal;

/// Enter when score ≥ this (IMPROVEMENT_PLAN phase 3).
pub const SCORE_ENTER: u8 = 75;
/// Watch band lower bound — entry still soft-skipped in piece 1.
pub const SCORE_WATCH: u8 = 65;

/// Soft-reject threshold as fraction of 1R.
/// 15m: RT 0.08% / min stop 2% → costR ≈ 0.04R → **X = 4**.
pub fn cost_r_soft_max() -> Decimal {
    Decimal::new(4, 2)
}

pub const COST_R_SOFT_MAX_PCT: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupAction {
    /// score ≥ SCORE_ENTER, no soft reject
    Enter,
    /// SCORE_WATCH..SCORE_ENTER-1 (monitor ACTIVE later; enter soft-skip now)
    Watch,
    /// hard or soft reject / score < SCORE_WATCH
    Skip,
}

#[derive(Debug, Clone)]
pub struct SetupScore {
    pub score: u8,
    pub action: SetupAction,
    pub hard_reject: Option<String>,
    pub soft_reject: Option<String>,
    /// Projected RT fee / natural stop risk (units of R). None if stop N/A.
    pub cost_r: Option<Decimal>,
    pub ema4h_pts: u8,
    pub pullback_pts: u8,
    pub tape_pts: u8,
    pub session_pts: u8,
    pub regime_pts: u8,
}

impl SetupScore {
    pub fn skip_reason(&self) -> Option<String> {
        if let Some(r) = &self.hard_reject {
            return Some(r.clone());
        }
        if let Some(r) = &self.soft_reject {
            return Some(r.clone());
        }
        match self.action {
            SetupAction::Enter => None,
            SetupAction::Watch => Some(format!(
                "setup score {} — watch (need ≥{})",
                self.score, SCORE_ENTER
            )),
            SetupAction::Skip => Some(format!(
                "setup score {} — soft skip (need ≥{})",
                self.score, SCORE_WATCH
            )),
        }
    }
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap_or(Decimal::ZERO)
}

fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 100) as u8
}

/// Bars strictly before `last` (same as continuation hist_bars).
fn hist_before<'a>(bars: &'a [Bar], last: &Bar) -> &'a [Bar] {
    let end = bars
        .iter()
        .position(|b| b.open_time >= last.open_time)
        .unwrap_or(bars.len());
    &bars[..end]
}

/// Natural (pre-floor) stop risk as fraction of mark. None if invalid / too wide.
pub fn natural_stop_risk_pct(
    snapshot: &MarketSnapshot,
    symbol: &str,
    last: &Bar,
    mark: Decimal,
    p: &ContinuationParams,
) -> Option<Decimal> {
    if mark <= Decimal::ZERO {
        return None;
    }
    let bars = snapshot.bars_for(symbol);
    let hist = hist_before(bars, last);
    let mut sl = last.low;
    for bar in hist.iter().rev().take(p.stop_lookback) {
        if bar.low > Decimal::ZERO {
            sl = sl.min(bar.low);
        }
    }
    if p.atr_period > 0 {
        if let Some(atr) = last_atr(bars, p.atr_period) {
            if atr > Decimal::ZERO {
                let atr_sl = mark - p.atr_k * atr;
                if atr_sl > Decimal::ZERO {
                    sl = sl.min(atr_sl);
                }
            }
        }
    }
    if sl <= Decimal::ZERO || sl >= mark {
        return None;
    }
    let risk = (mark - sl) / mark;
    if risk > p.max_stop_pct {
        return None;
    }
    Some(risk)
}

/// costR = RT_taker_pct / stop_risk_pct (dimensionless R units).
pub fn projected_cost_r(stop_risk_pct: Decimal) -> Option<Decimal> {
    if stop_risk_pct <= Decimal::ZERO {
        return None;
    }
    Some(round_trip_taker_pct() / stop_risk_pct)
}

fn score_ema4h(snapshot: &MarketSnapshot, symbol: &str) -> u8 {
    // Hard gate already requires close > EMA20. Soft: prefer modest clear (0.3–3%).
    let bars = snapshot.htf_bars_for(symbol);
    if bars.len() < 21 {
        return 0;
    }
    let closes: Vec<Decimal> = bars.iter().map(|b| b.close).collect();
    let Some(ema) = last_ema(&closes, 20) else {
        return 0;
    };
    let Some(last) = bars.last() else {
        return 0;
    };
    if ema <= Decimal::ZERO || last.close <= ema {
        return 0;
    }
    let dist = (last.close - ema) / ema;
    // 0–0.3%: 12 · 0.3–1.5%: 25 · 1.5–3%: 20 · 3–5%: 12 · >5%: 6 (chase)
    if dist < d("0.003") {
        12
    } else if dist < d("0.015") {
        25
    } else if dist < d("0.03") {
        20
    } else if dist < d("0.05") {
        12
    } else {
        6
    }
}

fn score_pullback_vs_atr(
    snapshot: &MarketSnapshot,
    symbol: &str,
    last: &Bar,
    p: &ContinuationParams,
) -> u8 {
    let bars = snapshot.bars_for(symbol);
    let hist = hist_before(bars, last);
    if hist.len() < 2 {
        return 0;
    }
    let recent: Vec<&Bar> = hist.iter().rev().take(5).collect();
    let swing_high = recent.iter().map(|b| b.high).max().unwrap_or(last.high);
    let pullback_low = recent.iter().map(|b| b.low).min().unwrap_or(last.low);
    if swing_high <= Decimal::ZERO {
        return 0;
    }
    let depth = (swing_high - pullback_low) / swing_high;
    let atr = last_atr(bars, if p.atr_period > 0 { p.atr_period } else { 14 });
    let mark = last.close;
    let atr_frac = atr
        .filter(|a| *a > Decimal::ZERO && mark > Decimal::ZERO)
        .map(|a| a / mark);
    // Prefer pullback ≈ 0.5–2.0× ATR; shallow or crash-like scores low.
    let Some(af) = atr_frac else {
        // No ATR: fall back to min_pullback band quality.
        if depth < p.min_pullback_pct {
            return 5;
        }
        if depth < p.min_pullback_pct * d("2") {
            return 18;
        }
        return 12;
    };
    if af <= Decimal::ZERO {
        return 0;
    }
    let ratio = depth / af;
    if ratio < d("0.35") {
        6 // too shallow vs noise
    } else if ratio < d("0.75") {
        16
    } else if ratio <= d("2.0") {
        25 // sweet: pullback vs ATR
    } else if ratio <= d("3.5") {
        12
    } else {
        4 // dump-like wick
    }
}

fn score_tape(ticker: &Ticker, p: &ContinuationParams) -> u8 {
    // Dumps already hard-rejected. Soft: mid-band green day.
    let c = ticker.price_change_percent;
    if c <= Decimal::ZERO || c < p.min_change_percent {
        return 0;
    }
    if c <= -p.stretch_pct {
        return 0;
    }
    let stretch = p.stretch_pct.max(d("1"));
    // Prefer ~min..~2×stretch; penalize near max_change chase.
    if c < p.min_change_percent * d("2") {
        10
    } else if c <= stretch {
        20
    } else if c <= stretch * d("2") {
        15
    } else if p.max_change_percent.map(|m| c > m * d("0.7")).unwrap_or(false) {
        6
    } else {
        10
    }
}

fn score_session(now: f64, p: &ContinuationParams) -> u8 {
    if p.always_enter && p.entry_windows.is_empty() {
        return 15; // 24/7 soak — neutral credit
    }
    if in_entry_window(now, Some(&p.entry_windows), false) {
        20
    } else if p.always_enter {
        8 // outside preferred hours but soak allows
    } else {
        0 // hard session gate will usually block enter anyway
    }
}

fn score_regime(snapshot: &MarketSnapshot) -> u8 {
    match classify_snapshot(snapshot) {
        BtcRegime::StrongBull => 10,
        BtcRegime::Bull => 8,
        BtcRegime::Neutral => 4,
        BtcRegime::Bear | BtcRegime::Panic => 0,
    }
}

/// Evaluate SetupScore for a ticker that already passed hard universe gates
/// (or still include hard cost/HTF checks here for pure unit tests).
pub fn evaluate(
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    signal: &Bar,
    p: &ContinuationParams,
    now: f64,
) -> SetupScore {
    let ema4h_pts = score_ema4h(snapshot, &ticker.symbol);
    let pullback_pts = score_pullback_vs_atr(snapshot, &ticker.symbol, signal, p);
    let tape_pts = score_tape(ticker, p);
    let session_pts = score_session(now, p);
    let regime_pts = score_regime(snapshot);

    let raw = ema4h_pts as i32
        + pullback_pts as i32
        + tape_pts as i32
        + session_pts as i32
        + regime_pts as i32;
    // Max design points: 25+25+20+20+10 = 100
    let score = clamp_u8(raw);

    let mut soft_reject: Option<String> = None;
    let cost_r = natural_stop_risk_pct(snapshot, &ticker.symbol, signal, ticker.last_price, p)
        .and_then(projected_cost_r);

    if let Some(cr) = cost_r {
        // Strictly worse than 15m design (RT 0.08% / 2% = 0.04R). Equal-to-floor OK.
        if cr > cost_r_soft_max() {
            soft_reject = Some(format!(
                "costR {:.3}R > {:.0}% of 1R — soft reject",
                cr,
                cost_r_soft_max() * Decimal::from(100)
            ));
        }
    }

    let action = if soft_reject.is_some() {
        SetupAction::Skip
    } else if score >= SCORE_ENTER {
        SetupAction::Enter
    } else if score >= SCORE_WATCH {
        SetupAction::Watch
    } else {
        SetupAction::Skip
    };

    SetupScore {
        score,
        action,
        hard_reject: None,
        soft_reject,
        cost_r,
        ema4h_pts,
        pullback_pts,
        tape_pts,
        session_pts,
        regime_pts,
    }
}

/// Soft skip reason for S4 enter path, or None if score allows Enter.
pub fn soft_entry_skip(
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    signal: &Bar,
    p: &ContinuationParams,
    now: f64,
) -> Option<String> {
    if !p.setup_score {
        return None;
    }
    let s = evaluate(snapshot, ticker, signal, p, now);
    s.skip_reason()
}

/// 15m costR reference helper (tests / NOTES).
pub fn cost_r_at_stop_pct(stop_pct: Decimal) -> Option<Decimal> {
    projected_cost_r(stop_pct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TradeInterval;
    use crate::models::{Bar, MarketSnapshot, Ticker};

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn bar(i: i64, o: f64, h: f64, l: f64, c: f64, vol: f64) -> Bar {
        Bar {
            open_time: 1_700_000_000_000 + i * 15 * 60_000,
            open: Decimal::from_str_exact(&format!("{o}")).unwrap(),
            high: Decimal::from_str_exact(&format!("{h}")).unwrap(),
            low: Decimal::from_str_exact(&format!("{l}")).unwrap(),
            close: Decimal::from_str_exact(&format!("{c}")).unwrap(),
            volume: Decimal::from_str_exact(&format!("{vol}")).unwrap(),
        }
    }

    #[test]
    fn cost_r_matches_15m_fee_stop_reality() {
        // RT 0.08% / 2% = 0.04R; / 5% = 0.016R
        let c2 = cost_r_at_stop_pct(d("0.02")).unwrap();
        let c5 = cost_r_at_stop_pct(d("0.05")).unwrap();
        assert!((c2 - d("0.04")).abs() < d("0.0001"), "{c2}");
        assert!((c5 - d("0.016")).abs() < d("0.0001"), "{c5}");
        assert_eq!(cost_r_soft_max(), d("0.04"));
        assert_eq!(COST_R_SOFT_MAX_PCT, 4);
        assert!(c2 >= cost_r_soft_max()); // design point
        assert!(c5 < cost_r_soft_max());
    }

    #[test]
    fn soft_reject_when_natural_stop_tighter_than_fee_floor() {
        let p = ContinuationParams::default().with_interval(TradeInterval::Minute15);
        let mut snap = MarketSnapshot::empty(d("10000"));
        // Tight range bars → natural stop << 2%
        let mut bars = Vec::new();
        for i in 0..40 {
            let px = 100.0 + (i as f64) * 0.01;
            bars.push(bar(i, px, px + 0.05, px - 0.05, px + 0.02, 1000.0));
        }
        let last = bars.last().unwrap().clone();
        snap.bars.insert("ALTUSDT".into(), bars);
        // Rising 4h above EMA
        let mut htf = Vec::new();
        for i in 0..40 {
            let px = 90.0 + i as f64 * 0.5;
            htf.push(Bar {
                open_time: 1_700_000_000_000 + i * 4 * 3_600_000,
                open: d(&format!("{px}")),
                high: d(&format!("{}", px + 0.3)),
                low: d(&format!("{}", px - 0.3)),
                close: d(&format!("{}", px + 0.2)),
                volume: d("100"),
            });
        }
        snap.htf_bars.insert("ALTUSDT".into(), htf);
        let t = Ticker::new("ALTUSDT", last.close, d("2.0"), d("5_000_000"));
        let s = evaluate(&snap, &t, &last, &p, 7.0 * 3600.0); // London window
        assert!(s.cost_r.is_some(), "expected cost_r");
        let cr = s.cost_r.unwrap();
        assert!(
            cr > cost_r_soft_max(),
            "expected soft costR reject, got {cr} score={}",
            s.score
        );
        assert!(s.soft_reject.is_some());
        assert_eq!(s.action, SetupAction::Skip);
    }

    #[test]
    fn disabled_flag_skips_soft_gate() {
        let mut p = ContinuationParams::default().with_interval(TradeInterval::Minute15);
        p.setup_score = false;
        let snap = MarketSnapshot::empty(d("10000"));
        let last = bar(0, 10.0, 10.2, 9.9, 10.1, 100.0);
        let t = Ticker::new("ALTUSDT", d("10.1"), d("2"), d("1_000_000"));
        assert!(soft_entry_skip(&snap, &t, &last, &p, 0.0).is_none());
    }

    #[test]
    fn thresholds_match_plan() {
        assert_eq!(SCORE_ENTER, 75);
        assert_eq!(SCORE_WATCH, 65);
    }
}

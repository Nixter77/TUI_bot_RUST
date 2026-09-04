//! Strategy 4 (Continuation): long-only pullback on liquid names.
//!
//! Does **not** chase 24h % leaders. Signal bars come from `STRATEGY4_INTERVAL`
//! (5m / 15m / 30m / 1h). SL/TP width follows that interval; TP is 2R after fees.

use crate::config::TradeInterval;
use crate::indicators::{last_atr, last_ema, last_two_swing_lows, mean_volume, vwap};
use crate::money::round_trip_taker_pct;
use crate::models::{
    bar_is_red, last_closed_bar, near_24h_high, Bar, Decision, MarketSnapshot, Position, Side, Ticker,
};
use crate::ranking::{is_junk_symbol, is_major_symbol};
use crate::sessions::{
    in_entry_window, outside_entry_reason, session_status, HourWindow, DEFAULT_ENTRY_WINDOWS,
};
use crate::trail::{candidate_stop, long_stop_is_valid, trail_stop_upward};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

const NEAR_HIGH_SKIP: &str = "у 24h high — не догоняю";
/// Book kline fetch and new-entry scan share this cadence.
pub const SCAN_SEC: f64 = 60.0;

pub fn scan_due(last_scan_ts: f64, now: f64) -> bool {
    last_scan_ts <= 0.0 || (now - last_scan_ts) >= SCAN_SEC
}

fn s4_skip_tally() -> &'static Mutex<HashMap<String, u64>> {
    static TALLY: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    TALLY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Session tally of S4 entry skip reasons (read-only instrumentation).
pub fn note_s4_skip(reason: &str) {
    if reason.is_empty() {
        return;
    }
    if let Ok(mut g) = s4_skip_tally().lock() {
        *g.entry(reason.to_string()).or_insert(0) += 1;
    }
}

/// Top-N skip reasons for the current process session.
pub fn s4_skip_stats_top(n: usize) -> Vec<(String, u64)> {
    let Ok(g) = s4_skip_tally().lock() else {
        return Vec::new();
    };
    let mut rows: Vec<(String, u64)> = g.iter().map(|(k, v)| (k.clone(), *v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rows.truncate(n);
    rows
}



/// Strategy 4 knobs. `with_interval` sets SL/TP width for 5m / 15m / 30m / 1h.
#[derive(Debug, Clone, PartialEq)]
pub struct ContinuationParams {
    pub tp_pct: Decimal,
    pub trail_pct: Decimal,
    pub min_change_percent: Decimal,
    pub min_quote_volume: Decimal,
    pub min_price: Decimal,
    pub max_change_percent: Option<Decimal>,
    pub liquid_frac: Decimal,
    pub liquid_n: usize,
    pub week_leader_pct: Decimal,
    pub stretch_pct: Decimal,
    pub near_high_frac: Decimal,
    pub reward_r: Decimal,
    pub min_stop_pct: Decimal,
    pub max_stop_pct: Decimal,
    pub always_enter: bool,
    pub entry_windows: Vec<HourWindow>,
    pub cooldown_sec: f64,
    pub max_positions: i32,
    /// ATR period for adaptive stop calculation (0 = disabled).
    pub atr_period: usize,
    /// ATR multiplier: stop = mark - atr_k * ATR (wider wins vs structural low).
    pub atr_k: Decimal,
    /// Entry candle volume must be >= this fraction of the recent mean (0 = disabled).
    pub volume_confirm_frac: Decimal,
    /// Minimum pullback depth as fraction of swing high (0 = disabled).
    pub min_pullback_pct: Decimal,
    /// Number of historical bars to scan for the structural stop low.
    pub stop_lookback: usize,
    /// Signal kline interval (5m / 15m / 30m / 1h).
    pub interval: TradeInterval,
}

impl Default for ContinuationParams {
    fn default() -> Self {
        Self {
            tp_pct: Decimal::new(25, 3),
            trail_pct: Decimal::new(8, 3),
            min_change_percent: Decimal::new(5, 1),
            min_quote_volume: Decimal::from(50_000),
            min_price: Decimal::new(5, 1),
            max_change_percent: Some(Decimal::from(20)),
            liquid_frac: Decimal::new(2, 2),
            liquid_n: 20,
            week_leader_pct: Decimal::from(4),
            stretch_pct: Decimal::from(4),
            near_high_frac: Decimal::new(2, 2),
            reward_r: TradeInterval::Minute5.reward_r(),
            min_stop_pct: TradeInterval::Minute5.min_stop_pct(),
            max_stop_pct: TradeInterval::Minute5.max_stop_pct(),
            always_enter: false,
            entry_windows: DEFAULT_ENTRY_WINDOWS.to_vec(),
            cooldown_sec: 1800.0,
            max_positions: 5,
            atr_period: 14,
            atr_k: Decimal::from(2),
            volume_confirm_frac: Decimal::new(8, 1), // 0.8
            min_pullback_pct: TradeInterval::Minute5.min_pullback_pct(),
            stop_lookback: 3,
            interval: TradeInterval::Minute5,
        }
    }
}

impl ContinuationParams {
    pub fn with_interval(mut self, interval: TradeInterval) -> Self {
        self.interval = interval;
        self.min_stop_pct = interval.min_stop_pct();
        self.max_stop_pct = interval.max_stop_pct();
        self.min_pullback_pct = interval.min_pullback_pct();
        self.reward_r = interval.reward_r();
        self
    }
}

pub fn max_quote_volume(tickers: &[Ticker]) -> Decimal {
    tickers
        .iter()
        .map(|t| t.quote_volume)
        .max()
        .unwrap_or(Decimal::ZERO)
}

pub fn volume_floor(tickers: &[Ticker], p: &ContinuationParams) -> Decimal {
    let cap = max_quote_volume(tickers) * p.liquid_frac;
    if p.min_quote_volume > cap {
        p.min_quote_volume
    } else {
        cap
    }
}

pub fn liquid_universe<'a>(
    tickers: &'a [Ticker],
    exclude: &[String],
    p: &ContinuationParams,
) -> Vec<&'a Ticker> {
    let skip: HashSet<String> = exclude.iter().map(|s| s.to_ascii_uppercase()).collect();
    let floor = volume_floor(tickers, p);
    let mut rows: Vec<&Ticker> = tickers
        .iter()
        .filter(|t| {
            if skip.contains(&t.symbol.to_ascii_uppercase())
                || is_junk_symbol(&t.symbol)
                || is_major_symbol(&t.symbol)
            {
                return false;
            }
            t.last_price > Decimal::ZERO
                && t.last_price >= p.min_price
                && t.quote_volume >= floor
        })
        .collect();
    rows.sort_by(|a, b| {
        b.quote_volume
            .cmp(&a.quote_volume)
            .then(b.symbol.cmp(&a.symbol))
    });
    let n = p.liquid_n.max(1);
    rows.truncate(n);
    rows
}

pub fn liquid_keys(tickers: &[Ticker], exclude: &[String], p: &ContinuationParams) -> HashSet<String> {
    liquid_universe(tickers, exclude, p)
        .into_iter()
        .map(|t| t.symbol.to_ascii_uppercase())
        .collect()
}

fn has_tape(ticker: &Ticker, p: &ContinuationParams) -> bool {
    ticker.quote_volume > Decimal::ZERO && ticker.quote_volume >= p.min_quote_volume
}

pub fn week_change(ticker: &Ticker, bars: &[Bar]) -> Decimal {
    if ticker.week_change_percent != Decimal::ZERO {
        return ticker.week_change_percent;
    }
    if bars.len() < 2 {
        return Decimal::ZERO;
    }
    let last = &bars[bars.len() - 1];
    let span_ms = 7 * 24 * 3600 * 1000_i64;
    let target = last.open_time.saturating_sub(span_ms);
    let prev = bars
        .iter()
        .find(|b| b.open_time >= target)
        .unwrap_or(&bars[0]);
    if prev.close <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    (last.close - prev.close) / prev.close * Decimal::from(100)
}

fn signal_bar<'a>(snapshot: &'a MarketSnapshot, symbol: &str) -> Option<&'a Bar> {
    if let Some(bar) = snapshot.last_bars.get(symbol) {
        return Some(bar);
    }
    last_closed_bar(snapshot.bars_for(symbol))
}

fn ticker_for<'a>(tickers: &'a [Ticker], symbol: &str) -> Option<&'a Ticker> {
    tickers.iter().find(|t| t.symbol == symbol)
}

fn attach_stop_from_entry(pos: &Position, mark: Decimal, p: &ContinuationParams) -> Decision {
    let cand = match candidate_stop(pos.entry_price, "LONG", p.min_stop_pct) {
        Ok(c) => c,
        Err(_) => return Decision::hold("cannot attach stop from entry"),
    };
    if mark <= cand {
        return Decision::ExitPosition {
            reason: "continuation stop from entry".into(),
            symbol: pos.symbol.clone(),
        };
    }
    if !long_stop_is_valid(cand, mark) {
        return Decision::hold("cannot attach stop from entry");
    }
    Decision::AmendStop {
        stop_loss: cand,
        reason: "attach stop from entry".into(),
        symbol: pos.symbol.clone(),
    }
}

pub fn is_reversing(
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    recent_leaders: &[String],
    p: &ContinuationParams,
) -> bool {
    let bars = snapshot.bars_for(&ticker.symbol);
    let was_leader = recent_leaders
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&ticker.symbol))
        || week_change(ticker, bars) >= p.week_leader_pct;
    if !was_leader {
        return false;
    }
    let top: HashSet<String> = pick_recent_leaders(
        &snapshot.tickers,
        p.max_positions.max(5) as usize,
        &[],
        p,
    )
    .into_iter()
    .map(|s| s.to_ascii_uppercase())
    .collect();
    let dropped = !top.contains(&ticker.symbol.to_ascii_uppercase());
    let red = signal_bar(snapshot, &ticker.symbol)
        .map(|b| bar_is_red(Some(b)))
        .unwrap_or(false);
    dropped || red
}

pub fn manage_continuation_long(
    pos: &Position,
    snapshot: &MarketSnapshot,
    p: &ContinuationParams,
    now: f64,
    already_scaled: bool,
) -> Decision {
    if pos.side != Side::Long || pos.qty <= Decimal::ZERO {
        return Decision::hold("continuation is buy-only; short not managed");
    }
    let mark = ticker_for(&snapshot.tickers, &pos.symbol)
        .map(|t| t.last_price)
        .or_else(|| last_closed_bar(snapshot.bars_for(&pos.symbol)).map(|b| b.close))
        .unwrap_or(Decimal::ZERO);
    if mark <= Decimal::ZERO {
        return Decision::hold("no mark for open position");
    }
    if let Some(tp) = pos.take_profit {
        if mark >= tp {
            return Decision::ExitPosition {
                reason: "continuation take profit".into(),
                symbol: pos.symbol.clone(),
            };
        }
    }
    let Some(sl) = pos.stop_loss else {
        return attach_stop_from_entry(pos, mark, p);
    };
    if mark <= sl {
        return Decision::ExitPosition {
            reason: "continuation stop loss".into(),
            symbol: pos.symbol.clone(),
        };
    }
    let htf = snapshot.htf_bars_for(&pos.symbol);
    if htf.len() >= 21 {
        let closes: Vec<Decimal> = htf.iter().map(|b| b.close).collect();
        if let (Some(ema), Some(last)) = (last_ema(&closes, 20), htf.last()) {
            if last.close <= ema {
                return Decision::ExitPosition {
                    reason: "4ч сломал тренд".into(),
                    symbol: pos.symbol.clone(),
                };
            }
        }
    }
    if let Some(reason) = time_stop_reason(pos, now, p) {
        return Decision::ExitPosition {
            reason,
            symbol: pos.symbol.clone(),
        };
    }

    let peak = peak_since_entry(pos, mark, snapshot);

    // A) Pre-1R
    if sl < pos.entry_price {
        let risk = pos.entry_price - sl;
        let hit_one_r = reached_one_r(pos, mark, snapshot);
        if !hit_one_r && risk > Decimal::ZERO {
            let peak_08 = pos.entry_price + Decimal::new(8, 1) * risk;
            let near_025 = pos.entry_price + Decimal::new(25, 2) * risk;
            if peak >= peak_08 && mark < near_025 {
                let lock_025 = near_025;
                if lock_025 > sl && lock_025 < mark && long_stop_is_valid(lock_025, mark) {
                    return Decision::AmendStop {
                        stop_loss: lock_025,
                        reason: "откат с пика — замок 0.25R".into(),
                        symbol: pos.symbol.clone(),
                    };
                }
                return Decision::ExitPosition {
                    reason: "откат с пика".into(),
                    symbol: pos.symbol.clone(),
                };
            }
        }
        if hit_one_r {
            let be = pos.entry_price * (Decimal::ONE + round_trip_taker_pct());
            // Prefer scale-out half + BE first; next tick banks 1.5R on remainder.
            if !already_scaled {
                let reduce_qty = (pos.qty / Decimal::TWO).normalize();
                if reduce_qty > Decimal::ZERO
                    && reduce_qty < pos.qty
                    && be > sl
                    && be < mark
                    && long_stop_is_valid(be, mark)
                {
                    return Decision::ReduceLong {
                        symbol: pos.symbol.clone(),
                        reason: "частичная фиксация 1R".into(),
                        qty: reduce_qty,
                        stop_loss: be,
                    };
                }
            }
            if risk > Decimal::ZERO {
                let target_15 = pos.entry_price + Decimal::new(15, 1) * risk;
                let one_r = pos.entry_price + risk;
                if mark >= target_15 || (peak >= target_15 && mark >= one_r) {
                    return Decision::ExitPosition {
                        reason: "1.5R — фиксирую".into(),
                        symbol: pos.symbol.clone(),
                    };
                }
            }
            if be > sl && be < mark && long_stop_is_valid(be, mark) {
                return Decision::AmendStop {
                    stop_loss: be,
                    reason: "безубыток на 1R".into(),
                    symbol: pos.symbol.clone(),
                };
            }
            return Decision::ExitPosition {
                reason: "1R был — фиксирую".into(),
                symbol: pos.symbol.clone(),
            };
        }
        return Decision::hold("continuation hold / жду 1R");
    }

    // B) Post-BE
    let risk = position_risk(pos, p);
    if let Some(risk) = risk {
        if risk > Decimal::ZERO {
            let target_15 = pos.entry_price + Decimal::new(15, 1) * risk;
            let lock_05 = pos.entry_price + Decimal::new(5, 1) * risk;
            if mark >= target_15 {
                return Decision::ExitPosition {
                    reason: "1.5R — фиксирую".into(),
                    symbol: pos.symbol.clone(),
                };
            }
            if peak >= target_15 {
                if mark > lock_05
                    && lock_05 > sl
                    && lock_05 < mark
                    && long_stop_is_valid(lock_05, mark)
                {
                    return Decision::AmendStop {
                        stop_loss: lock_05,
                        reason: "замок 0.5R".into(),
                        symbol: pos.symbol.clone(),
                    };
                }
                return Decision::ExitPosition {
                    reason: "1.5R — фиксирую".into(),
                    symbol: pos.symbol.clone(),
                };
            }
        }
    }

    let Some(last) = signal_bar(snapshot, &pos.symbol) else {
        return Decision::hold("continuation hold / trail not raised");
    };
    let mut candidate = last.low;
    if p.trail_pct > Decimal::ZERO {
        if let Ok(pct_sl) = candidate_stop(mark, "LONG", p.trail_pct) {
            if pct_sl > candidate {
                candidate = pct_sl;
            }
        }
    }
    if candidate <= Decimal::ZERO || candidate <= sl {
        return Decision::hold("continuation hold / trail not raised");
    }
    let new_sl = match trail_stop_upward(Some(sl), candidate, "LONG") {
        Ok(v) => v,
        Err(_) => return Decision::hold("continuation hold / trail not raised"),
    };
    if new_sl > sl && long_stop_is_valid(new_sl, mark) {
        let reason = if new_sl > last.low {
            format!("trail mark {}%", (p.trail_pct * Decimal::from(100)).normalize())
        } else {
            format!("trail по минимуму {}", p.interval.as_ru())
        };
        return Decision::AmendStop {
            stop_loss: new_sl,
            reason,
            symbol: pos.symbol.clone(),
        };
    }
    Decision::hold("continuation hold / trail not raised")
}

fn position_risk(pos: &Position, p: &ContinuationParams) -> Option<Decimal> {
    if let Some(sl) = pos.stop_loss {
        if sl < pos.entry_price {
            let risk = pos.entry_price - sl;
            if risk > Decimal::ZERO {
                return Some(risk);
            }
        }
    }
    risk_from_take_profit(pos, p.reward_r)
}

fn peak_since_entry(pos: &Position, mark: Decimal, snapshot: &MarketSnapshot) -> Decimal {
    let mut peak = mark;
    if pos.qty > Decimal::ZERO && pos.unrealized_pnl > Decimal::ZERO {
        let implied = pos.entry_price + pos.unrealized_pnl / pos.qty;
        if implied > peak {
            peak = implied;
        }
    }
    for b in snapshot.bars_for(&pos.symbol) {
        let after = match pos.opened_bar_time {
            Some(since) => b.open_time >= since,
            None => true,
        };
        if after && b.high > peak {
            peak = b.high;
        }
    }
    if let Some(last) = snapshot.last_bars.get(&pos.symbol) {
        let after = match pos.opened_bar_time {
            Some(since) => last.open_time >= since,
            None => true,
        };
        if after && last.high > peak {
            peak = last.high;
        }
    }
    peak
}


fn risk_from_take_profit(pos: &Position, reward_r: Decimal) -> Option<Decimal> {
    let tp = pos.take_profit?;
    if reward_r <= Decimal::ZERO || pos.entry_price <= Decimal::ZERO {
        return None;
    }
    let gross_tp = tp / (Decimal::ONE + round_trip_taker_pct());
    if gross_tp <= pos.entry_price {
        return None;
    }
    let risk = (gross_tp - pos.entry_price) / reward_r;
    if risk <= Decimal::ZERO {
        None
    } else {
        Some(risk)
    }
}

fn time_stop_reason(pos: &Position, now: f64, p: &ContinuationParams) -> Option<String> {
    if let Some(opened_ms) = pos.opened_bar_time {
        let opened = (opened_ms as f64) / 1000.0;
        if opened > 0.0 && now - opened >= 4.0 * 3600.0 {
            return Some("тайм-стоп 4ч".into());
        }
    }
    if !p.always_enter
        && !p.entry_windows.is_empty()
        && !in_entry_window(now, Some(&p.entry_windows), false)
    {
        return Some("конец окна входа".into());
    }
    None
}


fn one_r_price(pos: &Position) -> Option<Decimal> {
    let sl = pos.stop_loss?;
    if sl >= pos.entry_price || pos.entry_price <= Decimal::ZERO {
        return None;
    }
    let risk = pos.entry_price - sl;
    if risk <= Decimal::ZERO {
        None
    } else {
        Some(pos.entry_price + risk)
    }
}

fn reached_one_r(pos: &Position, mark: Decimal, snapshot: &MarketSnapshot) -> bool {
    let Some(target) = one_r_price(pos) else {
        return false;
    };
    if mark >= target {
        return true;
    }
    let risk = target - pos.entry_price;
    if pos.qty > Decimal::ZERO && pos.unrealized_pnl >= pos.qty * risk {
        return true;
    }
    let bars = snapshot.bars_for(&pos.symbol);
    // Exchange restorations often have opened_bar_time None (exchange.rs).
    // Any post-entry bar (or any high if entry time unknown) that traded 1R counts.
    let hit = if let Some(since) = pos.opened_bar_time {
        bars.iter().any(|b| b.open_time >= since && b.high >= target)
    } else {
        bars.iter().any(|b| b.high >= target)
    };
    if hit {
        return true;
    }
    last_closed_bar(bars).is_some_and(|b| b.close >= target)
}

fn hist_bars<'a>(snapshot: &'a MarketSnapshot, symbol: &str, last: &Bar) -> &'a [Bar] {
    let bars = snapshot.bars_for(symbol);
    if bars
        .last()
        .is_some_and(|b| b.open_time == last.open_time && bars.len() >= 2)
    {
        &bars[..bars.len() - 1]
    } else {
        bars
    }
}

/// Skip if the signal bar is not a green pullback-resume (red in recent history).
fn skip_no_pullback(
    snapshot: &MarketSnapshot,
    symbol: &str,
    last: &Bar,
    p: &ContinuationParams,
) -> Option<String> {
    let tf = p.interval.as_ru();
    if last.close <= last.open {
        return Some(format!("{tf} красная — не вхожу"));
    }
    let range = last.high - last.low;
    if range > Decimal::ZERO && last.close < last.low + range / Decimal::TWO {
        return Some(format!("слабое закрытие {tf} — не вхожу"));
    }
    if last.close > Decimal::ZERO && range / last.close > Decimal::new(6, 2) {
        return Some("свеча слишком широкая — не вхожу".into());
    }
    let hist = hist_bars(snapshot, symbol, last);
    if hist.len() < 2 {
        return Some("нет отката — не догоняю".into());
    }
    let recent: Vec<&Bar> = hist.iter().rev().take(5).collect();
    if !recent.iter().any(|b| b.close < b.open) {
        return Some("нет отката — не догоняю".into());
    }
    if let Some(prev) = hist.last() {
        if last.close <= prev.close {
            return Some("нет продолжения вверх — не вхожу".into());
        }
    }
    if p.volume_confirm_frac > Decimal::ZERO {
        if let Some(avg_vol) = mean_volume(hist) {
            if avg_vol > Decimal::ZERO && last.volume < avg_vol * p.volume_confirm_frac {
                return Some("слабый объём — не подтверждено".into());
            }
        }
    }
    if p.min_pullback_pct > Decimal::ZERO {
        let swing_high = recent.iter().map(|b| b.high).max().unwrap_or(last.high);
        let pullback_low = recent.iter().map(|b| b.low).min().unwrap_or(last.low);
        if swing_high > Decimal::ZERO {
            let depth = (swing_high - pullback_low) / swing_high;
            if depth < p.min_pullback_pct {
                return Some("откат слишком мелкий — не вхожу".into());
            }
        }
    }
    None
}

/// Skip unless last 4h close is above EMA20. Missing 4h history skips.
/// When two 4h swing lows exist and the newer is not higher, skip.
/// Missing swings do not skip (EMA remains the hard gate).
/// Signal-TF higher-lows stay in `skip_no_uptrend`.
pub fn skip_no_htf_trend(snapshot: &MarketSnapshot, symbol: &str) -> Option<String> {
    let bars = snapshot.htf_bars_for(symbol);
    if bars.len() < 21 {
        return Some("нет 4ч истории — не вхожу".into());
    }
    let closes: Vec<Decimal> = bars.iter().map(|b| b.close).collect();
    let Some(ema) = last_ema(&closes, 20) else {
        return Some("нет 4ч истории — не вхожу".into());
    };
    let Some(last) = bars.last() else {
        return Some("нет 4ч истории — не вхожу".into());
    };
    if last.close <= ema {
        return Some("4ч ниже EMA20 — не вхожу".into());
    }
    match last_two_swing_lows(bars) {
        Some((prev, newer)) if newer <= prev => Some("4ч нет higher low — не вхожу".into()),
        _ => None,
    }
}

/// Skip unless last close is above EMA20 and the last two swing lows rise.
fn skip_no_uptrend(snapshot: &MarketSnapshot, symbol: &str, p: &ContinuationParams) -> Option<String> {
    let tf = p.interval.as_ru();
    let bars = snapshot.bars_for(symbol);
    if bars.len() < 21 {
        return Some(format!("нет {tf} истории — не вхожу"));
    }
    let closes: Vec<Decimal> = bars.iter().map(|b| b.close).collect();
    let Some(ema) = last_ema(&closes, 20) else {
        return Some(format!("нет {tf} истории — не вхожу"));
    };
    let last = bars.last()?;
    if last.close <= ema {
        return Some("цена ниже EMA20 — не вхожу".into());
    }
    match last_two_swing_lows(bars) {
        Some((prev, newer)) if newer > prev => None,
        Some(_) => Some("нет серии higher low — не вхожу".into()),
        None => Some("нет серии higher low — не вхожу".into()),
    }
}

/// Computes a structural stop loss combining N-bar low lookback and ATR-based widening.
/// Returns None if the resulting risk is outside [min_stop_pct, max_stop_pct].
fn structure_stop(
    snapshot: &MarketSnapshot,
    symbol: &str,
    last: &Bar,
    mark: Decimal,
    p: &ContinuationParams,
) -> Option<Decimal> {
    if mark <= Decimal::ZERO {
        return None;
    }
    // Structural low: scan stop_lookback bars back to find the deepest support
    let hist = hist_bars(snapshot, symbol, last);
    let mut sl = last.low;
    for bar in hist.iter().rev().take(p.stop_lookback) {
        if bar.low > Decimal::ZERO {
            sl = sl.min(bar.low);
        }
    }
    // ATR-adaptive stop: take the wider (lower) of structural and ATR-derived stop.
    // This prevents a tight structural stop from being hit by normal volatility.
    if p.atr_period > 0 {
        let bars = snapshot.bars_for(symbol);
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
    let risk_pct = (mark - sl) / mark;
    if risk_pct > p.max_stop_pct {
        return None;
    }
    if risk_pct < p.min_stop_pct {
        sl = mark * (Decimal::ONE - p.min_stop_pct);
    }
    if !long_stop_is_valid(sl, mark) {
        return None;
    }
    Some(sl)
}

fn enter_from_ticker(snapshot: &MarketSnapshot, ticker: &Ticker, p: &ContinuationParams) -> Decision {
    if is_major_symbol(&ticker.symbol) {
        return Decision::hold("мажор — не беру в S4");
    }
    if is_junk_symbol(&ticker.symbol) || ticker.last_price < p.min_price {
        return Decision::hold("мелочь — не гоняю");
    }
    let Some(last) = signal_bar(snapshot, &ticker.symbol).cloned() else {
        return Decision::hold(format!("нет {} бара — не вхожу", p.interval.as_ru()));
    };
    let Some(sl) = structure_stop(snapshot, &ticker.symbol, &last, ticker.last_price, p) else {
        return Decision::hold("стоп слишком широкий — не вхожу");
    };
    let risk = ticker.last_price - sl;
    if risk <= Decimal::ZERO {
        return Decision::hold("computed stop invalid");
    }
    let tp = ticker.last_price + p.reward_r * risk;
    let tp = tp * (Decimal::ONE + round_trip_taker_pct());
    if tp <= ticker.last_price {
        return Decision::hold("computed stop invalid");
    }
    Decision::EnterLong {
        symbol: ticker.symbol.clone(),
        reason: format!("откат ликвид {}%", ticker.price_change_percent),
        take_profit: tp,
        stop_loss: sl,
    }
}

fn skip_24h_tape(ticker: &Ticker, p: &ContinuationParams) -> Option<String> {
    let c = ticker.price_change_percent;
    // Dumps and dead tape stay out. A green day above `stretch_pct` is a
    // pullback candidate — chase is `near_24h_high`, not "anyone +4%".
    if c <= -p.stretch_pct {
        return Some("улетело за день — не догоняю".into());
    }
    if c < Decimal::ZERO || c < p.min_change_percent {
        return Some("слабый рост 24h — не вхожу".into());
    }
    if let Some(max_c) = p.max_change_percent {
        if c > max_c {
            return Some("улетело за день — не догоняю".into());
        }
    }
    None
}

/// Per-ticker S4 setup skip (no hours / halt / cooldown gates). `None` = ready.
pub fn s4_setup_skip(
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    p: &ContinuationParams,
    exclude: &[String],
) -> Option<String> {
    let liquid = liquid_keys(&snapshot.tickers, exclude, p);
    let leaders = pick_recent_leaders(
        &snapshot.tickers,
        p.max_positions.max(5) as usize,
        exclude,
        p,
    );
    skip_new_long(snapshot, ticker, p, &leaders, &liquid)
}

pub fn pick_strategy4_book(
    tickers: &[Ticker],
    n: usize,
    exclude: &[String],
    p: Option<&ContinuationParams>,
) -> Vec<Ticker> {
    if n == 0 {
        return Vec::new();
    }
    let owned = ContinuationParams::default();
    let p = p.unwrap_or(&owned);
    let mut rows: Vec<Ticker> = liquid_universe(tickers, exclude, p)
        .into_iter()
        .filter(|t| {
            if let Some(reason) = skip_24h_tape(t, p) {
                note_s4_skip(&reason);
                return false;
            }
            if near_24h_high(t, p.near_high_frac) {
                note_s4_skip(NEAR_HIGH_SKIP);
                return false;
            }
            true
        })
        .cloned()
        .collect();
    rows.sort_by(|a, b| {
        b.quote_volume
            .cmp(&a.quote_volume)
            .then(b.symbol.cmp(&a.symbol))
    });
    rows.truncate(n);
    rows
}

/// Top of the 24h tape by percent, including stretched names.
/// Entry book filters stretch; leader memory must not, or a pump is forgotten
/// and a reversing long waits for the exchange SL.
fn pick_recent_leaders(
    tickers: &[Ticker],
    n: usize,
    exclude: &[String],
    p: &ContinuationParams,
) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let skip: HashSet<String> = exclude
        .iter()
        .map(|s| s.to_ascii_uppercase())
        .collect();
    let mut rows: Vec<&Ticker> = tickers
        .iter()
        .filter(|t| {
            !skip.contains(&t.symbol.to_ascii_uppercase())
                && t.last_price > Decimal::ZERO
                && has_tape(t, p)
        })
        .collect();
    rows.sort_by(|a, b| {
        b.price_change_percent
            .cmp(&a.price_change_percent)
            .then(b.quote_volume.cmp(&a.quote_volume))
            .then(b.symbol.cmp(&a.symbol))
    });
    rows.into_iter()
        .take(n)
        .map(|t| t.symbol.clone())
        .collect()
}

pub fn continuation_decision(
    snapshot: &MarketSnapshot,
    position: Option<&Position>,
    now: f64,
    params: Option<&ContinuationParams>,
) -> Decision {
    let held: Vec<Position> = position
        .filter(|p| p.qty > Decimal::ZERO)
        .cloned()
        .into_iter()
        .collect();
    let empty: HashMap<String, f64> = HashMap::new();
    let scaled: HashSet<String> = HashSet::new();
    let (d, _, _) = continuation_decisions(
        snapshot,
        &held,
        now,
        0.0,
        &[],
        &empty,
        params,
        &[],
        true,
        &[],
        0.0,
        &scaled,
    );
    d.into_iter().next().unwrap_or_else(|| Decision::hold("hold"))
}

/// Final per-ticker gate before entry. 24h tape filters are repeated here so a
/// dump cannot sneak in if it still made the book.
fn skip_new_long(
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    p: &ContinuationParams,
    recent_leaders: &[String],
    liquid: &HashSet<String>,
) -> Option<String> {
    if is_major_symbol(&ticker.symbol) {
        return Some("мажор — не беру в S4".into());
    }
    if is_junk_symbol(&ticker.symbol) || ticker.last_price < p.min_price {
        return Some("мелочь — не гоняю".into());
    }
    if !liquid.contains(&ticker.symbol.to_ascii_uppercase()) {
        return Some("тонкий стакан — не гоняю".into());
    }
    if let Some(reason) = skip_24h_tape(ticker, p) {
        return Some(reason);
    }
    let Some(bar) = signal_bar(snapshot, &ticker.symbol) else {
        return Some(format!("нет {} бара — не вхожу", p.interval.as_ru()));
    };
    if let Some(reason) = skip_no_htf_trend(snapshot, &ticker.symbol) {
        return Some(reason);
    }
    if let Some(reason) = skip_no_pullback(snapshot, &ticker.symbol, bar, p) {
        return Some(reason);
    }
    if let Some(reason) = skip_no_uptrend(snapshot, &ticker.symbol, p) {
        return Some(reason);
    }
    if is_reversing(snapshot, ticker, recent_leaders, p) {
        return Some("разворот бывшего лидера — не гоняю".into());
    }
    let bars = snapshot.bars_for(&ticker.symbol);
    if !bars.is_empty() {
        if let Some(vwap_price) = vwap(bars) {
            if ticker.last_price < vwap_price {
                return Some("цена ниже VWAP — не вхожу".into());
            }
        }
    }
    if structure_stop(snapshot, &ticker.symbol, bar, ticker.last_price, p).is_none() {
        return Some("стоп слишком широкий — не вхожу".into());
    }
    None
}

fn manage_open_book(
    positions: &[Position],
    snapshot: &MarketSnapshot,
    p: &ContinuationParams,
    now: f64,
    scaled_one_r: &HashSet<String>,
) -> Vec<Decision> {
    let mut out = Vec::new();
    for pos in positions {
        if pos.qty <= Decimal::ZERO {
            continue;
        }
        let already = scaled_one_r
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&pos.symbol));
        let decision = manage_continuation_long(pos, snapshot, p, now, already);
        if !decision.is_hold() {
            out.push(decision);
        }
    }
    out
}

fn maybe_enter(
    snapshot: &MarketSnapshot,
    positions: &[Position],
    now: f64,
    inflight: &[String],
    cooldowns: &HashMap<String, f64>,
    p: &ContinuationParams,
    exclude: &[String],
    recent_leaders: &[String],
    mut out: Vec<Decision>,
) -> Vec<Decision> {
    let held: HashSet<String> = positions
        .iter()
        .filter(|pos| pos.qty > Decimal::ZERO)
        .map(|pos| pos.symbol.to_ascii_uppercase())
        .collect();
    let mut blocked = held.clone();
    for s in inflight {
        blocked.insert(s.to_ascii_uppercase());
    }
    for (sym, until) in cooldowns {
        if now < *until {
            blocked.insert(sym.to_ascii_uppercase());
        }
    }
    let mut slots = p.max_positions
        - held.len() as i32
        - inflight
            .iter()
            .filter(|s| !held.contains(&s.to_ascii_uppercase()))
            .count() as i32;
    let not_green: Vec<&Position> = positions
        .iter()
        .filter(|pos| pos.qty > Decimal::ZERO && pos.unrealized_pnl <= Decimal::ZERO)
        .collect();
    if !not_green.is_empty() {
        slots = 0;
    }
    let liquid = liquid_keys(&snapshot.tickers, exclude, p);
    let mut last_skip: Option<String> = None;
    let book = pick_strategy4_book(
        &snapshot.tickers,
        p.liquid_n.max(1),
        exclude,
        Some(p),
    );
    for ticker in book {
        if slots <= 0 {
            break;
        }
        if blocked.contains(&ticker.symbol.to_ascii_uppercase()) {
            continue;
        }
        if let Some(reason) = skip_new_long(snapshot, &ticker, p, recent_leaders, &liquid) {
            note_s4_skip(&reason);
            last_skip = Some(reason);
            continue;
        }
        let decision = enter_from_ticker(snapshot, &ticker, p);
        if let Decision::EnterLong { .. } = &decision {
            blocked.insert(ticker.symbol.to_ascii_uppercase());
            slots = 0;
            out.push(decision);
        }
    }
    if !out.is_empty() {
        out
    } else if !not_green.is_empty() {
        vec![Decision::hold("слот не в плюсе — новый не открываю")]
    } else if held.len() as i32 >= p.max_positions {
        vec![Decision::hold("continuation book full")]
    } else {
        vec![Decision::hold(
            last_skip.unwrap_or_else(|| "нет входа в топ роста".into()),
        )]
    }
}

/// Manage open longs, then at most one new enter per 60s scan.
pub fn continuation_decisions(
    snapshot: &MarketSnapshot,
    positions: &[Position],
    now: f64,
    last_scan_ts: f64,
    inflight: &[String],
    cooldowns: &HashMap<String, f64>,
    params: Option<&ContinuationParams>,
    exclude: &[String],
    allow_enter: bool,
    recent_leaders: &[String],
    desk_until: f64,
    scaled_one_r: &HashSet<String>,
) -> (Vec<Decision>, f64, Vec<String>) {
    let owned = ContinuationParams::default();
    let p = params.unwrap_or(&owned);
    let leaders: Vec<String> = pick_recent_leaders(
        &snapshot.tickers,
        p.max_positions.max(5) as usize,
        exclude,
        p,
    );
    let out = manage_open_book(positions, snapshot, p, now, scaled_one_r);
    if !allow_enter {
        if !out.is_empty() {
            return (out, last_scan_ts, leaders);
        }
        return (
            vec![Decision::hold("стоп дня. Новых входов нет до 00:00 UTC.")],
            last_scan_ts,
            leaders,
        );
    }
    if !in_entry_window(now, Some(&p.entry_windows), p.always_enter) {
        if out.is_empty() {
            let status = session_status(now, Some(&p.entry_windows), p.always_enter);
            return (
                vec![Decision::hold(outside_entry_reason(&status))],
                last_scan_ts,
                leaders,
            );
        }
        return (out, last_scan_ts, leaders);
    }
    if now < desk_until {
        if out.is_empty() {
            return (
                vec![Decision::hold("пауза после стопа — слот не заполняю")],
                last_scan_ts,
                leaders,
            );
        }
        return (out, last_scan_ts, leaders);
    }
    if !scan_due(last_scan_ts, now) {
        if out.is_empty() {
            return (
                vec![Decision::hold("waiting for next scan")],
                last_scan_ts,
                leaders,
            );
        }
        return (out, last_scan_ts, leaders);
    }
    let out = maybe_enter(
        snapshot,
        positions,
        now,
        inflight,
        cooldowns,
        p,
        exclude,
        recent_leaders,
        out,
    );
    (out, now, leaders)
}

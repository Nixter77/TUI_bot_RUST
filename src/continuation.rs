//! Strategy 4: liquid continuation vs illiquid weekly-leader skip (long-only).

use crate::indicators::{last_atr, mean_volume, vwap};
use crate::journal::round_trip_taker_pct;
use crate::models::{
    bar_is_red, last_closed_bar, near_24h_high, Bar, Decision, MarketSnapshot, Position, Side, Ticker,
};
use crate::ranking::is_junk_symbol;
use crate::sessions::{
    in_entry_window, outside_entry_reason, session_status, HourWindow, DEFAULT_ENTRY_WINDOWS,
};
use crate::trail::{candidate_stop, long_stop_is_valid, trail_stop_upward};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

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
}

impl Default for ContinuationParams {
    fn default() -> Self {
        Self {
            tp_pct: Decimal::new(25, 3),
            trail_pct: Decimal::new(20, 3),
            min_change_percent: Decimal::new(4, 1),
            min_quote_volume: Decimal::from(50_000),
            min_price: Decimal::new(5, 2),
            max_change_percent: Some(Decimal::from(12)),
            liquid_frac: Decimal::new(2, 2),
            liquid_n: 12,
            week_leader_pct: Decimal::from(4),
            stretch_pct: Decimal::from(4),
            near_high_frac: Decimal::new(2, 2),
            reward_r: Decimal::from(2),
            min_stop_pct: Decimal::new(8, 3),
            max_stop_pct: Decimal::new(25, 3),
            always_enter: false,
            entry_windows: DEFAULT_ENTRY_WINDOWS.to_vec(),
            cooldown_sec: 1800.0,
            max_positions: 3,
            atr_period: 14,
            atr_k: Decimal::new(15, 1),       // 1.5
            volume_confirm_frac: Decimal::new(8, 1), // 0.8
            min_pullback_pct: Decimal::new(3, 3),    // 0.003
            stop_lookback: 3,
        }
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
    let mut rows: Vec<&Ticker> = tickers
        .iter()
        .filter(|t| {
            if skip.contains(&t.symbol.to_ascii_uppercase()) || is_junk_symbol(&t.symbol) {
                return false;
            }
            t.last_price > Decimal::ZERO
                && t.last_price >= p.min_price
                && t.quote_volume >= p.min_quote_volume
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

pub fn is_liquid(ticker: &Ticker, tickers: &[Ticker], p: &ContinuationParams) -> bool {
    liquid_universe(tickers, &[], p)
        .iter()
        .any(|t| t.symbol.eq_ignore_ascii_case(&ticker.symbol))
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

pub fn is_illiquid_reversal(
    ticker: &Ticker,
    _tickers: &[Ticker],
    bars: &[Bar],
    p: &ContinuationParams,
) -> bool {
    let was = week_change(ticker, bars) >= p.week_leader_pct
        || ticker.price_change_percent >= p.stretch_pct
        || near_24h_high(ticker, p.near_high_frac);
    was && !has_tape(ticker, p)
}

fn bars_for<'a>(snapshot: &'a MarketSnapshot, symbol: &str) -> &'a [Bar] {
    if let Some(extra) = snapshot.universe_bars.get(symbol) {
        if !extra.is_empty() {
            return extra;
        }
    }
    if symbol == snapshot.chart_symbol || snapshot.chart_symbol.is_empty() {
        return &snapshot.bars;
    }
    &[]
}

fn five_min_bar<'a>(snapshot: &'a MarketSnapshot, symbol: &str) -> Option<&'a Bar> {
    if let Some(bar) = snapshot.last_bars.get(symbol) {
        return Some(bar);
    }
    last_closed_bar(bars_for(snapshot, symbol))
}

fn ticker_for<'a>(tickers: &'a [Ticker], symbol: &str) -> Option<&'a Ticker> {
    tickers.iter().find(|t| t.symbol == symbol)
}

fn attach_stop_from_entry(pos: &Position, mark: Decimal, p: &ContinuationParams) -> Decision {
    let cand = match candidate_stop(pos.entry_price, "LONG", p.trail_pct) {
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
    let bars = bars_for(snapshot, &ticker.symbol);
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
    let red = five_min_bar(snapshot, &ticker.symbol)
        .map(|b| bar_is_red(Some(b)))
        .unwrap_or(false);
    dropped || red
}

pub fn manage_continuation_long(
    pos: &Position,
    snapshot: &MarketSnapshot,
    p: &ContinuationParams,
    recent_leaders: &[String],
) -> Decision {
    if pos.side != Side::Long || pos.qty <= Decimal::ZERO {
        return Decision::hold("continuation is buy-only; short not managed");
    }
    let mark = ticker_for(&snapshot.tickers, &pos.symbol)
        .map(|t| t.last_price)
        .or_else(|| last_closed_bar(bars_for(snapshot, &pos.symbol)).map(|b| b.close))
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
    // Exit if price drops below entry AND breaks the 5m bar low (bar_near_mark was always true)
    if mark < pos.entry_price {
        if let Some(bar) = five_min_bar(snapshot, &pos.symbol) {
            if mark < bar.low {
                return Decision::ExitPosition {
                    reason: "пробой минимума 5м — закрываю".into(),
                    symbol: pos.symbol.clone(),
                };
            }
        }
    }
    if bar_is_red(five_min_bar(snapshot, &pos.symbol)) {
        return Decision::ExitPosition {
            reason: "5м разворот — закрываю до стопа".into(),
            symbol: pos.symbol.clone(),
        };
    }
    if let Some(ticker) = ticker_for(&snapshot.tickers, &pos.symbol) {
        if is_reversing(snapshot, ticker, recent_leaders, p) {
            return Decision::ExitPosition {
                reason: "разворот бывшего лидера".into(),
                symbol: pos.symbol.clone(),
            };
        }
    }
    if pos.stop_loss.is_none() {
        return attach_stop_from_entry(pos, mark, p);
    }
    let sl = pos.stop_loss.unwrap();
    if mark <= sl {
        return Decision::ExitPosition {
            reason: "continuation stop loss".into(),
            symbol: pos.symbol.clone(),
        };
    }
    let cand = match candidate_stop(mark, "LONG", p.trail_pct) {
        Ok(c) => c,
        Err(_) => return Decision::hold("continuation hold / trail not raised"),
    };
    let new_sl = match trail_stop_upward(Some(sl), cand, "LONG") {
        Ok(v) => v,
        Err(_) => return Decision::hold("continuation hold / trail not raised"),
    };
    if new_sl > sl && long_stop_is_valid(new_sl, mark) {
        return Decision::AmendStop {
            stop_loss: new_sl,
            reason: "trail stop вверх".into(),
            symbol: pos.symbol.clone(),
        };
    }
    Decision::hold("continuation hold / trail not raised")
}

fn hist_bars<'a>(snapshot: &'a MarketSnapshot, symbol: &str, last: &Bar) -> &'a [Bar] {
    let bars = bars_for(snapshot, symbol);
    if bars
        .last()
        .is_some_and(|b| b.open_time == last.open_time && bars.len() >= 2)
    {
        &bars[..bars.len() - 1]
    } else {
        bars
    }
}

/// Returns a skip reason if the 5m candle quality or pullback conditions are not met.
/// Checks: candle direction, close position, width, pullback presence, volume, depth.
fn skip_no_pullback(
    snapshot: &MarketSnapshot,
    symbol: &str,
    last: &Bar,
    p: &ContinuationParams,
) -> Option<String> {
    if last.close <= last.open {
        return Some("5м красная — не вхожу".into());
    }
    let range = last.high - last.low;
    if range > Decimal::ZERO && last.close < last.low + range / Decimal::TWO {
        return Some("слабое закрытие 5м — не вхожу".into());
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
    // Volume confirmation: entry candle must show conviction vs recent average
    if p.volume_confirm_frac > Decimal::ZERO {
        if let Some(avg_vol) = mean_volume(hist) {
            if avg_vol > Decimal::ZERO && last.volume < avg_vol * p.volume_confirm_frac {
                return Some("слабый объём — не подтверждено".into());
            }
        }
    }
    // Pullback depth: ensure the pullback was meaningful, not just a single tick
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
        let bars = bars_for(snapshot, symbol);
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
    let Some(last) = five_min_bar(snapshot, &ticker.symbol).cloned() else {
        return Decision::hold("нет 5м бара — не вхожу");
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
            if t.price_change_percent < p.min_change_percent {
                return false;
            }
            if t.price_change_percent >= p.stretch_pct {
                return false;
            }
            if let Some(max_c) = p.max_change_percent {
                if t.price_change_percent > max_c {
                    return false;
                }
            }
            if near_24h_high(t, p.near_high_frac) {
                return false;
            }
            true
        })
        .cloned()
        .collect();
    rows.sort_by(|a, b| {
        b.quote_volume
            .cmp(&a.quote_volume)
            .then(a.price_change_percent.cmp(&b.price_change_percent))
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
    let owned = ContinuationParams::default();
    let p = params.unwrap_or(&owned);
    if let Some(pos) = position {
        if pos.qty > Decimal::ZERO {
            return manage_continuation_long(pos, snapshot, p, &[]);
        }
    }
    if !in_entry_window(now, Some(&p.entry_windows), p.always_enter) {
        let status = session_status(now, Some(&p.entry_windows), p.always_enter);
        return Decision::hold(outside_entry_reason(&status));
    }
    let book = pick_strategy4_book(&snapshot.tickers, p.max_positions.max(1) as usize, &[], Some(p));
    let single = book.len() == 1;
    for ticker in book {
        if let Some(reason) = skip_new_long(snapshot, &ticker, p, &[]) {
            if single {
                return Decision::hold(reason);
            }
            continue;
        }
        return enter_from_ticker(snapshot, &ticker, p);
    }
    Decision::hold("no liquid continuation")
}

/// Final per-ticker gate before entry. `pick_strategy4_book` already filters
/// near_24h_high / stretch_pct / max_change_percent, so they are not duplicated here.
fn skip_new_long(
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    p: &ContinuationParams,
    recent_leaders: &[String],
) -> Option<String> {
    if is_junk_symbol(&ticker.symbol) || ticker.last_price < p.min_price {
        return Some("мелочь — не гоняю".into());
    }
    if !is_liquid(ticker, &snapshot.tickers, p) {
        return Some("тонкий стакан — не гоняю".into());
    }
    if is_reversing(snapshot, ticker, recent_leaders, p) {
        return Some("разворот бывшего лидера — не гоняю".into());
    }
    // VWAP directional filter: price must be above session VWAP
    let bars = bars_for(snapshot, &ticker.symbol);
    if !bars.is_empty() {
        if let Some(vwap_price) = vwap(bars) {
            if ticker.last_price < vwap_price {
                return Some("цена ниже VWAP — не вхожу".into());
            }
        }
    }
    let Some(bar) = five_min_bar(snapshot, &ticker.symbol) else {
        return Some("нет 5м бара — не вхожу".into());
    };
    if let Some(reason) = skip_no_pullback(snapshot, &ticker.symbol, bar, p) {
        return Some(reason);
    }
    if structure_stop(snapshot, &ticker.symbol, bar, ticker.last_price, p).is_none() {
        return Some("стоп слишком широкий — не вхожу".into());
    }
    None
}

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
) -> (Vec<Decision>, f64, Vec<String>) {
    let owned = ContinuationParams::default();
    let p = params.unwrap_or(&owned);
    let leaders: Vec<String> = pick_recent_leaders(
        &snapshot.tickers,
        p.max_positions.max(5) as usize,
        exclude,
        p,
    );
    let mut out: Vec<Decision> = Vec::new();
    for pos in positions {
        if pos.qty <= Decimal::ZERO {
            continue;
        }
        let decision = manage_continuation_long(pos, snapshot, p, recent_leaders);
        if !decision.is_hold() {
            out.push(decision);
        }
    }
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
    let due = last_scan_ts <= 0.0 || (now - last_scan_ts) >= 60.0;
    if !due {
        if out.is_empty() {
            return (
                vec![Decision::hold("waiting for next scan")],
                last_scan_ts,
                leaders,
            );
        }
        return (out, last_scan_ts, leaders);
    }
    let held: HashSet<String> = positions
        .iter()
        .filter(|p| p.qty > Decimal::ZERO)
        .map(|p| p.symbol.to_ascii_uppercase())
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
    let red: Vec<&Position> = positions
        .iter()
        .filter(|p| p.qty > Decimal::ZERO && p.unrealized_pnl < Decimal::ZERO)
        .collect();
    if !red.is_empty() {
        slots = 0;
    }
    let mut last_skip: Option<String> = None;
    let book = pick_strategy4_book(
        &snapshot.tickers,
        (p.max_positions.max(1) as usize) + 4,
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
        if let Some(reason) = skip_new_long(snapshot, &ticker, p, recent_leaders) {
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
        return (out, now, leaders);
    }
    if !red.is_empty() {
        return (
            vec![Decision::hold("слот в минусе — новый не открываю")],
            last_scan_ts,
            leaders,
        );
    }
    if held.len() as i32 >= p.max_positions {
        return (
            vec![Decision::hold("continuation book full")],
            last_scan_ts,
            leaders,
        );
    }
    (
        vec![Decision::hold(
            last_skip.unwrap_or_else(|| "нет входа в топ роста".into()),
        )],
        last_scan_ts,
        leaders,
    )
}

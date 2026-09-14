//! Strategy 1 (Momentum rider): 24h-gain book, TP/trail. Pure decisions, no HTTP.

use crate::config::{default_risk_pct, TradeInterval, STRATEGY1_POLL_SECONDS};
use crate::dayrisk::{default_daily_loss_r, default_daily_loss_usdt};
use crate::indicators::last_ema;
use crate::models::{bar_is_red, Bar, Decision, MarketSnapshot, Position, Side, Ticker};
use crate::ranking::{momentum_min_change_percent, pick_strategy1_book_with};
use crate::sessions::{
    in_entry_window, outside_entry_reason, session_status, HourWindow, DEFAULT_ENTRY_WINDOWS,
};
use crate::trail::{candidate_stop, long_stop_is_valid, take_profit_price_net, trail_stop_upward};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

/// Tick knobs. S4 fields hitch here so `engine::tick` takes one options object.
#[derive(Debug, Clone, PartialEq)]
pub struct MomentumParams {
    pub poll_seconds: i32,
    pub tp_pct: Decimal,
    pub trail_pct: Decimal,
    pub min_quote_volume: Decimal,
    pub entry_windows: Vec<HourWindow>,
    pub always_enter: bool,
    pub s4_entry_windows: Vec<HourWindow>,
    pub s4_always_enter: bool,
    pub s4_interval: TradeInterval,
    pub s4_max_positions: i32,
    pub s5_max_positions: i32,
    pub min_change_percent: Decimal,
    pub max_change_percent: Option<Decimal>,
    pub min_price: Decimal,
    pub cooldown_sec: f64,
    pub max_positions: i32,
    pub daily_loss_usdt: Decimal,
    pub daily_loss_r: Decimal,
    pub risk_pct: Decimal,
}

impl Default for MomentumParams {
    fn default() -> Self {
        Self {
            poll_seconds: STRATEGY1_POLL_SECONDS,
            tp_pct: Decimal::new(25, 3),
            trail_pct: Decimal::new(20, 3),
            min_quote_volume: Decimal::from(50_000),
            entry_windows: DEFAULT_ENTRY_WINDOWS.to_vec(),
            always_enter: false,
            s4_entry_windows: DEFAULT_ENTRY_WINDOWS.to_vec(),
            s4_always_enter: false,
            s4_interval: TradeInterval::Minute5,
            s4_max_positions: crate::config::DEFAULT_S4_MAX_POSITIONS,
            s5_max_positions: crate::config::DEFAULT_S4_MAX_POSITIONS,
            min_change_percent: momentum_min_change_percent(),
            max_change_percent: Some(Decimal::from(12)),
            min_price: Decimal::ZERO,
            cooldown_sec: 1800.0,
            max_positions: 1,
            daily_loss_usdt: default_daily_loss_usdt(),
            daily_loss_r: default_daily_loss_r(),
            risk_pct: default_risk_pct(),
        }
    }
}

pub(crate) fn mark_for(
    symbol: &str,
    tickers: &[Ticker],
    bars_close: Option<Decimal>,
) -> Option<Decimal> {
    if let Some(c) = bars_close {
        if c > Decimal::ZERO {
            return Some(c);
        }
    }
    tickers
        .iter()
        .find(|t| t.symbol == symbol && t.last_price > Decimal::ZERO)
        .map(|t| t.last_price)
}

fn manage_momentum_long(position: &Position, tickers: &[Ticker], p: &MomentumParams) -> Decision {
    if position.side != Side::Long {
        return Decision::hold("momentum is buy-only; short not managed");
    }
    let Some(mark) = mark_for(&position.symbol, tickers, None) else {
        return Decision::hold("no mark for open position");
    };
    if let Some(tp) = position.take_profit {
        if mark >= tp {
            return Decision::ExitPosition {
                reason: "momentum take profit".into(),
                symbol: position.symbol.clone(),
            };
        }
    }
    // Rank-drop and a single red 5m bar do not flatten. Live S1 0/9 WR was alt-chase
    // plus those panic exits; Python rider waits for TP / SL / trail only.
    let Some(sl) = position.stop_loss else {
        let anchor = if position.entry_price > Decimal::ZERO {
            position.entry_price
        } else {
            mark
        };
        let cand = match candidate_stop(anchor, "LONG", p.trail_pct) {
            Ok(c) => c,
            Err(_) => return Decision::hold("cannot attach stop"),
        };
        if mark <= cand {
            return Decision::ExitPosition {
                reason: "momentum stop loss".into(),
                symbol: position.symbol.clone(),
            };
        }
        if !long_stop_is_valid(cand, mark) {
            return Decision::hold("cannot attach stop");
        }
        return Decision::AmendStop {
            stop_loss: cand,
            reason: "attach stop".into(),
            symbol: position.symbol.clone(),
        };
    };
    if mark <= sl {
        return Decision::ExitPosition {
            reason: "momentum stop loss".into(),
            symbol: position.symbol.clone(),
        };
    }
    let cand = match candidate_stop(mark, "LONG", p.trail_pct) {
        Ok(c) => c,
        Err(_) => return Decision::hold("momentum hold / trail not raised"),
    };
    let new_sl = match trail_stop_upward(Some(sl), cand, "LONG") {
        Ok(v) => v,
        Err(_) => return Decision::hold("momentum hold / trail not raised"),
    };
    if new_sl > sl && long_stop_is_valid(new_sl, mark) {
        return Decision::AmendStop {
            stop_loss: new_sl,
            reason: "trail stop вверх".into(),
            symbol: position.symbol.clone(),
        };
    }
    Decision::hold("momentum hold / trail not raised")
}

/// 4h close must be above EMA20 when history exists. Missing 4h = fail-open
/// (unit tests and the first live ticks before klines land).
fn s1_htf_skip(snapshot: Option<&MarketSnapshot>, symbol: &str) -> Option<String> {
    let snap = snapshot?;
    let bars = snap.htf_bars_for(symbol);
    if bars.len() < 21 {
        return None;
    }
    let closes: Vec<Decimal> = bars.iter().map(|b| b.close).collect();
    let ema = last_ema(&closes, 20)?;
    let last = bars.last()?;
    if last.close <= ema {
        return Some("4ч ниже EMA20 — не вхожу".into());
    }
    None
}

/// ~3 calendar days from closed 4h bars (18×4h). None = not enough history.
fn s1_ret_3d_pct(htf: &[Bar]) -> Option<Decimal> {
    // Need prior close ~72h back: index len-1-18.
    if htf.len() < 19 {
        return None;
    }
    let last = htf.last()?;
    let prev = &htf[htf.len() - 19];
    if prev.close <= Decimal::ZERO {
        return None;
    }
    Some((last.close - prev.close) / prev.close * Decimal::from(100))
}

/// ~1h return from 5m universe bars (12 bars). None = not enough history.
fn s1_ret_1h_pct(bars: &[Bar]) -> Option<Decimal> {
    if bars.len() < 13 {
        return None;
    }
    let last = bars.last()?;
    let prev = &bars[bars.len() - 13];
    if prev.close <= Decimal::ZERO {
        return None;
    }
    Some((last.close - prev.close) / prev.close * Decimal::from(100))
}

/// Research (~28d majors): 24h in [2%, 4%) faded on that sample (trail-heavy, thin 1h MFE);
/// +4%…+12% continued more often when multi-day was still green. Missing bars = fail-open.
/// Not a proven edge — cargo `--backtest` on this crate is still unprofitable.
fn s1_edge_skip(ticker: &Ticker, snapshot: Option<&MarketSnapshot>) -> Option<String> {
    let c24 = ticker.price_change_percent;
    // Mid-band mean-reversion pocket — not "new indicators", just don't buy the fade.
    if c24 >= Decimal::from(2) && c24 < Decimal::from(4) {
        return Some("24h mid-band fade — не вхожу".into());
    }
    let Some(snap) = snapshot else {
        return None;
    };
    let htf = snap.htf_bars_for(&ticker.symbol);
    if let Some(r3) = s1_ret_3d_pct(htf) {
        // Longer lookback: skip one-day spikes that already faded on 3d.
        if r3 < Decimal::ONE {
            return Some("3д импульс слабый — не вхожу".into());
        }
    }
    // Late chase: hot 24h already given back on the 1h — skip when 5m depth exists.
    if c24 >= Decimal::new(25, 1) {
        if let Some(bars) = snap.universe_bars.get(&ticker.symbol) {
            if let Some(r1) = s1_ret_1h_pct(bars) {
                if r1 < Decimal::new(1, 1) {
                    // < +0.1%
                    return Some("догон 24h без 1h — не вхожу".into());
                }
            }
        }
    }
    None
}

fn enter_from_ticker(ticker: &Ticker, p: &MomentumParams) -> Decision {
    let tp = match take_profit_price_net(ticker.last_price, "LONG", p.tp_pct) {
        Ok(v) => v,
        Err(_) => return Decision::hold("computed stop invalid"),
    };
    let sl = match candidate_stop(ticker.last_price, "LONG", p.trail_pct) {
        Ok(v) => v,
        Err(_) => return Decision::hold("computed stop invalid"),
    };
    if !long_stop_is_valid(sl, ticker.last_price) {
        return Decision::hold("computed stop invalid");
    }
    let rank_note = if p.max_positions > 1 {
        format!(
            "top {} rising {}%",
            p.max_positions, ticker.price_change_percent
        )
    } else {
        format!("most rising {}%", ticker.price_change_percent)
    };
    Decision::EnterLong {
        symbol: ticker.symbol.clone(),
        reason: rank_note,
        take_profit: tp,
        stop_loss: sl,
    }
}

/// Per-ticker S1 setup skip (no hours / halt / cooldown gates). `None` = ready.
pub fn s1_setup_skip(
    ticker: &Ticker,
    last_bars: &HashMap<String, Bar>,
    in_book: bool,
    snapshot: Option<&MarketSnapshot>,
) -> Option<String> {
    if let Some(snap) = snapshot {
        if let Some(reason) = crate::regime::block_alt_entry(snap) {
            return Some(reason);
        }
    }
    if !in_book {
        return Some("не в топе роста".into());
    }
    if let Some(reason) = s1_htf_skip(snapshot, &ticker.symbol) {
        return Some(reason);
    }
    if let Some(reason) = s1_edge_skip(ticker, snapshot) {
        return Some(reason);
    }
    if !last_bars.is_empty() {
        match last_bars.get(&ticker.symbol) {
            None => return Some("нет 5м бара — не вхожу".into()),
            Some(bar) if bar_is_red(Some(bar)) => return Some("5м красная — не вхожу".into()),
            Some(_) => {}
        }
    }
    None
}

pub fn momentum_decisions(
    tickers: &[Ticker],
    positions: &[Position],
    now: f64,
    last_scan_ts: f64,
    inflight: &[String],
    cooldowns: &HashMap<String, f64>,
    params: Option<&MomentumParams>,
    exclude: &[String],
    last_bars: &HashMap<String, Bar>,
    allow_enter: bool,
    desk_until: f64,
    snapshot: Option<&MarketSnapshot>,
) -> (Vec<Decision>, f64) {
    let owned = MomentumParams::default();
    let p = params.unwrap_or(&owned);
    let poll = p.poll_seconds;
    if poll != 60 && poll != 120 {
        return (vec![Decision::hold("poll_seconds must be 60 or 120")], now);
    }
    let book = pick_strategy1_book_with(
        tickers,
        p.max_positions.max(1) as usize,
        p.min_quote_volume,
        p.min_price,
        p.min_change_percent,
        p.max_change_percent,
        exclude,
    );
    let mut out: Vec<Decision> = Vec::new();
    for pos in positions {
        if pos.qty <= Decimal::ZERO {
            continue;
        }
        let decision = manage_momentum_long(pos, tickers, p);
        if !decision.is_hold() {
            out.push(decision);
        }
    }

    if !allow_enter {
        if !out.is_empty() {
            return (out, last_scan_ts);
        }
        return (
            vec![Decision::hold("стоп дня. Новых входов нет до 00:00 UTC.")],
            last_scan_ts,
        );
    }

    if !in_entry_window(now, Some(&p.entry_windows), p.always_enter) {
        if out.is_empty() {
            let status = session_status(now, Some(&p.entry_windows), p.always_enter);
            return (
                vec![Decision::hold(outside_entry_reason(&status))],
                last_scan_ts,
            );
        }
        return (out, last_scan_ts);
    }
    if now < desk_until {
        if out.is_empty() {
            return (
                vec![Decision::hold("пауза после стопа — слот не заполняю")],
                last_scan_ts,
            );
        }
        return (out, last_scan_ts);
    }

    let due = last_scan_ts <= 0.0 || (now - last_scan_ts) >= poll as f64;
    if !due {
        if out.is_empty() {
            return (vec![Decision::hold("waiting for next scan")], last_scan_ts);
        }
        return (out, last_scan_ts);
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
    let mut skipped_red = false;
    let mut skipped_no_bar = false;
    let mut skipped_htf = false;
    if let Some(snap) = snapshot {
        if let Some(reason) = crate::regime::block_alt_entry(snap) {
            if out.is_empty() {
                return (vec![Decision::hold(reason)], now);
            }
            return (out, last_scan_ts);
        }
    }
    for ticker in &book {
        if slots <= 0 {
            break;
        }
        if blocked.contains(&ticker.symbol.to_ascii_uppercase()) {
            continue;
        }
        if s1_htf_skip(snapshot, &ticker.symbol).is_some() {
            skipped_htf = true;
            continue;
        }
        if s1_edge_skip(ticker, snapshot).is_some() {
            continue;
        }
        if !last_bars.is_empty() {
            match last_bars.get(&ticker.symbol) {
                None => {
                    skipped_no_bar = true;
                    continue;
                }
                Some(bar) if bar_is_red(Some(bar)) => {
                    skipped_red = true;
                    continue;
                }
                Some(_) => {}
            }
        } else if bar_is_red(last_bars.get(&ticker.symbol)) {
            skipped_red = true;
            continue;
        }
        let decision = enter_from_ticker(ticker, p);
        if let Decision::EnterLong { .. } = &decision {
            blocked.insert(ticker.symbol.to_ascii_uppercase());
            slots = 0;
            out.push(decision);
        }
    }
    if !out.is_empty() {
        return (out, now);
    }
    if skipped_htf && held.is_empty() {
        return (vec![Decision::hold("4ч ниже EMA20 — не вхожу")], now);
    }
    if skipped_red && held.is_empty() {
        return (vec![Decision::hold("5м красная — не вхожу")], now);
    }
    if skipped_no_bar && held.is_empty() && out.is_empty() {
        return (vec![Decision::hold("нет 5м бара — не вхожу")], now);
    }
    if !red.is_empty() {
        return (
            vec![Decision::hold("слот в минусе — новый не открываю")],
            now,
        );
    }
    if book.is_empty() {
        return (vec![Decision::hold("no eligible rising symbol")], now);
    }
    if held.len() as i32 >= p.max_positions {
        return (vec![Decision::hold("momentum book full")], now);
    }
    (
        vec![Decision::hold("top rising already held or cooling")],
        now,
    )
}

pub fn momentum_decision(
    tickers: &[Ticker],
    position: Option<&Position>,
    now: f64,
    last_scan_ts: f64,
    params: Option<&MomentumParams>,
) -> (Decision, f64) {
    if let Some(pos) = position {
        if pos.qty > Decimal::ZERO && pos.side != Side::Long {
            return (
                Decision::hold("momentum is buy-only; short not managed"),
                last_scan_ts,
            );
        }
    }
    let held: Vec<Position> = position
        .filter(|p| p.qty > Decimal::ZERO)
        .cloned()
        .into_iter()
        .collect();
    let empty_cool: HashMap<String, f64> = HashMap::new();
    let empty_bars: HashMap<String, Bar> = HashMap::new();
    let (decisions, scan_ts) = momentum_decisions(
        tickers,
        &held,
        now,
        last_scan_ts,
        &[],
        &empty_cool,
        params,
        &[],
        &empty_bars,
        true,
        0.0,
        None,
    );
    (
        decisions
            .into_iter()
            .next()
            .unwrap_or_else(|| Decision::hold("hold")),
        scan_ts,
    )
}

//! Strategy orchestration over market snapshots. Pure: returns decisions only.

use crate::config::STRATEGY1_POLL_SECONDS;
use crate::dayrisk::{apply_day_risk, default_daily_loss_usdt};
use crate::errors::is_retry_error;
use crate::models::{
    bar_is_red, coalesce_position, remembered_positions, unmanaged_positions, Bar, Decision, EngineState,
    MarketSnapshot, Position, Side, Ticker,
};

pub use crate::models::{Decision as EngineDecision, EngineState as PubEngineState, MarketSnapshot as PubMarketSnapshot};
use crate::profit::current_equity;
use crate::ranking::{iter_liquid_majors, momentum_min_change_percent, pick_momentum_book};
use crate::continuation::{continuation_decision, continuation_decisions, ContinuationParams};
use crate::scalp::{scalp_decision, ScalpParams};
use crate::sessions::{in_entry_window, outside_entry_reason, session_status, HourWindow, DEFAULT_ENTRY_WINDOWS};
use crate::trail::{candidate_stop, long_stop_is_valid, take_profit_price_net, trail_stop_upward};
use crate::trend::{trend_decision, TrendParams};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

pub const STRATEGY_IDS: [i32; 4] = [1, 2, 3, 4];

pub fn strategy_names() -> [(i32, &'static str); 4] {
    STRATEGY_NAMES
}

pub const STRATEGY_NAMES: [(i32, &'static str); 4] = [
    (1, "Momentum rider (растущий + TP + SL вверх)"),
    (2, "Скальп: откат к VWAP/EMA9"),
    (3, "Тренд: пробой Donchian 20/10 (день)"),
    (4, "Continuation: откат ликвидных (не догон 24h %)"),
];

pub fn strategy_title(id: i32) -> &'static str {
    STRATEGY_NAMES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, n)| *n)
        .unwrap_or("")
}

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
    pub min_change_percent: Decimal,
    pub max_change_percent: Option<Decimal>,
    pub min_price: Decimal,
    pub cooldown_sec: f64,
    pub max_positions: i32,
    pub daily_loss_usdt: Decimal,
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
            min_change_percent: momentum_min_change_percent(),
            max_change_percent: Some(Decimal::from(12)),
            min_price: Decimal::ZERO,
            cooldown_sec: 1800.0,
            max_positions: 1,
            daily_loss_usdt: default_daily_loss_usdt(),
        }
    }
}

pub fn select_strategy(raw: i32) -> Result<i32, String> {
    if !STRATEGY_IDS.contains(&raw) {
        return Err("strategy must be 1, 2, 3, or 4".into());
    }
    Ok(raw)
}

pub fn select_strategy_str(raw: &str) -> Result<i32, String> {
    let sid: i32 = raw
        .parse()
        .map_err(|_| "strategy must be 1, 2, 3, or 4".to_string())?;
    select_strategy(sid)
}

fn mark_for(symbol: &str, tickers: &[Ticker], bars_close: Option<Decimal>) -> Option<Decimal> {
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

fn manage_momentum_long(
    position: &Position,
    tickers: &[Ticker],
    last_bars: &HashMap<String, Bar>,
    book_syms: &HashSet<String>,
    p: &MomentumParams,
) -> Decision {
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
    if !book_syms.is_empty() && !book_syms.contains(&position.symbol) {
        return Decision::ExitPosition {
            reason: "выпал из топа — закрываю до стопа".into(),
            symbol: position.symbol.clone(),
        };
    }
    if bar_is_red(last_bars.get(&position.symbol)) {
        return Decision::ExitPosition {
            reason: "5м разворот — закрываю до стопа".into(),
            symbol: position.symbol.clone(),
        };
    }
    if position.stop_loss.is_none() {
        let cand = match candidate_stop(mark, "LONG", p.trail_pct) {
            Ok(c) => c,
            Err(_) => return Decision::hold("cannot attach stop"),
        };
        if !long_stop_is_valid(cand, mark) {
            return Decision::hold("cannot attach stop");
        }
        return Decision::AmendStop {
            stop_loss: cand,
            reason: "attach stop".into(),
            symbol: position.symbol.clone(),
        };
    }
    let sl = position.stop_loss.unwrap();
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
        format!("top {} rising {}%", p.max_positions, ticker.price_change_percent)
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
) -> (Vec<Decision>, f64) {
    let owned = MomentumParams::default();
    let p = params.unwrap_or(&owned);
    let poll = p.poll_seconds;
    if poll != 60 && poll != 120 {
        panic!("poll_seconds must be 60 or 120");
    }
    let book = pick_momentum_book(
        tickers,
        p.max_positions.max(1) as usize,
        p.min_quote_volume,
        p.min_price,
        p.min_change_percent,
        p.max_change_percent,
        exclude,
        true,
    );
    let book_syms: HashSet<String> = book.iter().map(|t| t.symbol.clone()).collect();
    let mut out: Vec<Decision> = Vec::new();
    for pos in positions {
        if pos.qty <= Decimal::ZERO {
            continue;
        }
        let decision = manage_momentum_long(pos, tickers, last_bars, &book_syms, p);
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
            return (vec![Decision::hold(outside_entry_reason(&status))], last_scan_ts);
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
    for ticker in &book {
        if slots <= 0 {
            break;
        }
        if blocked.contains(&ticker.symbol.to_ascii_uppercase()) {
            continue;
        }
        if crate::models::near_24h_high(ticker, Decimal::new(2, 2)) {
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
    if skipped_red && held.is_empty() {
        return (vec![Decision::hold("5м красная — не вхожу")], now);
    }
    if skipped_no_bar && held.is_empty() && out.is_empty() {
        return (vec![Decision::hold("нет 5м бара — не вхожу")], now);
    }
    if !red.is_empty() {
        return (vec![Decision::hold("слот в минусе — новый не открываю")], now);
    }
    if book.is_empty() {
        return (vec![Decision::hold("no eligible rising symbol")], now);
    }
    if held.len() as i32 >= p.max_positions {
        return (vec![Decision::hold("momentum book full")], now);
    }
    (vec![Decision::hold("top rising already held or cooling")], now)
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
            return (Decision::hold("momentum is buy-only; short not managed"), last_scan_ts);
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
    );
    (decisions.into_iter().next().unwrap_or_else(|| Decision::hold("hold")), scan_ts)
}

fn bars_for<'a>(snapshot: &'a MarketSnapshot, symbol: &str) -> &'a [Bar] {
    if let Some(extra) = snapshot.universe_bars.get(symbol) {
        if !extra.is_empty() {
            return extra;
        }
    }
    if symbol == snapshot.chart_symbol {
        return &snapshot.bars;
    }
    &[]
}

fn short_usdt(symbol: &str) -> String {
    let t = symbol.to_ascii_uppercase();
    if t.ends_with("USDT") {
        t[..t.len() - 4].to_string()
    } else {
        t
    }
}

fn combine_holds(rows: &[(String, String)]) -> String {
    if rows.is_empty() {
        return "нет символа".into();
    }
    if rows.len() == 1 {
        return rows[0].1.clone();
    }
    let mut by_reason: HashMap<String, Vec<String>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (symbol, reason) in rows {
        if !by_reason.contains_key(reason) {
            order.push(reason.clone());
            by_reason.insert(reason.clone(), Vec::new());
        }
        by_reason
            .get_mut(reason)
            .unwrap()
            .push(short_usdt(symbol));
    }
    if order.len() == 1 {
        let names = by_reason[&order[0]].join(", ");
        return format!("{} ({names})", order[0]);
    }
    let parts: Vec<String> = order
        .iter()
        .map(|reason| format!("{}: {reason}", by_reason[reason].join(", ")))
        .collect();
    format!("нет входа — {}", parts.join("; "))
}

fn desk_symbols(
    snapshot: &MarketSnapshot,
    exclude: &[String],
    cooldowns: &HashMap<String, f64>,
    now: f64,
) -> (Vec<String>, Vec<String>) {
    let skip: HashSet<String> = exclude.iter().map(|s| s.to_ascii_uppercase()).collect();
    let mut ordered: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for ticker in iter_liquid_majors(&snapshot.tickers, exclude) {
        if seen.insert(ticker.symbol.clone()) {
            ordered.push(ticker.symbol);
        }
    }
    for symbol in snapshot.universe_bars.keys() {
        if !seen.contains(symbol) && !skip.contains(&symbol.to_ascii_uppercase()) {
            ordered.push(symbol.clone());
            seen.insert(symbol.clone());
        }
    }
    let chart = &snapshot.chart_symbol;
    if !chart.is_empty() && !seen.contains(chart) && !skip.contains(&chart.to_ascii_uppercase()) {
        ordered.push(chart.clone());
    }
    let mut live = Vec::new();
    let mut cooling = Vec::new();
    for symbol in ordered {
        if now < *cooldowns.get(&symbol.to_ascii_uppercase()).unwrap_or(&0.0) {
            cooling.push(symbol);
        } else {
            live.push(symbol);
        }
    }
    (live, cooling)
}

pub fn decide(
    strategy_id: i32,
    snapshot: &MarketSnapshot,
    now: f64,
    last_scan_ts: f64,
    momentum: Option<&MomentumParams>,
    scalp: Option<&ScalpParams>,
    trend: Option<&TrendParams>,
    continuation: Option<&ContinuationParams>,
    exclude: &[String],
    cooldowns: &HashMap<String, f64>,
) -> Result<(Decision, f64), String> {
    let sid = select_strategy(strategy_id)?;
    let position = snapshot.position.as_ref();
    if sid == 1 {
        return Ok(momentum_decision(
            &snapshot.tickers,
            position,
            now,
            last_scan_ts,
            momentum,
        ));
    }
    if sid == 4 {
        return Ok((
            continuation_decision(snapshot, position, now, continuation),
            last_scan_ts,
        ));
    }
    if let Some(pos) = position {
        if pos.qty > Decimal::ZERO {
            let bars = bars_for(snapshot, &pos.symbol);
            if sid == 2 {
                return Ok((
                    scalp_decision(bars, Some(pos), &pos.symbol, scalp, Some(now)),
                    last_scan_ts,
                ));
            }
            return Ok((trend_decision(bars, Some(pos), &pos.symbol, trend), last_scan_ts));
        }
    }
    let (live, cooling) = desk_symbols(snapshot, exclude, cooldowns, now);
    if live.is_empty() {
        if !cooling.is_empty() {
            return Ok((Decision::hold("пауза после сделки"), last_scan_ts));
        }
        return Ok((Decision::hold("no symbol"), last_scan_ts));
    }
    let mut holds: Vec<(String, String)> = Vec::new();
    for symbol in live {
        let bars = bars_for(snapshot, &symbol);
        if bars.is_empty() {
            holds.push((symbol, "нет графика".into()));
            continue;
        }
        let decision = if sid == 2 {
            scalp_decision(bars, None, &symbol, scalp, Some(now))
        } else {
            trend_decision(bars, None, &symbol, trend)
        };
        if let Decision::EnterLong { .. } = &decision {
            return Ok((decision, last_scan_ts));
        }
        holds.push((symbol, decision.reason().to_string()));
    }
    Ok((Decision::hold(combine_holds(&holds)), last_scan_ts))
}

fn persist_last_error(held: Option<&str>) -> Option<String> {
    if is_retry_error(held) {
        None
    } else {
        held.map(|s| s.to_string())
    }
}

fn base_cooldown(
    strategy_id: i32,
    momentum: Option<&MomentumParams>,
    scalp: Option<&ScalpParams>,
    trend: Option<&TrendParams>,
) -> f64 {
    match strategy_id {
        1 => momentum.map(|m| m.cooldown_sec).unwrap_or(1800.0),
        2 => scalp.map(|s| s.cooldown_sec).unwrap_or(1200.0),
        4 => ContinuationParams::default().cooldown_sec,
        _ => trend.map(|t| t.cooldown_sec).unwrap_or(3600.0),
    }
}

fn cooldown_seconds(decision: &Decision, base: f64) -> f64 {
    if base <= 0.0 {
        return 0.0;
    }
    if let Decision::ExitPosition { reason, .. } = decision {
        if reason.to_ascii_lowercase().contains("take profit") {
            return base.min(300.0);
        }
    }
    base
}

fn set_cooldown(map: &mut HashMap<String, f64>, symbol: &str, until: f64) {
    let key = symbol.to_ascii_uppercase();
    if key.is_empty() {
        return;
    }
    let cur = map.get(&key).copied().unwrap_or(0.0);
    map.insert(key, cur.max(until));
}

fn describe(decision: &Decision) -> String {
    match decision {
        Decision::EnterLong {
            symbol,
            reason,
            take_profit,
            stop_loss,
        } => format!("BUY {symbol} TP={take_profit} SL={stop_loss} ({reason})"),
        Decision::AmendStop {
            stop_loss,
            reason,
            symbol,
        } => {
            let tag = if symbol.is_empty() {
                String::new()
            } else {
                format!("{symbol} ")
            };
            format!("SL {tag}-> {stop_loss} ({reason})")
        }
        Decision::ExitPosition { reason, symbol } => {
            let tag = if symbol.is_empty() {
                String::new()
            } else {
                format!("{symbol} ")
            };
            format!("EXIT {tag}({reason})")
        }
        Decision::Hold { reason } => reason.clone(),
    }
}

fn continuation_params_from_momentum(momentum: Option<&MomentumParams>) -> ContinuationParams {
    let mut p = ContinuationParams::default();
    if let Some(m) = momentum {
        p.tp_pct = m.tp_pct;
        p.trail_pct = m.trail_pct;
        p.max_positions = m.max_positions;
        p.max_change_percent = m.max_change_percent;
        p.min_quote_volume = m.min_quote_volume.max(p.min_quote_volume);
        p.always_enter = m.s4_always_enter;
        p.entry_windows = m.s4_entry_windows.clone();
    }
    p
}

pub fn tick(
    state: &EngineState,
    snapshot: &MarketSnapshot,
    now: f64,
    momentum: Option<&MomentumParams>,
    scalp: Option<&ScalpParams>,
    trend: Option<&TrendParams>,
) -> (EngineState, Decision) {
    let (new_state, decisions) = tick_decisions(state, snapshot, now, momentum, scalp, trend);
    (
        new_state,
        decisions.into_iter().next().unwrap_or_else(|| Decision::hold("hold")),
    )
}

pub fn tick_decisions(
    state: &EngineState,
    snapshot: &MarketSnapshot,
    now: f64,
    momentum: Option<&MomentumParams>,
    scalp: Option<&ScalpParams>,
    trend: Option<&TrendParams>,
) -> (EngineState, Vec<Decision>) {
    let mut state = state.clone();
    let remembered = remembered_positions(state.position.as_ref(), &state.positions);
    let (merged_list, mut inflight): (Vec<Position>, Vec<String>) = if snapshot.live_book {
        let mut live_longs: Vec<Position> = snapshot
            .open_positions
            .iter()
            .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
            .cloned()
            .collect();
        if live_longs.is_empty() {
            if let Some(pos) = &snapshot.position {
                if pos.qty > Decimal::ZERO && pos.side == Side::Long {
                    live_longs.push(pos.clone());
                }
            }
        }
        let merged: Vec<Position> = live_longs
            .iter()
            .map(|live| {
                let rem = remembered.iter().find(|r| r.symbol == live.symbol);
                coalesce_position(Some(live), rem).unwrap_or_else(|| live.clone())
            })
            .collect();
        let live_keys: HashSet<String> = merged.iter().map(|p| p.symbol.to_ascii_uppercase()).collect();
        let pending = state
            .inflight_symbols
            .iter()
            .filter(|s| !live_keys.contains(&s.to_ascii_uppercase()))
            .cloned()
            .collect();
        (merged, pending)
    } else {
        let mut merged_list = remembered.clone();
        if let Some(pos) = &snapshot.position {
            if pos.qty > Decimal::ZERO && !merged_list.iter().any(|p| p.symbol == pos.symbol) {
                let rem = remembered.iter().find(|r| r.symbol == pos.symbol);
                if let Some(extra) = coalesce_position(Some(pos), rem) {
                    merged_list.push(extra);
                }
            }
        }
        let mut inflight = state.inflight_symbols.clone();
        if state.entry_inflight && state.position.is_none() && inflight.is_empty() {
            inflight = vec!["*".into()];
        }
        (merged_list, inflight)
    };

    let merged = merged_list.first().cloned();
    let mut work = snapshot.clone();
    work.position = merged.clone();
    let prev_syms: HashSet<String> = remembered
        .iter()
        .map(|p| p.symbol.to_ascii_uppercase())
        .collect();
    let now_syms: HashSet<String> = merged_list
        .iter()
        .map(|p| p.symbol.to_ascii_uppercase())
        .collect();
    let pause_sec = base_cooldown(state.strategy_id, momentum, scalp, trend);
    let loss_windows: Vec<crate::sessions::HourWindow> = if state.strategy_id == 4 {
        momentum
            .map(|m| m.s4_entry_windows.clone())
            .unwrap_or_else(|| crate::sessions::DEFAULT_ENTRY_WINDOWS.to_vec())
    } else {
        momentum
            .map(|m| m.entry_windows.clone())
            .unwrap_or_else(|| crate::sessions::DEFAULT_ENTRY_WINDOWS.to_vec())
    };
    let mut cooldown_until = state.cooldown_until;
    let mut cooldowns = state.cooldowns.clone();
    for symbol in prev_syms.difference(&now_syms) {
        inflight.retain(|s| !s.eq_ignore_ascii_case(symbol));
        let remembered_pos = remembered
            .iter()
            .find(|p| p.symbol.eq_ignore_ascii_case(symbol));
        let mark = mark_for(symbol, &snapshot.tickers, None).unwrap_or(Decimal::ZERO);
        let won = remembered_pos
            .map(|p| crate::journal::long_close_was_win(p.entry_price, mark, p.take_profit))
            .unwrap_or(false);
        if pause_sec > 0.0 {
            set_cooldown(&mut cooldowns, symbol, now + pause_sec);
            if !won {
                let until = crate::sessions::pause_until_after_loss(now, &loss_windows, pause_sec);
                cooldown_until = cooldown_until.max(until);
            }
        }
    }
    let now_flat = merged_list.is_empty();

    let sid = state.strategy_id;
    let limit = momentum.map(|m| m.daily_loss_usdt).unwrap_or_else(default_daily_loss_usdt);
    if snapshot.account_ok {
        apply_day_risk(
            &mut state,
            now,
            current_equity(snapshot.account.wallet_balance, snapshot.account.unrealized_pnl),
            limit,
        );
    }
    let mut tail = Vec::new();
    if snapshot.live_book && now_flat {
        tail = unmanaged_positions(&snapshot.open_positions, &merged_list);
    }

    let mut next_leaders = state.recent_leaders.clone();
    let (mut decisions, scan_ts) = if state.entries_paused {
        (
            vec![Decision::hold("вход на паузе после закрытия всех")],
            state.last_scan_ts,
        )
    } else if inflight == ["*".to_string()] && now_flat {
        (vec![Decision::hold("entry in flight")], state.last_scan_ts)
    } else if !tail.is_empty() {
        let names = tail
            .iter()
            .map(|p| format!("{} {}", p.side, p.symbol))
            .collect::<Vec<_>>()
            .join(", ");
        (
            vec![Decision::hold(format!(
                "на бирже хвост {names}. Стратегия не ведёт шорты. x x закроет."
            ))],
            state.last_scan_ts,
        )
    } else if sid == 1 {
        let inflight_f: Vec<String> = inflight.iter().filter(|s| s.as_str() != "*").cloned().collect();
        momentum_decisions(
            &snapshot.tickers,
            &merged_list,
            now,
            state.last_scan_ts,
            &inflight_f,
            &cooldowns,
            momentum,
            &state.skip_symbols,
            &snapshot.last_bars,
            !state.daily_halt,
            cooldown_until,
        )
    } else if sid == 4 {
        let inflight_f: Vec<String> = inflight.iter().filter(|s| s.as_str() != "*").cloned().collect();
        let cont = continuation_params_from_momentum(momentum);
        let (d, ts, leaders) = continuation_decisions(
            snapshot,
            &merged_list,
            now,
            state.last_scan_ts,
            &inflight_f,
            &cooldowns,
            Some(&cont),
            &state.skip_symbols,
            !state.daily_halt,
            &state.recent_leaders,
            cooldown_until,
        );
        next_leaders = leaders;
        (d, ts)
    } else {
        let (decision, scan_ts) = decide(
            sid,
            &work,
            now,
            state.last_scan_ts,
            momentum,
            scalp,
            trend,
            None,
            &state.skip_symbols,
            &cooldowns,
        )
        .unwrap_or_else(|e| (Decision::hold(e), state.last_scan_ts));
        if let Decision::ExitPosition { symbol, .. } = &decision {
            if pause_sec > 0.0 {
                let wait = cooldown_seconds(&decision, pause_sec);
                if wait > 0.0 {
                    cooldown_until = cooldown_until.max(now + wait);
                    if !symbol.is_empty() {
                        set_cooldown(&mut cooldowns, symbol, now + wait);
                    }
                }
            }
        }
        (vec![decision], scan_ts)
    };

    if state.daily_halt {
        let kept: Vec<Decision> = decisions
            .into_iter()
            .filter(|d| !d.is_enter_long())
            .collect();
        decisions = if kept.is_empty() {
            vec![Decision::hold("стоп дня. Новых входов нет до 00:00 UTC.")]
        } else {
            kept
        };
    }

    for decision in &decisions {
        if let Decision::EnterLong { symbol, .. } = decision {
            if !inflight.iter().any(|s| s.eq_ignore_ascii_case(symbol)) {
                inflight.push(symbol.clone());
            }
        }
    }

    let mut actions = state.recent_actions.clone();
    for decision in &decisions {
        if !decision.is_hold() {
            actions.push(describe(decision));
        }
    }
    if actions.len() > 8 {
        actions = actions[actions.len() - 8..].to_vec();
    }
    let book: Vec<Position> = merged_list.into_iter().filter(|p| p.qty > Decimal::ZERO).collect();
    let new_state = EngineState {
        last_scan_ts: scan_ts,
        positions: book.clone(),
        position: book.first().cloned(),
        last_error: persist_last_error(state.last_error.as_deref()),
        recent_actions: actions,
        entry_inflight: !inflight.is_empty() && now_flat,
        cooldown_until,
        inflight_symbols: inflight.into_iter().filter(|s| s != "*").collect(),
        cooldowns,
        strategy_id: state.strategy_id,
        entries_paused: state.entries_paused,
        skip_symbols: state.skip_symbols,
        skip_reasons: state.skip_reasons,
        day_utc: state.day_utc,
        day_start_equity: state.day_start_equity,
        daily_halt: state.daily_halt,
        recent_leaders: next_leaders,
        sized_stops: state.sized_stops,
    };
    (
        new_state,
        if decisions.is_empty() {
            vec![Decision::hold("hold")]
        } else {
            decisions
        },
    )
}

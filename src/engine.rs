//! Strategy orchestration over market snapshots. Pure: returns decisions only.

use crate::config::default_risk_pct;
use crate::continuation::{continuation_decisions, ContinuationParams};
use crate::dayrisk::{apply_day_risk, default_daily_loss_r, default_daily_loss_usdt};
use crate::errors::is_retry_error;
use crate::models::{
    coalesce_position, push_recent, remembered_positions, unmanaged_positions, Decision, EngineState,
    MarketSnapshot, Position, Side,
};
use crate::momentum::mark_for;
use crate::profit::current_equity;
use crate::ranking::iter_liquid_majors;
use crate::scalp::{scalp_decision, ScalpParams};
use crate::trend::{trend_decision, TrendParams};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

pub use crate::momentum::{momentum_decision, momentum_decisions, MomentumParams};

pub const STRATEGY_IDS: [i32; 4] = [1, 2, 3, 4];

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
        let held: Vec<Position> = snapshot
            .open_positions
            .iter()
            .filter(|p| p.qty > Decimal::ZERO && p.side == Side::Long)
            .cloned()
            .collect();
        let held = if held.is_empty() {
            position
                .filter(|p| p.qty > Decimal::ZERO)
                .cloned()
                .into_iter()
                .collect()
        } else {
            held
        };
        let scaled = HashSet::new();
        let (d, scan_ts, _) = continuation_decisions(
            snapshot,
            &held,
            now,
            last_scan_ts,
            &[],
            cooldowns,
            continuation,
            exclude,
            true,
            &[],
            0.0,
            &scaled,
        );
        return Ok((
            d.into_iter().next().unwrap_or_else(|| Decision::hold("hold")),
            scan_ts,
        ));
    }
    if let Some(pos) = position {
        if pos.qty > Decimal::ZERO {
            let bars = snapshot.bars_for(&pos.symbol);
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
        let bars = snapshot.bars_for(&symbol);
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

fn continuation_params(momentum: Option<&MomentumParams>) -> ContinuationParams {
    let interval = momentum.map(|m| m.s4_interval).unwrap_or_default();
    let mut p = ContinuationParams::default().with_interval(interval);
    if let Some(m) = momentum {
        // Never shrink below 3; STRATEGY4_MAX_POSITIONS (default 5) sets the working cap.
        p.max_positions = m.s4_max_positions.max(3);
        p.always_enter = m.s4_always_enter;
        p.entry_windows = m.s4_entry_windows.clone();
    }
    p
}

/// First decision from `tick_decisions` (single-slot callers / dump-frame).
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

/// Pure strategy tick: no HTTP. May emit several non-hold decisions (trail + enter).
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
        let up = symbol.to_ascii_uppercase();
        state.scaled_one_r.remove(&up);
        state.rearm_miss_since.remove(&up);
        state.rearm_fail_count.remove(&up);
        let remembered_pos = remembered
            .iter()
            .find(|p| p.symbol.eq_ignore_ascii_case(symbol));
        let mark = mark_for(symbol, &snapshot.tickers, None).unwrap_or(Decimal::ZERO);
        let won = remembered_pos
            .map(|p| crate::journal::long_close_was_win(p.entry_price, mark, p.take_profit))
            .unwrap_or(false);
        if pause_sec > 0.0 {
            set_cooldown(
                &mut cooldowns,
                symbol,
                crate::journal::symbol_cooldown_until(now, won, pause_sec),
            );
            if !won {
                let until = crate::sessions::pause_until_after_loss(now, &loss_windows, pause_sec);
                cooldown_until = cooldown_until.max(until);
            }
        }
    }
    let now_flat = merged_list.is_empty();

    let sid = state.strategy_id;
    let limit = momentum.map(|m| m.daily_loss_usdt).unwrap_or_else(default_daily_loss_usdt);
    let limit_r = momentum.map(|m| m.daily_loss_r).unwrap_or_else(default_daily_loss_r);
    let risk_pct = momentum.map(|m| m.risk_pct).unwrap_or_else(default_risk_pct);
    if snapshot.account_ok {
        apply_day_risk(
            &mut state,
            now,
            current_equity(snapshot.account.wallet_balance, snapshot.account.unrealized_pnl),
            limit,
            limit_r,
            risk_pct,
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
        let cont = continuation_params(momentum);
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
            &state.scaled_one_r,
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
    if now < state.retry_until {
        let kept: Vec<Decision> = decisions
            .into_iter()
            .filter(|d| !d.is_enter_long())
            .collect();
        decisions = if kept.iter().any(|d| !d.is_hold()) {
            kept
        } else {
            vec![Decision::hold("сеть: повтор входа после сбоя")]
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
            push_recent(&mut actions, now, decision.describe());
        }
    }
    let book: Vec<Position> = merged_list.into_iter().filter(|p| p.qty > Decimal::ZERO).collect();
    let mut last_error = persist_last_error(state.last_error.as_deref());
    if last_error.is_none() {
        last_error = crate::journal::take_last_error();
    }
    let new_state = EngineState {
        last_scan_ts: scan_ts,
        positions: book.clone(),
        position: book.first().cloned(),
        last_error,
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
        retry_until: state.retry_until,
        retry_strikes: state.retry_strikes,
        rearm_miss_since: state.rearm_miss_since,
        rearm_fail_count: state.rearm_fail_count,
        scaled_one_r: state.scaled_one_r,
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

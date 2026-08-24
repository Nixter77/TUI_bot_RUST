//! Live TestNet side-effects: enter / trail / exit / panic flatten.

use crate::config::Config;
use crate::errors::{
    classify, ACTION_COOLDOWN, ACTION_IGNORE, ACTION_OPERATOR, ACTION_SKIP, COOLDOWN_SEC,
};
use crate::exchange::{
    sell_protectives_are_sized, size_market_order, ExchangeError, LiveClient,
};
use crate::flatten::{close_targets, flatten_open_book, FlattenResult};
use crate::journal;
use crate::models::{Decision, EngineState, MarketSnapshot, Position, Side};
use crate::sessions::pause_until_after_loss;
use crate::signals::{emit_decision, emit_flatten};
use crate::trail::take_profit_price_net;
use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LiveApplyResult {
    pub error: Option<String>,
    pub filled: bool,
    pub mark: Option<Decimal>,
    pub qty: Option<Decimal>,
    pub forget_symbol: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReconcileResult {
    pub skip_tick: bool,
    pub last_text: String,
}

fn skip_symbols(state: Option<&EngineState>) -> std::collections::HashSet<String> {
    state
        .map(|s| s.skip_symbols.iter().map(|x| x.to_ascii_uppercase()).collect())
        .unwrap_or_default()
}

fn held_symbols(snapshot: &MarketSnapshot, state: Option<&EngineState>) -> std::collections::HashSet<String> {
    let mut held = std::collections::HashSet::new();
    if let Some(pos) = &snapshot.position {
        if pos.qty > Decimal::ZERO {
            held.insert(pos.symbol.clone());
        }
    }
    for pos in &snapshot.open_positions {
        if pos.qty > Decimal::ZERO {
            held.insert(pos.symbol.clone());
        }
    }
    if let Some(st) = state {
        if let Some(pos) = &st.position {
            if pos.qty > Decimal::ZERO {
                held.insert(pos.symbol.clone());
            }
        }
        for pos in &st.positions {
            if pos.qty > Decimal::ZERO {
                held.insert(pos.symbol.clone());
            }
        }
    }
    held
}

fn position_for<'a>(
    snapshot: &'a MarketSnapshot,
    state: Option<&'a EngineState>,
    symbol: &str,
) -> Option<&'a Position> {
    if !symbol.is_empty() {
        if let Some(st) = state {
            if let Some(pos) = &st.position {
                if pos.symbol == symbol {
                    return Some(pos);
                }
            }
            if let Some(pos) = st.positions.iter().find(|p| p.symbol == symbol) {
                return Some(pos);
            }
        }
        if let Some(pos) = &snapshot.position {
            if pos.symbol == symbol {
                return Some(pos);
            }
        }
        return snapshot.open_positions.iter().find(|p| p.symbol == symbol);
    }
    snapshot
        .position
        .as_ref()
        .or_else(|| state.and_then(|s| s.position.as_ref()))
}

fn snapshot_row<'a>(snapshot: &'a MarketSnapshot, symbol: &str) -> Option<&'a Position> {
    let want = symbol.to_ascii_uppercase();
    let mut rows: Vec<&Position> = snapshot.open_positions.iter().collect();
    if let Some(p) = &snapshot.position {
        rows.push(p);
    }
    for pos in rows {
        if pos.qty <= Decimal::ZERO {
            continue;
        }
        if want.is_empty() || pos.symbol == want {
            return Some(pos);
        }
    }
    None
}

fn snapshot_long<'a>(snapshot: &'a MarketSnapshot, symbol: &str) -> Option<&'a Position> {
    if let Some(row) = snapshot_row(snapshot, symbol) {
        if row.side == Side::Long {
            return Some(row);
        }
    }
    if !symbol.is_empty() {
        return snapshot
            .open_positions
            .iter()
            .find(|p| p.symbol == symbol.to_ascii_uppercase() && p.side == Side::Long && p.qty > Decimal::ZERO);
    }
    None
}

fn fetch_book(client: &mut dyn LiveClient) -> Option<Vec<Position>> {
    let raw = client.position_risk().ok()?;
    crate::exchange::parse_positions(&raw).ok()
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn push_recent(state: &mut EngineState, text: String) {
    state.recent_actions.push(text);
    if state.recent_actions.len() > 8 {
        let n = state.recent_actions.len();
        state.recent_actions = state.recent_actions[n - 8..].to_vec();
    }
}

fn symbol_hint(snapshot: &MarketSnapshot, state: Option<&EngineState>) -> String {
    if let Some(pos) = &snapshot.position {
        if pos.qty > Decimal::ZERO {
            return pos.symbol.clone();
        }
    }
    if let Some(st) = state {
        if let Some(pos) = &st.position {
            if pos.qty > Decimal::ZERO {
                return pos.symbol.clone();
            }
        }
    }
    snapshot
        .open_positions
        .iter()
        .find(|p| p.qty > Decimal::ZERO)
        .map(|p| p.symbol.clone())
        .unwrap_or_default()
}

fn desk_pause_after_loss(cfg: &Config, state: &mut EngineState, now: f64) {
    let windows = if state.strategy_id == 4 {
        cfg.s4_entry_windows.as_slice()
    } else {
        cfg.entry_windows.as_slice()
    };
    let until = pause_until_after_loss(now, windows, COOLDOWN_SEC);
    if until > state.cooldown_until {
        state.cooldown_until = until;
    }
}

fn mark_for_symbol(snapshot: &MarketSnapshot, symbol: &str) -> Option<Decimal> {
    for ticker in &snapshot.tickers {
        if ticker.symbol == symbol && ticker.last_price > Decimal::ZERO {
            return Some(ticker.last_price);
        }
    }
    snapshot.bars.last().and_then(|b| {
        if b.close > Decimal::ZERO {
            Some(b.close)
        } else {
            None
        }
    })
}

fn flatten_live_short(client: &mut dyn LiveClient, symbol: &str) -> Option<Position> {
    let want = symbol.to_ascii_uppercase();
    if want.is_empty() {
        return None;
    }
    let book = fetch_book(client)?;
    let row = book.iter().find(|p| p.symbol == want && p.qty > Decimal::ZERO)?.clone();
    if row.side != Side::Short {
        return None;
    }
    let _ = close_targets(client, std::slice::from_ref(&row));
    Some(row)
}

fn place_fill_protectives(
    client: &mut dyn LiveClient,
    symbol: &str,
    take_profit: Decimal,
    stop_loss: Decimal,
    qty: Decimal,
) -> Result<(), ExchangeError> {
    match client.place_tp_sl(symbol, take_profit, stop_loss, Some(qty)) {
        Ok(()) => Ok(()),
        Err(exc) => {
            let info = classify(&exc.0);
            if info.code == Some(-4130) {
                return Ok(());
            }
            if info.code != Some(-2022) {
                return Err(exc);
            }
            let first = exc;
            let book = fetch_book(client);
            let Some(book) = book else {
                return Err(first);
            };
            let row = book.iter().find(|p| p.symbol == symbol.to_ascii_uppercase() && p.qty > Decimal::ZERO);
            if row.map(|r| r.side) != Some(Side::Long) {
                return Err(first);
            }
            match client.place_tp_sl(symbol, take_profit, stop_loss, Some(qty)) {
                Ok(()) => Ok(()),
                Err(retry) => {
                    if classify(&retry.0).code == Some(-4130) {
                        Ok(())
                    } else {
                        Err(retry)
                    }
                }
            }
        }
    }
}

fn leverage_for(cfg: &Config, snapshot: &MarketSnapshot, symbol: &str) -> i32 {
    if let Some(l) = cfg.leverage {
        return l;
    }
    for pos in &snapshot.open_positions {
        if pos.symbol == symbol && pos.leverage > 0 {
            return pos.leverage;
        }
    }
    if let Some(pos) = &snapshot.position {
        if pos.symbol == symbol && pos.leverage > 0 {
            return pos.leverage;
        }
    }
    20
}

fn existing_notional(snapshot: &MarketSnapshot, symbol: &str, mark: Decimal) -> Decimal {
    let mut total = Decimal::ZERO;
    let mut rows = snapshot.open_positions.clone();
    if let Some(p) = &snapshot.position {
        rows.push(p.clone());
    }
    let mut seen = std::collections::HashSet::new();
    for pos in rows {
        if pos.symbol != symbol || pos.qty <= Decimal::ZERO {
            continue;
        }
        let key = format!("{}:{}", pos.symbol, pos.side);
        if !seen.insert(key) {
            continue;
        }
        total += pos.qty * mark;
    }
    total
}

fn notional_fits(
    client: &mut dyn LiveClient,
    snapshot: &MarketSnapshot,
    symbol: &str,
    mark: Decimal,
    order_notional: Decimal,
    leverage: i32,
) -> bool {
    match client.max_notional(symbol, leverage) {
        Ok(Some(cap)) => existing_notional(snapshot, symbol, mark) + order_notional <= cap,
        _ => true,
    }
}

pub fn apply_live(
    cfg: &Config,
    client: &mut dyn LiveClient,
    snapshot: &MarketSnapshot,
    decision: &Decision,
    state: Option<&EngineState>,
) -> LiveApplyResult {
    if !cfg.live {
        return LiveApplyResult::default();
    }
    if cfg.credentials.is_none() {
        return LiveApplyResult {
            error: Some("live refused: no credentials".into()),
            ..Default::default()
        };
    }
    match decision {
        Decision::EnterLong {
            symbol,
            take_profit,
            stop_loss,
            ..
        } => {
            if skip_symbols(state).contains(&symbol.to_ascii_uppercase()) {
                return LiveApplyResult::default();
            }
            if state.map(|s| s.daily_halt).unwrap_or(false) {
                return LiveApplyResult::default();
            }
            let held = held_symbols(snapshot, state);
            if held.contains(symbol) {
                return LiveApplyResult {
                    error: Some("skip enter: already in position".into()),
                    ..Default::default()
                };
            }
            if held.len() as i32 >= cfg.max_positions {
                return LiveApplyResult {
                    error: Some("skip enter: book full".into()),
                    ..Default::default()
                };
            }
            let filters = match client.filters_for(symbol) {
                Ok(f) => f,
                Err(e) => {
                    return LiveApplyResult {
                        error: Some(e.0),
                        ..Default::default()
                    };
                }
            };
            let Some(mark) = snapshot
                .tickers
                .iter()
                .find(|t| t.symbol == *symbol)
                .map(|t| t.last_price)
            else {
                return LiveApplyResult {
                    error: Some("skip enter: no mark".into()),
                    ..Default::default()
                };
            };
            let notional = if cfg.notional_from_exchange {
                filters.min_notional
            } else {
                cfg.order_notional
            };
            let qty = match size_market_order(notional, mark, &filters) {
                Ok(q) => q,
                Err(e) => {
                    return LiveApplyResult {
                        error: Some(e.0),
                        ..Default::default()
                    };
                }
            };
            let leverage = leverage_for(cfg, snapshot, symbol);
            if !notional_fits(client, snapshot, symbol, mark, qty * mark, leverage) {
                return LiveApplyResult {
                    error: Some("leverage cap (-2027): notional exceeds bracket".into()),
                    ..Default::default()
                };
            }
            if let Some(lev) = cfg.leverage {
                if let Err(e) = client.set_leverage(symbol, lev) {
                    return LiveApplyResult {
                        error: Some(e.0),
                        ..Default::default()
                    };
                }
            }
            if let Err(e) = client.market_buy(symbol, qty) {
                return LiveApplyResult {
                    error: Some(e.0),
                    ..Default::default()
                };
            }
            if let Some(flipped) = flatten_live_short(client, symbol) {
                return LiveApplyResult {
                    error: Some(format!("вход перевернул в шорт — закрыл {}", flipped.symbol)),
                    forget_symbol: flipped.symbol,
                    ..Default::default()
                };
            }
            match place_fill_protectives(client, symbol, *take_profit, *stop_loss, qty) {
                Ok(()) => LiveApplyResult {
                    filled: true,
                    mark: Some(mark),
                    qty: Some(qty),
                    ..Default::default()
                },
                Err(exc) => {
                    if classify(&exc.0).code == Some(-4130) {
                        LiveApplyResult {
                            filled: true,
                            mark: Some(mark),
                            qty: Some(qty),
                            ..Default::default()
                        }
                    } else {
                        LiveApplyResult {
                            error: Some(format!(
                                "filled but TP/SL failed (fail-closed on further entries): {exc}"
                            )),
                            filled: true,
                            mark: Some(mark),
                            qty: Some(qty),
                            ..Default::default()
                        }
                    }
                }
            }
        }
        Decision::AmendStop {
            stop_loss,
            symbol,
            ..
        } => {
            if snapshot.live_book && snapshot_long(snapshot, symbol).is_none() {
                let hint = if symbol.is_empty() {
                    symbol_hint(snapshot, state)
                } else {
                    symbol.clone()
                };
                if let Some(flipped) = flatten_live_short(client, &hint) {
                    return LiveApplyResult {
                        error: Some(format!("трейл перевернул в шорт — закрыл {}", flipped.symbol)),
                        forget_symbol: flipped.symbol,
                        ..Default::default()
                    };
                }
                return LiveApplyResult {
                    error: Some("skip amend: нет живого лонга".into()),
                    forget_symbol: hint,
                    ..Default::default()
                };
            }
            let Some(pos) = position_for(snapshot, state, symbol).cloned() else {
                return LiveApplyResult {
                    error: Some("skip amend: no position".into()),
                    ..Default::default()
                };
            };
            if pos.side != Side::Long {
                if let Some(flipped) = flatten_live_short(client, &pos.symbol) {
                    return LiveApplyResult {
                        error: Some(format!("трейл перевернул в шорт — закрыл {}", flipped.symbol)),
                        forget_symbol: flipped.symbol,
                        ..Default::default()
                    };
                }
                return LiveApplyResult {
                    error: Some("skip amend: нет живого лонга".into()),
                    forget_symbol: pos.symbol,
                    ..Default::default()
                };
            }
            let tp = if let Some(tp) = pos.take_profit {
                tp
            } else {
                if pos.entry_price <= Decimal::ZERO {
                    return LiveApplyResult {
                        error: Some("skip amend: missing take profit and entry".into()),
                        ..Default::default()
                    };
                }
                match take_profit_price_net(pos.entry_price, "LONG", cfg.tp_pct) {
                    Ok(v) => v,
                    Err(e) => {
                        return LiveApplyResult {
                            error: Some(e),
                            ..Default::default()
                        };
                    }
                }
            };
            if let Err(e) = client.replace_stop(&pos.symbol, *stop_loss, Some(tp), Some(pos.qty)) {
                return LiveApplyResult {
                    error: Some(e.0),
                    ..Default::default()
                };
            }
            if let Some(flipped) = flatten_live_short(client, &pos.symbol) {
                return LiveApplyResult {
                    error: Some(format!("трейл перевернул в шорт — закрыл {}", flipped.symbol)),
                    forget_symbol: flipped.symbol,
                    ..Default::default()
                };
            }
            LiveApplyResult::default()
        }
        Decision::ExitPosition { symbol, .. } => {
            let mut pos = position_for(snapshot, state, symbol).cloned();
            if snapshot.live_book {
                let hint = pos.as_ref().map(|p| p.symbol.clone()).unwrap_or_else(|| symbol.clone());
                let live = snapshot_row(snapshot, &hint);
                if live.is_none() {
                    return LiveApplyResult {
                        forget_symbol: hint,
                        ..Default::default()
                    };
                }
                pos = live.cloned();
            }
            let Some(pos) = pos else {
                return LiveApplyResult::default();
            };
            let _ = client.cancel_protectives(&pos.symbol);
            if let Err(e) = client.market_close(&pos.symbol, pos.side.as_str(), pos.qty) {
                return LiveApplyResult {
                    error: Some(e.0),
                    ..Default::default()
                };
            }
            let _ = client.cancel_protectives(&pos.symbol);
            let exit_px = mark_for_symbol(snapshot, &pos.symbol).unwrap_or(pos.entry_price);
            LiveApplyResult {
                filled: true,
                mark: Some(exit_px),
                qty: Some(pos.qty),
                forget_symbol: pos.symbol.clone(),
                ..Default::default()
            }
        }
        Decision::Hold { .. } => LiveApplyResult::default(),
    }
}

pub fn adopt_live_fill(state: &mut EngineState, decision: &Decision, mark: Decimal, qty: Decimal) {
    let Decision::EnterLong {
        symbol,
        stop_loss,
        take_profit,
        ..
    } = decision
    else {
        return;
    };
    let filled = Position {
        symbol: symbol.clone(),
        side: Side::Long,
        qty,
        entry_price: mark,
        stop_loss: Some(*stop_loss),
        take_profit: Some(*take_profit),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: None,
        leverage: 0,
    };
    state.entry_inflight = true;
    state.positions.retain(|p| p.symbol != filled.symbol);
    state.positions.push(filled.clone());
    state.position = state.positions.first().cloned();
    if !state.inflight_symbols.contains(symbol) {
        state.inflight_symbols.push(symbol.clone());
    }
}

pub fn record_flatten(state: &mut EngineState, result: FlattenResult, pause_entries: bool) -> FlattenResult {
    if !result.closed.is_empty() {
        let closed_syms: std::collections::HashSet<String> = result.symbols().into_iter().collect();
        if pause_entries {
            state.position = None;
            state.positions.clear();
            state.entry_inflight = false;
            state.inflight_symbols.clear();
            state.entries_paused = true;
        } else {
            state.positions.retain(|p| !closed_syms.contains(&p.symbol));
            state.position = state.positions.first().cloned();
        }
        let prefix = if pause_entries { "FLAT " } else { "FLAT хвосты " };
        state.recent_actions.push(format!("{prefix}{}", result.closed.join(", ")));
        if state.recent_actions.len() > 8 {
            let n = state.recent_actions.len();
            state.recent_actions = state.recent_actions[n - 8..].to_vec();
        }
    }
    if !result.errors.is_empty() {
        state.last_error = result.error();
    } else if !result.closed.is_empty() {
        if pause_entries {
            state.last_error = None;
        }
    } else {
        state.last_error = Some("нечего закрывать".into());
    }
    result
}

pub fn apply_flatten(
    cfg: &Config,
    client: &mut dyn LiveClient,
    state: &mut EngineState,
    _snapshot: Option<&MarketSnapshot>,
    targets: Option<&[Position]>,
) -> FlattenResult {
    let (result, pause) = if !cfg.live {
        (
            FlattenResult {
                closed: Vec::new(),
                errors: vec!["flatten refused: not live".into()],
            },
            true,
        )
    } else if cfg.credentials.is_none() {
        (
            FlattenResult {
                closed: Vec::new(),
                errors: vec!["flatten refused: no credentials".into()],
            },
            true,
        )
    } else if let Some(t) = targets {
        (
            if t.is_empty() {
                FlattenResult::default()
            } else {
                close_targets(client, t)
            },
            false,
        )
    } else {
        (flatten_open_book(client), true)
    };
    let out = record_flatten(state, result, pause);
    emit_flatten(&out);
    out
}

pub fn apply_decision(
    cfg: &Config,
    client: &mut dyn LiveClient,
    state: &mut EngineState,
    snapshot: &MarketSnapshot,
    decision: &Decision,
) -> LiveApplyResult {
    let closing = if let Decision::ExitPosition { symbol, reason } = decision {
        position_for(snapshot, Some(state), symbol).cloned().map(|p| (p, reason.clone()))
    } else {
        None
    };
    let result = apply_live(cfg, client, snapshot, decision, Some(state));
    if let Some((pos, reason)) = closing {
        if result.error.is_none() {
            let exit_px = result
                .mark
                .or_else(|| mark_for_symbol(snapshot, &pos.symbol))
                .unwrap_or(pos.entry_price);
            journal::record_close(
                state.strategy_id,
                &pos.symbol,
                pos.qty,
                pos.entry_price,
                exit_px,
                &reason,
                cfg.live,
            );
            if !journal::long_close_was_win(pos.entry_price, exit_px, pos.take_profit) {
                desk_pause_after_loss(cfg, state, unix_now());
            }
            let key = pos.symbol.to_ascii_uppercase();
            let until = unix_now() + COOLDOWN_SEC;
            let cur = state.cooldowns.get(&key).copied().unwrap_or(0.0);
            state.cooldowns.insert(key, cur.max(until));
        }
    }
    if !result.forget_symbol.is_empty() {
        drop_symbol(state, &result.forget_symbol);
        if result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("шорт"))
        {
            journal::record_flatten(
                state.strategy_id,
                &[format!("SHORT {}", result.forget_symbol)],
                cfg.live,
                "закрыл чужой шорт",
            );
        }
    }
    if let Some(err) = &result.error {
        let info = classify(err);
        if info.action == ACTION_IGNORE {
            state.last_error = None;
        } else {
            state.last_error = Some(err.clone());
            if info.action == ACTION_OPERATOR {
                state.entries_paused = true;
            } else if let Decision::EnterLong { symbol, .. } = decision {
                if info.action == ACTION_SKIP || info.action == ACTION_COOLDOWN {
                    let up = symbol.to_ascii_uppercase();
                    if !state.skip_symbols.iter().any(|s| s.eq_ignore_ascii_case(&up)) {
                        state.skip_symbols.push(up.clone());
                        state.skip_symbols.sort();
                    }
                    state.skip_reasons.insert(
                        up,
                        info.code.map(|c| c.to_string()).unwrap_or_else(|| info.message.clone()),
                    );
                }
            }
        }
    } else if !matches!(decision, Decision::Hold { .. }) {
        state.last_error = None;
    }
    if result.filled {
        if let Decision::EnterLong { .. } = decision {
            if let (Some(mark), Some(qty)) = (result.mark, result.qty) {
                adopt_live_fill(state, decision, mark, qty);
                if result.error.is_none() {
                    if let Decision::EnterLong { symbol, .. } = decision {
                        state.sized_stops.insert(symbol.to_ascii_uppercase());
                    }
                }
            } else {
                state.entry_inflight = true;
            }
        }
    }
    let has_position = snapshot.position.is_some()
        || snapshot.open_positions.iter().any(|p| p.qty > Decimal::ZERO);
    emit_decision(decision, &result, cfg.live, has_position);
    result
}

fn drop_symbol(state: &mut EngineState, symbol: &str) {
    let want = symbol.to_ascii_uppercase();
    state.positions.retain(|p| p.symbol.to_ascii_uppercase() != want);
    state.position = state.positions.first().cloned();
    state.inflight_symbols.retain(|s| s.to_ascii_uppercase() != want);
    state.entry_inflight = !state.inflight_symbols.is_empty() && state.positions.is_empty();
    state.sized_stops.remove(&want);
}

pub fn rearm_live_protectives(
    cfg: &Config,
    client: &mut dyn LiveClient,
    state: &mut EngineState,
    snapshot: &MarketSnapshot,
) -> Vec<String> {
    if !cfg.live || cfg.credentials.is_none() || !snapshot.live_book {
        return Vec::new();
    }
    let mut done = Vec::new();
    let longs: Vec<Position> = snapshot
        .open_positions
        .iter()
        .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
        .cloned()
        .collect();
    for live in longs {
        let (Some(sl), Some(tp)) = (live.stop_loss, live.take_profit) else {
            continue;
        };
        let key = live.symbol.to_ascii_uppercase();
        if state.sized_stops.contains(&key) {
            continue;
        }
        let rows = client.open_algo_orders(Some(&key)).ok();
        if let Some(rows) = rows {
            if sell_protectives_are_sized(&rows) {
                state.sized_stops.insert(key);
                continue;
            }
        }
        if let Err(exc) = client.replace_stop(&live.symbol, sl, Some(tp), Some(live.qty)) {
            if classify(&exc.0).code == Some(-4130) {
                state.sized_stops.insert(key);
                continue;
            }
            state.last_error = Some(exc.0);
            continue;
        }
        if let Some(flipped) = flatten_live_short(client, &live.symbol) {
            journal::record_flatten(
                state.strategy_id,
                &[format!("SHORT {}", flipped.symbol)],
                cfg.live,
                "закрыл чужой шорт",
            );
            drop_symbol(state, &flipped.symbol);
            continue;
        }
        state.sized_stops.insert(key);
        done.push(live.symbol.clone());
        push_recent(state, format!("TP/SL на размер лонга {}", live.symbol));
    }
    done
}

fn live_long_keys(snapshot: &MarketSnapshot) -> HashSet<String> {
    let mut live_longs = HashSet::new();
    for pos in &snapshot.open_positions {
        if pos.side == Side::Long && pos.qty > Decimal::ZERO {
            live_longs.insert(pos.symbol.to_ascii_uppercase());
        }
    }
    if let Some(pos) = &snapshot.position {
        if pos.side == Side::Long && pos.qty > Decimal::ZERO {
            live_longs.insert(pos.symbol.to_ascii_uppercase());
        }
    }
    live_longs
}

fn order_symbol(row: &Value) -> Option<String> {
    let symbol = row.get("symbol")?.as_str()?.trim();
    if symbol.is_empty() {
        None
    } else {
        Some(symbol.to_ascii_uppercase())
    }
}

fn collect_order_symbols(rows: &[Value]) -> BTreeSet<String> {
    rows.iter().filter_map(order_symbol).collect()
}

fn probe_orphan_symbols(snapshot: &MarketSnapshot, state: &EngineState) -> BTreeSet<String> {
    let mut symbols = BTreeSet::new();
    for pos in snapshot.open_positions.iter().chain(state.positions.iter()) {
        if !pos.symbol.is_empty() {
            symbols.insert(pos.symbol.to_ascii_uppercase());
        }
    }
    if let Some(pos) = &snapshot.position {
        if !pos.symbol.is_empty() {
            symbols.insert(pos.symbol.to_ascii_uppercase());
        }
    }
    if let Some(pos) = &state.position {
        if !pos.symbol.is_empty() {
            symbols.insert(pos.symbol.to_ascii_uppercase());
        }
    }
    if !snapshot.chart_symbol.is_empty() {
        symbols.insert(snapshot.chart_symbol.to_ascii_uppercase());
    }
    for extra in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        symbols.insert(extra.to_string());
    }
    symbols
}

/// When the exchange closed our long, kill leftover TP/SL before they open a short.
pub fn clear_vanished_longs(
    cfg: &Config,
    client: &mut dyn LiveClient,
    state: &mut EngineState,
    snapshot: &MarketSnapshot,
    now: f64,
) -> Vec<String> {
    if !cfg.live || cfg.credentials.is_none() || !snapshot.live_book {
        return Vec::new();
    }
    let live_longs = live_long_keys(snapshot);
    let mut remembered = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut rows = state.positions.clone();
    if let Some(pos) = &state.position {
        rows.push(pos.clone());
    }
    for pos in rows {
        if pos.side != Side::Long || pos.qty <= Decimal::ZERO {
            continue;
        }
        let key = pos.symbol.to_ascii_uppercase();
        if seen.contains(&key) || live_longs.contains(&key) {
            continue;
        }
        seen.insert(key);
        remembered.push(pos);
    }
    if remembered.is_empty() {
        return Vec::new();
    }
    let until = now + COOLDOWN_SEC;
    let mut cleared = Vec::new();
    for pos in remembered {
        let _ = client.cancel_protectives(&pos.symbol);
        let exit_px = mark_for_symbol(snapshot, &pos.symbol).unwrap_or(pos.entry_price);
        journal::record_close(
            state.strategy_id,
            &pos.symbol,
            pos.qty,
            pos.entry_price,
            exit_px,
            "биржа закрыла лонг",
            cfg.live,
        );
        let key = pos.symbol.to_ascii_uppercase();
        let cur = state.cooldowns.get(&key).copied().unwrap_or(0.0);
        state.cooldowns.insert(key, cur.max(until));
        if !journal::long_close_was_win(pos.entry_price, exit_px, pos.take_profit) {
            desk_pause_after_loss(cfg, state, now);
        }
        push_recent(state, format!("снял TP/SL после закрытия {}", pos.symbol));
        cleared.push(pos.symbol.clone());
        drop_symbol(state, &pos.symbol);
    }
    cleared
}

/// Leftover TP/SL on a flat (or short) symbol — including after restart, when
/// state no longer remembers the vanished long.
pub fn clear_orphan_protectives(
    cfg: &Config,
    client: &mut dyn LiveClient,
    state: &mut EngineState,
    snapshot: &MarketSnapshot,
) -> Vec<String> {
    if !cfg.live || cfg.credentials.is_none() || !snapshot.live_book {
        return Vec::new();
    }
    let live_longs = live_long_keys(snapshot);
    let mut leftover = BTreeSet::new();
    if let Ok(rows) = client.open_algo_orders(None) {
        leftover.extend(collect_order_symbols(&rows));
    }
    if let Ok(rows) = client.open_orders(None) {
        leftover.extend(collect_order_symbols(&rows));
    }
    for symbol in probe_orphan_symbols(snapshot, state) {
        if live_longs.contains(&symbol) || leftover.contains(&symbol) {
            continue;
        }
        let algos = client.open_algo_orders(Some(&symbol)).ok().unwrap_or_default();
        let orders = client.open_orders(Some(&symbol)).ok().unwrap_or_default();
        if !algos.is_empty() || !orders.is_empty() {
            leftover.insert(symbol);
        }
    }
    leftover.retain(|symbol| !live_longs.contains(symbol));
    if leftover.is_empty() {
        return Vec::new();
    }
    let mut cleared = Vec::new();
    for symbol in leftover {
        let _ = client.cancel_protectives(&symbol);
        push_recent(state, format!("снял сиротский стоп: {symbol}"));
        cleared.push(symbol);
    }
    cleared
}

/// Long-only desk: close leftover shorts without waiting for x x.
pub fn sweep_rogue_shorts(
    cfg: &Config,
    client: &mut dyn LiveClient,
    state: &mut EngineState,
    snapshot: &MarketSnapshot,
    now: Option<f64>,
) -> FlattenResult {
    if !cfg.live || cfg.credentials.is_none() || !snapshot.live_book {
        return FlattenResult::default();
    }
    let mut shorts: Vec<Position> = snapshot
        .open_positions
        .iter()
        .filter(|p| p.side == Side::Short && p.qty > Decimal::ZERO)
        .cloned()
        .collect();
    if let Some(pos) = &snapshot.position {
        if pos.side == Side::Short
            && pos.qty > Decimal::ZERO
            && !shorts.iter().any(|p| p.symbol == pos.symbol)
        {
            shorts.push(pos.clone());
        }
    }
    if shorts.is_empty() {
        return FlattenResult::default();
    }
    let result = close_targets(client, &shorts);
    if !result.closed.is_empty() {
        let ts = now.unwrap_or_else(unix_now);
        for label in &result.closed {
            let symbol = label.rsplit(' ').next().unwrap_or(label);
            let until = ts + COOLDOWN_SEC;
            let key = symbol.to_ascii_uppercase();
            let cur = state.cooldowns.get(&key).copied().unwrap_or(0.0);
            state.cooldowns.insert(key, cur.max(until));
            drop_symbol(state, symbol);
        }
        push_recent(
            state,
            format!("закрыл чужой шорт: {}", result.closed.join(", ")),
        );
        if result.errors.is_empty() {
            state.last_error = None;
        }
        journal::record_flatten(
            state.strategy_id,
            &result.closed,
            cfg.live,
            "закрыл чужой шорт",
        );
        emit_flatten(&result);
    }
    if !result.errors.is_empty() {
        state.last_error = result.error();
    }
    result
}

/// Vanished longs, leftover shorts, then size TP/SL. TUI calls this once per tick.
pub fn reconcile_live(
    cfg: &Config,
    client: &mut dyn LiveClient,
    state: &mut EngineState,
    snapshot: &MarketSnapshot,
    now: Option<f64>,
) -> ReconcileResult {
    if !cfg.live || cfg.credentials.is_none() || !snapshot.live_book {
        return ReconcileResult::default();
    }
    let now_ts = now.unwrap_or_else(unix_now);
    clear_vanished_longs(cfg, client, state, snapshot, now_ts);
    let orphans = clear_orphan_protectives(cfg, client, state, snapshot);
    let swept = sweep_rogue_shorts(cfg, client, state, snapshot, now);
    if !swept.closed.is_empty() {
        return ReconcileResult {
            skip_tick: true,
            last_text: format!("закрыл чужой шорт: {}", swept.closed.join(", ")),
        };
    }
    if !orphans.is_empty() {
        return ReconcileResult {
            skip_tick: true,
            last_text: format!("снял сиротский стоп: {}", orphans.join(", ")),
        };
    }
    let rearmed = rearm_live_protectives(cfg, client, state, snapshot);
    if !rearmed.is_empty() {
        return ReconcileResult {
            skip_tick: false,
            last_text: format!("TP/SL на размер лонга: {}", rearmed.join(", ")),
        };
    }
    ReconcileResult::default()
}

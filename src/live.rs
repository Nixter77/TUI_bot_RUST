//! Live TestNet side-effects: enter / trail / exit / panic flatten.

use crate::config::Config;
use crate::errors::{
    classify, retry_backoff_sec, ACTION_COOLDOWN, ACTION_IGNORE, ACTION_OPERATOR, ACTION_RETRY,
    ACTION_SKIP, COOLDOWN_SEC,
};
use crate::exchange::{
    size_risk_market_order, sell_protectives_are_sized, size_market_order, ExchangeError, LiveClient,
};
use crate::flatten::{close_targets, flatten_open_book, FlattenResult};
use crate::journal;
use crate::models::{Decision, EngineState, MarketSnapshot, Position, Side};
use crate::sessions::{pause_until_after_loss, unix_now};
use crate::signals::{emit_decision, emit_flatten, reason_suggests_win};
use crate::trail::{candidate_stop, take_profit_price_net};
use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};

/// Missing/failed protective rearm wall-time budget before fail-closed flatten.
pub const REARM_FAIL_BUDGET_SEC: f64 = 90.0;
/// Consecutive failed rearm attempts before fail-closed flatten.
pub const REARM_FAIL_MAX: u8 = 3;

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

fn fetch_book(client: &mut dyn LiveClient) -> Result<Vec<Position>, crate::exchange::ExchangeError> {
    let raw = client.position_risk()?;
    crate::exchange::parse_positions(&raw)
}

fn push_recent(state: &mut EngineState, text: String) {
    state.push_action(unix_now(), text);
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

fn arm_entry_pause(state: &mut EngineState, now: f64) {
    state.entries_paused = true;
    let until = now + COOLDOWN_SEC;
    if until > state.cooldown_until {
        state.cooldown_until = until;
    }
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

fn cool_symbol(state: &mut EngineState, symbol: &str, now: f64, won: bool) {
    let until = journal::symbol_cooldown_until(now, won, COOLDOWN_SEC);
    if until <= now {
        return;
    }
    let key = symbol.to_ascii_uppercase();
    let cur = state.cooldowns.get(&key).copied().unwrap_or(0.0);
    state.cooldowns.insert(key, cur.max(until));
}

fn exit_sound_won(
    pos: &Position,
    reason: &str,
    result: &LiveApplyResult,
    snapshot: &MarketSnapshot,
) -> bool {
    if reason_suggests_win(reason) {
        return true;
    }
    if pos.unrealized_pnl > Decimal::ZERO {
        return true;
    }
    if let Some(row) = snapshot_row(snapshot, &pos.symbol) {
        if row.unrealized_pnl > Decimal::ZERO {
            return true;
        }
    }
    let exit_px = result
        .mark
        .or_else(|| mark_for_symbol(snapshot, &pos.symbol))
        .unwrap_or(pos.entry_price);
    if pos.side == Side::Long && exit_px > pos.entry_price {
        return true;
    }
    journal::long_close_was_win(pos.entry_price, exit_px, pos.take_profit)
}

fn flatten_sound_won(closed: &[String], positions: &[Position]) -> Option<bool> {
    if closed.is_empty() {
        return None;
    }
    let mut all_green = true;
    for label in closed {
        let symbol = label.rsplit(' ').next().unwrap_or(label);
        let Some(pos) = positions
            .iter()
            .find(|p| p.symbol.eq_ignore_ascii_case(symbol))
        else {
            return None;
        };
        if pos.unrealized_pnl <= Decimal::ZERO {
            all_green = false;
        }
    }
    Some(all_green)
}

fn positions_for_flatten(
    snapshot: Option<&MarketSnapshot>,
    state: &EngineState,
    targets: Option<&[Position]>,
) -> Vec<Position> {
    if let Some(t) = targets {
        return t.to_vec();
    }
    let mut out: Vec<Position> = Vec::new();
    if let Some(s) = snapshot {
        for p in &s.open_positions {
            if p.qty > Decimal::ZERO {
                out.push(p.clone());
            }
        }
        if let Some(p) = &s.position {
            if p.qty > Decimal::ZERO
                && !out.iter().any(|x| x.symbol == p.symbol && x.side == p.side)
            {
                out.push(p.clone());
            }
        }
    }
    for p in &state.positions {
        if p.qty > Decimal::ZERO
            && !out.iter().any(|x| x.symbol == p.symbol && x.side == p.side)
        {
            out.push(p.clone());
        }
    }
    out
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
    let book = fetch_book(client).ok()?;
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
            let book = match fetch_book(client) {
                Ok(b) => b,
                Err(_) => return Err(first),
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

fn err(msg: impl Into<String>) -> LiveApplyResult {
    LiveApplyResult {
        error: Some(msg.into()),
        ..Default::default()
    }
}

fn enter_live(
    cfg: &Config,
    client: &mut dyn LiveClient,
    snapshot: &MarketSnapshot,
    state: Option<&EngineState>,
    symbol: &str,
    take_profit: Decimal,
    stop_loss: Decimal,
) -> LiveApplyResult {
    if skip_symbols(state).contains(&symbol.to_ascii_uppercase()) {
        return LiveApplyResult::default();
    }
    if state.map(|s| s.daily_halt).unwrap_or(false) {
        return LiveApplyResult::default();
    }
    let mut held = held_symbols(snapshot, state);
    // Fail-closed: a 502 here used to look like a flat book and double-buy.
    let book = match fetch_book(client) {
        Ok(b) => b,
        Err(e) => return err(format!("skip enter: нет снимка позиций ({e})")),
    };
    for pos in &book {
        if pos.qty > Decimal::ZERO && pos.side == Side::Long {
            held.insert(pos.symbol.clone());
        }
    }
    let want = symbol.to_ascii_uppercase();
    if held.iter().any(|s| s.eq_ignore_ascii_case(&want)) {
        return err("skip enter: already in position");
    }
    let cap = if state.map(|s| s.strategy_id == 4).unwrap_or(false) {
        cfg.s4_max_positions
    } else {
        cfg.max_positions
    };
    if held.iter().filter(|s| !s.is_empty()).count() as i32 >= cap {
        return err("skip enter: book full");
    }
    let filters = match client.filters_for(symbol) {
        Ok(f) => f,
        Err(e) => return err(e.0),
    };
    let Some(mark) = snapshot
        .tickers
        .iter()
        .find(|t| t.symbol == *symbol)
        .map(|t| t.last_price)
    else {
        return err("skip enter: no mark");
    };
    // S4 live path: RISK_PCT of account equity (wallet+uPnL). 0 = fall back to ORDER_NOTIONAL_USDT.
    // Never bump qty so qty*(entry-SL) exceeds the risk budget — skip the symbol instead.
    let s4_risk = state.map(|s| s.strategy_id == 4).unwrap_or(false) && cfg.risk_pct > Decimal::ZERO;
    let mut qty = if s4_risk {
        match size_risk_market_order(
            snapshot.account.equity(),
            cfg.risk_pct,
            mark,
            stop_loss,
            &filters,
        ) {
            Ok(Some(q)) => q,
            Ok(None) => return err("skip enter: minNotional inflates risk"),
            Err(e) => return err(e.0),
        }
    } else {
        let notional = if cfg.notional_from_exchange {
            filters.min_notional
        } else {
            cfg.order_notional
        };
        match size_market_order(notional, mark, &filters) {
            Ok(q) => q,
            Err(e) => return err(e.0),
        }
    };
    let leverage = leverage_for(cfg, snapshot, symbol);
    if !notional_fits(client, snapshot, symbol, mark, qty * mark, leverage) {
        return err("leverage cap (-2027): notional exceeds bracket");
    }
    if let Some(lev) = cfg.leverage {
        if let Err(e) = client.set_leverage(symbol, lev) {
            return err(e.0);
        }
    }
    if let Err(e) = client.market_buy(symbol, qty) {
        match fetch_book(client) {
            Ok(book) => {
                let Some(row) = book.iter().find(|p| {
                    p.symbol.eq_ignore_ascii_case(symbol)
                        && p.side == Side::Long
                        && p.qty > Decimal::ZERO
                }) else {
                    return err(e.0);
                };
                qty = row.qty;
            }
            Err(_) => return err(e.0),
        }
    }
    if let Some(flipped) = flatten_live_short(client, symbol) {
        return LiveApplyResult {
            error: Some(format!("вход перевернул в шорт — закрыл {}", flipped.symbol)),
            forget_symbol: flipped.symbol,
            ..Default::default()
        };
    }
    match place_fill_protectives(client, symbol, take_profit, stop_loss, qty) {
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
                let _ = client.cancel_protectives(symbol);
                let close_note = match client.market_close(symbol, "LONG", qty) {
                    Ok(()) => String::new(),
                    Err(e) => format!("; close: {e}"),
                };
                if let Some(st) = state {
                    journal::record_flatten(
                        st.strategy_id,
                        &[symbol.to_string()],
                        cfg.live,
                        "flattened naked fill",
                    );
                }
                LiveApplyResult {
                    error: Some(format!("flattened naked fill: {exc}{close_note}")),
                    filled: false,
                    mark: Some(mark),
                    qty: Some(qty),
                    forget_symbol: symbol.to_string(),
                }
            }
        }
    }
}

fn amend_live(
    cfg: &Config,
    client: &mut dyn LiveClient,
    snapshot: &MarketSnapshot,
    state: Option<&EngineState>,
    stop_loss: Decimal,
    symbol: &str,
    reason: &str,
) -> LiveApplyResult {
    if snapshot.live_book && snapshot_long(snapshot, symbol).is_none() {
        let hint = if symbol.is_empty() {
            symbol_hint(snapshot, state)
        } else {
            symbol.to_string()
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
        return err("skip amend: no position");
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
            return err("skip amend: missing take profit and entry");
        }
        match take_profit_price_net(pos.entry_price, "LONG", cfg.tp_pct) {
            Ok(v) => v,
            Err(e) => return err(e),
        }
    };
    if let Err(e) = client.replace_stop(&pos.symbol, stop_loss, Some(tp), Some(pos.qty)) {
        if reason.contains("безубыток на 1R") {
            let _ = client.cancel_protectives(&pos.symbol);
            let close_note = match client.market_close(&pos.symbol, "LONG", pos.qty) {
                Ok(()) => String::new(),
                Err(close_e) => format!("; close: {close_e}"),
            };
            if let Some(st) = state {
                journal::record_flatten(
                    st.strategy_id,
                    &[pos.symbol.clone()],
                    cfg.live,
                    "flattened naked fill",
                );
            }
            let mark = snapshot
                .tickers
                .iter()
                .find(|t| t.symbol.eq_ignore_ascii_case(&pos.symbol))
                .map(|t| t.last_price);
            return LiveApplyResult {
                error: Some(format!("flattened naked fill: {e}{close_note}")),
                filled: false,
                mark,
                qty: Some(pos.qty),
                forget_symbol: pos.symbol.clone(),
            };
        }
        return err(e.0);
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

fn exit_live(
    client: &mut dyn LiveClient,
    snapshot: &MarketSnapshot,
    state: Option<&EngineState>,
    symbol: &str,
) -> LiveApplyResult {
    let mut pos = position_for(snapshot, state, symbol).cloned();
    if snapshot.live_book {
        let hint = pos.as_ref().map(|p| p.symbol.clone()).unwrap_or_else(|| symbol.to_string());
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
        return err(e.0);
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

/// Partial close ~qty then BE-amend remainder. Fail-closed flatten if BE protectives miss.
fn reduce_live(
    cfg: &Config,
    client: &mut dyn LiveClient,
    snapshot: &MarketSnapshot,
    state: Option<&EngineState>,
    symbol: &str,
    reduce_qty: Decimal,
    stop_loss: Decimal,
    reason: &str,
) -> LiveApplyResult {
    let Some(pos) = position_for(snapshot, state, symbol).cloned() else {
        return err("skip reduce: no position");
    };
    if pos.side != Side::Long || pos.qty <= Decimal::ZERO {
        return err("skip reduce: нет живого лонга");
    }
    let close_qty = reduce_qty.min(pos.qty);
    if close_qty <= Decimal::ZERO || close_qty >= pos.qty {
        // Cannot partial — fall through to full BE amend path semantics.
        return amend_live(cfg, client, snapshot, state, stop_loss, &pos.symbol, "безубыток на 1R");
    }
    let remain = pos.qty - close_qty;
    // Do not cancel protectives first — that would naked the remainder.
    if let Err(e) = client.market_close(&pos.symbol, "LONG", close_qty) {
        return err(e.0);
    }
    let tp = pos.take_profit.or_else(|| {
        if pos.entry_price <= Decimal::ZERO {
            None
        } else {
            take_profit_price_net(pos.entry_price, "LONG", cfg.tp_pct).ok()
        }
    });
    if let Err(e) = client.replace_stop(&pos.symbol, stop_loss, tp, Some(remain)) {
        let _ = client.cancel_protectives(&pos.symbol);
        let close_note = match client.market_close(&pos.symbol, "LONG", remain) {
            Ok(()) => String::new(),
            Err(close_e) => format!("; close: {close_e}"),
        };
        if let Some(st) = state {
            journal::record_flatten(
                st.strategy_id,
                &[pos.symbol.clone()],
                cfg.live,
                "flattened naked fill",
            );
        }
        let mark = mark_for_symbol(snapshot, &pos.symbol);
        return LiveApplyResult {
            error: Some(format!("flattened naked fill after reduce: {e}{close_note}")),
            filled: true,
            mark,
            qty: Some(close_qty),
            forget_symbol: pos.symbol.clone(),
        };
    }
    if let Some(flipped) = flatten_live_short(client, &pos.symbol) {
        return LiveApplyResult {
            error: Some(format!("reduce перевернул в шорт — закрыл {}", flipped.symbol)),
            filled: true,
            mark: mark_for_symbol(snapshot, &pos.symbol),
            qty: Some(close_qty),
            forget_symbol: flipped.symbol,
            ..Default::default()
        };
    }
    let _ = reason; // journaled in apply_decision
    LiveApplyResult {
        filled: true,
        mark: mark_for_symbol(snapshot, &pos.symbol),
        qty: Some(close_qty),
        ..Default::default()
    }
}

/// Send the decision to TestNet. No-op unless `cfg.live` and keys are present.
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
        return err("live refused: no credentials");
    }
    match decision {
        Decision::EnterLong {
            symbol,
            take_profit,
            stop_loss,
            ..
        } => enter_live(cfg, client, snapshot, state, symbol, *take_profit, *stop_loss),
        Decision::AmendStop {
            stop_loss,
            reason,
            symbol,
            ..
        } => amend_live(cfg, client, snapshot, state, *stop_loss, symbol, reason),
        Decision::ExitPosition { symbol, .. } => exit_live(client, snapshot, state, symbol),
        Decision::ReduceLong {
            symbol,
            qty,
            stop_loss,
            reason,
            ..
        } => reduce_live(cfg, client, snapshot, state, symbol, *qty, *stop_loss, reason),
        Decision::Hold { .. } => LiveApplyResult::default(),
    }
}


fn adopt_amended_stop(state: &mut EngineState, symbol: &str, stop_loss: Decimal) {
    for p in state.positions.iter_mut() {
        if p.symbol.eq_ignore_ascii_case(symbol) {
            p.stop_loss = Some(stop_loss);
        }
    }
    if let Some(p) = state.position.as_mut() {
        if p.symbol.eq_ignore_ascii_case(symbol) {
            p.stop_loss = Some(stop_loss);
        }
    }
}

fn adopt_reduced_long(state: &mut EngineState, symbol: &str, closed_qty: Decimal, stop_loss: Decimal) {
    for p in state.positions.iter_mut() {
        if p.symbol.eq_ignore_ascii_case(symbol) {
            p.qty = (p.qty - closed_qty).max(Decimal::ZERO);
            p.stop_loss = Some(stop_loss);
        }
    }
    state.positions.retain(|p| p.qty > Decimal::ZERO);
    if let Some(p) = state.position.as_mut() {
        if p.symbol.eq_ignore_ascii_case(symbol) {
            p.qty = (p.qty - closed_qty).max(Decimal::ZERO);
            p.stop_loss = Some(stop_loss);
            if p.qty <= Decimal::ZERO {
                state.position = state.positions.first().cloned();
            }
        }
    } else {
        state.position = state.positions.first().cloned();
    }
    let key = symbol.to_ascii_uppercase();
    state.sized_stops.insert(key.clone());
    state.scaled_one_r.insert(key);
}

/// Paper/offline adopt: mutate local book on AmendStop / ReduceLong / Exit.
/// No exchange calls. Used by TUI when `!cfg.live` and by tests.
pub fn apply_paper_decision(
    state: &mut EngineState,
    snapshot: &MarketSnapshot,
    decision: &Decision,
) {
    match decision {
        Decision::AmendStop {
            stop_loss,
            symbol,
            reason,
            ..
        } => {
            adopt_amended_stop(state, symbol, *stop_loss);
            let tp = state
                .positions
                .iter()
                .chain(state.position.iter())
                .find(|p| p.symbol.eq_ignore_ascii_case(symbol))
                .and_then(|p| p.take_profit);
            journal::record_amend(
                state.strategy_id,
                symbol,
                *stop_loss,
                tp,
                false,
                reason,
            );
        }
        Decision::ReduceLong {
            symbol,
            qty,
            stop_loss,
            reason,
            ..
        } => {
            let Some(pos) = position_for(snapshot, Some(state), symbol).cloned() else {
                return;
            };
            let close_qty = (*qty).min(pos.qty);
            if close_qty <= Decimal::ZERO {
                return;
            }
            let remaining = pos.qty - close_qty;
            if remaining <= Decimal::ZERO {
                // Full close via reduce — treat as exit of the book slot.
                let exit_px = mark_for_symbol(snapshot, symbol).unwrap_or(pos.entry_price);
                journal::record_close(
                    state.strategy_id,
                    &pos.symbol,
                    pos.qty,
                    pos.entry_price,
                    exit_px,
                    reason,
                    false,
                    pos.stop_loss,
                    pos.take_profit,
                );
                drop_symbol(state, symbol);
                return;
            }
            let exit_px = mark_for_symbol(snapshot, symbol).unwrap_or(pos.entry_price);
            journal::record_close(
                state.strategy_id,
                &pos.symbol,
                close_qty,
                pos.entry_price,
                exit_px,
                reason,
                false,
                pos.stop_loss,
                pos.take_profit,
            );
            adopt_reduced_long(state, symbol, close_qty, *stop_loss);
            let tp = state
                .positions
                .iter()
                .chain(state.position.iter())
                .find(|p| p.symbol.eq_ignore_ascii_case(symbol))
                .and_then(|p| p.take_profit);
            journal::record_amend(
                state.strategy_id,
                symbol,
                *stop_loss,
                tp,
                false,
                "безубыток на 1R",
            );
        }
        Decision::ExitPosition { symbol, reason } => {
            let Some(pos) = position_for(snapshot, Some(state), symbol).cloned() else {
                return;
            };
            let exit_px = mark_for_symbol(snapshot, symbol).unwrap_or(pos.entry_price);
            journal::record_close(
                state.strategy_id,
                &pos.symbol,
                pos.qty,
                pos.entry_price,
                exit_px,
                reason,
                false,
                pos.stop_loss,
                pos.take_profit,
            );
            drop_symbol(state, symbol);
        }
        _ => {}
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
        opened_bar_time: Some((unix_now() * 1000.0) as i64),
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
            arm_entry_pause(state, unix_now());
        } else {
            state.positions.retain(|p| !closed_syms.contains(&p.symbol));
            state.position = state.positions.first().cloned();
        }
        let prefix = if pause_entries { "FLAT " } else { "FLAT хвосты " };
        state.push_action(unix_now(), format!("{prefix}{}", result.closed.join(", ")));
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
    snapshot: Option<&MarketSnapshot>,
    targets: Option<&[Position]>,
) -> FlattenResult {
    let book = positions_for_flatten(snapshot, state, targets);
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
    emit_flatten(&out, flatten_sound_won(&out.closed, &book));
    out
}

/// Apply one decision, journal the fill/close, adopt state. TUI calls this per tick.
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
    let reducing = if let Decision::ReduceLong { symbol, reason, qty, .. } = decision {
        position_for(snapshot, Some(state), symbol)
            .cloned()
            .map(|p| (p, reason.clone(), *qty))
    } else {
        None
    };
    let result = apply_live(cfg, client, snapshot, decision, Some(state));
    if let Some((pos, reason)) = &closing {
        // Vanished live longs are journaled in clear_vanished_longs. A second
        // close line double-counts PnL in --report.
        if result.error.is_none() && result.filled {
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
                pos.stop_loss,
                pos.take_profit,
            );
            let won = journal::long_close_was_win(pos.entry_price, exit_px, pos.take_profit);
            if !won {
                desk_pause_after_loss(cfg, state, unix_now());
            }
            cool_symbol(state, &pos.symbol, unix_now(), won);
        }
    }
    if let Some((pos, reason, closed_qty)) = &reducing {
        // Paper/sim: apply_live is a no-op (filled=false). Still journal + adopt,
        // same gap AmendStop once had — latch scaled_one_r so manage does not re-reduce.
        let paper_or_filled = result.filled || !cfg.live;
        if result.error.is_none() && paper_or_filled {
            let exit_px = result
                .mark
                .or_else(|| mark_for_symbol(snapshot, &pos.symbol))
                .unwrap_or(pos.entry_price);
            let qty = result.qty.unwrap_or(*closed_qty);
            journal::record_close(
                state.strategy_id,
                &pos.symbol,
                qty,
                pos.entry_price,
                exit_px,
                reason,
                cfg.live,
                pos.stop_loss,
                pos.take_profit,
            );
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
        if info.action != ACTION_IGNORE {
            if let Decision::AmendStop { symbol, .. } = decision {
                state.sized_stops.remove(&symbol.to_ascii_uppercase());
            }
        }
        if info.action == ACTION_IGNORE {
            state.last_error = None;
        } else {
            state.last_error = Some(err.clone());
            if info.action == ACTION_RETRY {
                state.retry_strikes = state.retry_strikes.saturating_add(1).min(3);
                let until = unix_now() + retry_backoff_sec(state.retry_strikes);
                if until > state.retry_until {
                    state.retry_until = until;
                }
            }
            if info.action == ACTION_OPERATOR {
                arm_entry_pause(state, unix_now());
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
        state.retry_strikes = 0;
    }
    if result.error.is_none() {
        if let Decision::AmendStop {
            stop_loss,
            symbol,
            reason,
            ..
        } = decision
        {
            adopt_amended_stop(state, symbol, *stop_loss);
            let tp = state
                .positions
                .iter()
                .chain(state.position.iter())
                .find(|p| p.symbol.eq_ignore_ascii_case(symbol))
                .and_then(|p| p.take_profit);
            journal::record_amend(
                state.strategy_id,
                symbol,
                *stop_loss,
                tp,
                cfg.live,
                reason,
            );
        }
        if let Decision::ReduceLong {
            stop_loss,
            symbol,
            reason,
            qty,
            ..
        } = decision
        {
            // Live needs an exchange fill; paper adopts when a book slot exists
            // (same gap AmendStop once had). Prefer apply_paper_decision in TUI.
            if result.filled || (!cfg.live && reducing.is_some()) {
                let closed = result.qty.unwrap_or(*qty);
                adopt_reduced_long(state, symbol, closed, *stop_loss);
                let tp = remembered_sl_tp(state, symbol).1;
                journal::record_amend(
                    state.strategy_id,
                    symbol,
                    *stop_loss,
                    tp,
                    cfg.live,
                    "безубыток на 1R",
                );
                let _ = reason;
            } else if cfg.live && reducing.is_some() {
                // Lot could not be split — reduce_live fell through to BE amend.
                adopt_amended_stop(state, symbol, *stop_loss);
                let key = symbol.to_ascii_uppercase();
                state.scaled_one_r.insert(key.clone());
                state.sized_stops.insert(key);
                let tp = remembered_sl_tp(state, symbol).1;
                journal::record_amend(
                    state.strategy_id,
                    symbol,
                    *stop_loss,
                    tp,
                    cfg.live,
                    "безубыток на 1R",
                );
            }
        }
    }
    if result.filled {
        if let Decision::EnterLong { .. } = decision {
            if let (Some(mark), Some(qty)) = (result.mark, result.qty) {
                adopt_live_fill(state, decision, mark, qty);
                if result.error.is_none() {
                    if let Decision::EnterLong {
                        symbol,
                        reason,
                        stop_loss,
                        take_profit,
                    } = decision
                    {
                        state.sized_stops.insert(symbol.to_ascii_uppercase());
                        journal::record_open(
                            state.strategy_id,
                            symbol,
                            qty,
                            mark,
                            reason,
                            cfg.live,
                            Some(*stop_loss),
                            Some(*take_profit),
                        );
                    }
                }
            } else {
                state.entry_inflight = true;
            }
        }
    }
    let has_position = snapshot.position.is_some()
        || snapshot.open_positions.iter().any(|p| p.qty > Decimal::ZERO);
    let sound_won = closing.as_ref().map(|(pos, reason)| {
        exit_sound_won(pos, reason, &result, snapshot)
    });
    emit_decision(decision, &result, cfg.live, has_position, sound_won);
    result
}

fn drop_symbol(state: &mut EngineState, symbol: &str) {
    let want = symbol.to_ascii_uppercase();
    state.positions.retain(|p| p.symbol.to_ascii_uppercase() != want);
    state.position = state.positions.first().cloned();
    state.inflight_symbols.retain(|s| s.to_ascii_uppercase() != want);
    state.entry_inflight = !state.inflight_symbols.is_empty() && state.positions.is_empty();
    state.sized_stops.remove(&want);
    state.rearm_miss_since.remove(&want);
    state.rearm_fail_count.remove(&want);
    state.scaled_one_r.remove(&want);
}

fn remembered_sl_tp(state: &EngineState, symbol: &str) -> (Option<Decimal>, Option<Decimal>) {
    let want = symbol.to_ascii_uppercase();
    for pos in state.positions.iter().chain(state.position.iter()) {
        if pos.symbol.eq_ignore_ascii_case(&want) {
            return (pos.stop_loss, pos.take_profit);
        }
    }
    (None, None)
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
    let now = unix_now();
    let mut done = Vec::new();
    let longs: Vec<Position> = snapshot
        .open_positions
        .iter()
        .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
        .cloned()
        .collect();
    for live in longs {
        let key = live.symbol.to_ascii_uppercase();
        let (sl, tp) = match (live.stop_loss, live.take_profit) {
            (Some(sl), Some(tp)) => (sl, tp),
            (sl, tp) => {
                let (rsl, rtp) = remembered_sl_tp(state, &key);
                match (sl.or(rsl), tp.or(rtp)) {
                    (Some(sl), Some(tp)) => (sl, tp),
                    _ => {
                        // Try attach-from-entry once; still naked after budget → flatten.
                        let derived = derive_protectives_from_entry(cfg, &live);
                        match derived {
                            Some(pair) => pair,
                            None => {
                                if note_rearm_failure(state, &key, now) {
                                    flatten_missing_protectives(
                                        cfg, client, state, &live, "нет SL/TP для rearm",
                                    );
                                    done.push(live.symbol.clone());
                                }
                                continue;
                            }
                        }
                    }
                }
            }
        };
        match client.open_algo_orders(Some(&key)) {
            Ok(rows) if sell_protectives_are_sized(&rows) => {
                clear_rearm_tracking(state, &key);
                state.sized_stops.insert(key);
                continue;
            }
            // Listing error is not proof the stop is still there. Rearm.
            _ => {}
        }
        if let Err(exc) = client.replace_stop(&live.symbol, sl, Some(tp), Some(live.qty)) {
            if classify(&exc.0).code == Some(-4130) {
                clear_rearm_tracking(state, &key);
                state.sized_stops.insert(key);
                continue;
            }
            if note_rearm_failure(state, &key, now) {
                flatten_missing_protectives(cfg, client, state, &live, &exc.0);
                done.push(live.symbol.clone());
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
        clear_rearm_tracking(state, &key);
        state.sized_stops.insert(key);
        done.push(live.symbol.clone());
        push_recent(state, format!("TP/SL на размер лонга {}", live.symbol));
    }
    done
}

fn clear_rearm_tracking(state: &mut EngineState, key: &str) {
    state.rearm_miss_since.remove(key);
    state.rearm_fail_count.remove(key);
}

/// Returns true when consecutive fails or wall budget is exhausted.
fn note_rearm_failure(state: &mut EngineState, key: &str, now: f64) -> bool {
    let since = *state.rearm_miss_since.entry(key.to_string()).or_insert(now);
    let count = state.rearm_fail_count.entry(key.to_string()).or_insert(0);
    *count = count.saturating_add(1);
    *count >= REARM_FAIL_MAX || (now - since) >= REARM_FAIL_BUDGET_SEC
}

fn derive_protectives_from_entry(cfg: &Config, live: &Position) -> Option<(Decimal, Decimal)> {
    if live.entry_price <= Decimal::ZERO {
        return None;
    }
    let min_stop = crate::continuation::ContinuationParams::default()
        .with_interval(cfg.s4_interval)
        .min_stop_pct;
    let sl = candidate_stop(live.entry_price, "LONG", min_stop).ok()?;
    let tp = take_profit_price_net(live.entry_price, "LONG", cfg.tp_pct).ok()?;
    if sl <= Decimal::ZERO || tp <= sl {
        return None;
    }
    Some((sl, tp))
}

fn flatten_missing_protectives(
    cfg: &Config,
    client: &mut dyn LiveClient,
    state: &mut EngineState,
    live: &Position,
    detail: &str,
) {
    let _ = client.cancel_protectives(&live.symbol);
    let close_note = match client.market_close(&live.symbol, "LONG", live.qty) {
        Ok(()) => String::new(),
        Err(e) => format!("; close: {e}"),
    };
    journal::record_flatten(
        state.strategy_id,
        &[live.symbol.clone()],
        cfg.live,
        "нет protectives — flatten",
    );
    push_recent(
        state,
        format!("нет protectives — flatten {}{close_note}", live.symbol),
    );
    state.last_error = Some(format!(
        "rearm budget exhausted ({} fails / {}s): {detail}{close_note}",
        REARM_FAIL_MAX, REARM_FAIL_BUDGET_SEC as i64
    ));
    drop_symbol(state, &live.symbol);
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
    let mut cleared = Vec::new();
    for pos in remembered {
        let _ = client.cancel_protectives(&pos.symbol);
        let exit_px = mark_for_symbol(snapshot, &pos.symbol).unwrap_or(pos.entry_price);
        let won = journal::long_close_was_win(pos.entry_price, exit_px, pos.take_profit);
        let reason = if won {
            "биржа закрыла лонг по TP"
        } else {
            "биржа закрыла лонг"
        };
        journal::record_close(
            state.strategy_id,
            &pos.symbol,
            pos.qty,
            pos.entry_price,
            exit_px,
            reason,
            cfg.live,
            pos.stop_loss,
            pos.take_profit,
        );
        cool_symbol(state, &pos.symbol, now, won);
        if !won {
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
    // Prefer account-wide listings. Per-symbol probes are only a fallback when
    // both global calls fail — otherwise every TUI tick hammered BTC/ETH/SOL/chart
    // with 2×N REST on the UI thread (felt like a hang).
    let mut listed_ok = false;
    match client.open_algo_orders(None) {
        Ok(rows) => {
            leftover.extend(collect_order_symbols(&rows));
            listed_ok = true;
        }
        Err(_) => {}
    }
    match client.open_orders(None) {
        Ok(rows) => {
            leftover.extend(collect_order_symbols(&rows));
            listed_ok = true;
        }
        Err(_) => {}
    }
    if !listed_ok {
        for symbol in probe_orphan_symbols(snapshot, state) {
            if live_longs.contains(&symbol) {
                continue;
            }
            let algos = client.open_algo_orders(Some(&symbol)).ok().unwrap_or_default();
            let orders = client.open_orders(Some(&symbol)).ok().unwrap_or_default();
            if !algos.is_empty() || !orders.is_empty() {
                leftover.insert(symbol);
            }
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
        emit_flatten(&result, flatten_sound_won(&result.closed, &shorts));
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
    let rearmed = rearm_live_protectives(cfg, client, state, snapshot);
    let last_text = if !swept.closed.is_empty() {
        format!("закрыл чужой шорт: {}", swept.closed.join(", "))
    } else if !orphans.is_empty() {
        format!("снял сиротский стоп: {}", orphans.join(", "))
    } else if !rearmed.is_empty() {
        format!("TP/SL на размер лонга: {}", rearmed.join(", "))
    } else {
        String::new()
    };
    ReconcileResult { last_text }
}

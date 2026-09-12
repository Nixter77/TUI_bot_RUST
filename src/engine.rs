//! Strategy orchestration over market snapshots. Pure: returns decisions only.

use crate::config::default_risk_pct;
use crate::continuation::{continuation_decisions, ContinuationParams, SCAN_SEC};
use crate::dayrisk::{apply_day_risk, default_daily_loss_r, default_daily_loss_usdt};
use crate::errors::is_retry_error;
use crate::models::{
    coalesce_position, last_closed_bar, push_recent, remembered_positions, unmanaged_positions,
    Decision, EngineState, MarketSnapshot, Position, Side,
};
use crate::momentum::mark_for;
use crate::profit::current_equity;
use crate::ranking::iter_liquid_majors;
use crate::scalp::{scalp_decision, ScalpParams};
use crate::sessions::HourWindow;
use crate::trend::{trend_decision, TrendParams};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

pub use crate::momentum::{momentum_decision, momentum_decisions, MomentumParams};

pub const STRATEGY_IDS: [i32; 5] = [1, 2, 3, 4, 5];

pub const STRATEGY_NAMES: [(i32, &'static str); 5] = [
    (1, "Momentum rider (растущий + TP + SL вверх)"),
    (2, "Скальп: откат к VWAP/EMA9"),
    (3, "Тренд: пробой Donchian 20/10 (день)"),
    (4, "Continuation: откат ликвидных (не догон 24h %)"),
    (5, "S5 Verify: continuation 1ч (A/B vs S4)"),
];

/// S4 (env TF) and S5 Verify (locked 1h) share the continuation core.
pub fn is_continuation(strategy_id: i32) -> bool {
    matches!(strategy_id, 4 | 5)
}

/// Signal TF: S5 is the 1h verification arm; S4 keeps STRATEGY4_INTERVAL.
pub fn continuation_interval(
    strategy_id: i32,
    s4_interval: crate::config::TradeInterval,
) -> crate::config::TradeInterval {
    if strategy_id == 5 {
        crate::config::TradeInterval::Hour1
    } else {
        s4_interval
    }
}

/// Stop/pullback band follows the signal TF. S5 Verify uses Hour1 geometry
/// (3–8%) so 1h ATR fits; pinning the 15m 2–5% soak band skipped setups and
/// put the stop inside 1h noise.
pub fn continuation_stop_band(
    strategy_id: i32,
    s4_interval: crate::config::TradeInterval,
) -> crate::config::TradeInterval {
    continuation_interval(strategy_id, s4_interval)
}

/// Basket cap: S5 reads `STRATEGY5_MAX_POSITIONS` (falls back to S4 when unset).
pub fn continuation_slot_cap(strategy_id: i32, s4: i32, s5: i32) -> i32 {
    if strategy_id == 5 {
        s5
    } else {
        s4
    }
}

/// S4 and S5 share `STRATEGY4_ALWAYS_ENTER` / entry windows.
/// Forced S5-only UTC bands (s5-entry-hours) were 2nd-worst bt_netR; S4 soak unchanged.
pub fn continuation_session_knobs(
    _strategy_id: i32,
    s4_always_enter: bool,
    s4_entry_windows: &[HourWindow],
) -> (bool, Vec<HourWindow>) {
    (s4_always_enter, s4_entry_windows.to_vec())
}

/// Continuation params: signal TF + stop/pullback from `continuation_stop_band`.
/// S5 is Hour1 3–8% / 2% pullback even when S4 soaks 15m 2–5%.
pub fn continuation_trade_params(
    strategy_id: i32,
    s4_interval: crate::config::TradeInterval,
) -> crate::continuation::ContinuationParams {
    use crate::continuation::ContinuationParams;
    let signal = continuation_interval(strategy_id, s4_interval);
    let band = continuation_stop_band(strategy_id, s4_interval);
    let mut p = ContinuationParams::default().with_interval(signal);
    p.min_stop_pct = band.min_stop_pct();
    p.max_stop_pct = band.max_stop_pct();
    p.min_pullback_pct = band.min_pullback_pct();
    if strategy_id == 5 {
        // A/B vs 5% and 8% on cached 1h: 3% had higher pnl (less skip) than 8%.
        p.near_high_frac = Decimal::new(3, 2);
        // One step above S4 50k so thin 1h names fail the desk floor.
        p.min_quote_volume = Decimal::from(100_000);
        // A/B vs 3 and 4 on cached 1h: lookback 2 had higher pnl (4 matched 3).
        p.stop_lookback = 2;
        // A/B vs 2.5× on cached 1h: 2×ATR had higher pnl (same n/wr, still in Hour1 3–8%).
        p.atr_k = Decimal::from(2);
        // A/B vs 1.5R on cached 1h: 2R bank had higher pnl (same n/wr). S4 stays 1.5R.
        p.bank_r = Decimal::from(2);
        // A/B 0.5× SMA20 vol: cache pnl +0.24 vs prior +3.09 — keep 0.3× hist mean.
        // A/B 1.5% vs 2.5% vs 2% on cached 1h: 1.5% had higher pnl. S4 band stays 2%.
        p.min_pullback_pct = Decimal::new(15, 3);
        // A/B 15 vs 25 vs 20: per-symbol cache is identical (one ticker). Keep 20.
        p.liquid_n = 20;
    }
    p
}

pub fn strategy_title(id: i32) -> &'static str {
    STRATEGY_NAMES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, n)| *n)
        .unwrap_or("")
}

pub fn select_strategy(raw: i32) -> Result<i32, String> {
    if !STRATEGY_IDS.contains(&raw) {
        return Err("strategy must be 1, 2, 3, 4, or 5".into());
    }
    Ok(raw)
}

pub fn select_strategy_str(raw: &str) -> Result<i32, String> {
    let sid: i32 = raw
        .parse()
        .map_err(|_| "strategy must be 1, 2, 3, 4, or 5".to_string())?;
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
        }
        by_reason
            .entry(reason.clone())
            .or_default()
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
    if is_continuation(sid) {
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
        let trail_bar = HashMap::new();
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
            &trail_bar,
            &HashSet::new(),
        );
        return Ok((
            d.into_iter()
                .next()
                .unwrap_or_else(|| Decision::hold("hold")),
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
            return Ok((
                trend_decision(bars, Some(pos), &pos.symbol, trend),
                last_scan_ts,
            ));
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

fn persist_last_error(held: Option<&str>, retry_until: f64, now: f64) -> Option<String> {
    let Some(s) = held else {
        return None;
    };
    // Drop stale retry noise only after backoff; during backoff the footer
    // must still show why entries are blocked (3AM timeout / 5xx).
    if is_retry_error(Some(s)) && now >= retry_until {
        None
    } else {
        Some(s.to_string())
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
        4 | 5 => ContinuationParams::default().cooldown_sec,
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

fn drop_stale_inflight(
    pending: Vec<String>,
    snapshot: &MarketSnapshot,
    last_scan_ts: f64,
    now: f64,
    retry_until: f64,
) -> Vec<String> {
    // Buy timed out and positionRisk 502: keep the slot so we do not double-buy.
    if now < retry_until {
        return pending;
    }
    if snapshot.live_book
        && snapshot.account_fresh
        && last_scan_ts > 0.0
        && now - last_scan_ts >= SCAN_SEC
    {
        Vec::new()
    } else {
        pending
    }
}

fn expire_entries_paused(paused: bool, cooldown_until: f64, now: f64) -> bool {
    if paused && cooldown_until > 0.0 && now >= cooldown_until {
        false
    } else {
        paused
    }
}

fn continuation_params(strategy_id: i32, momentum: Option<&MomentumParams>) -> ContinuationParams {
    let s4 = momentum.map(|m| m.s4_interval).unwrap_or_default();
    let mut p = continuation_trade_params(strategy_id, s4);
    if let Some(m) = momentum {
        // Honor STRATEGY5_MAX_POSITIONS 1–10 (do not floor at 3 — that ignored 1|2).
        let cap = continuation_slot_cap(strategy_id, m.s4_max_positions, m.s5_max_positions);
        p.max_positions = cap.clamp(1, 10);
        let (always, windows) =
            continuation_session_knobs(strategy_id, m.s4_always_enter, &m.s4_entry_windows);
        p.always_enter = always;
        p.entry_windows = windows;
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
    continuation_override: Option<&ContinuationParams>,
) -> (EngineState, Decision) {
    let (new_state, decisions) = tick_decisions(
        state,
        snapshot,
        now,
        momentum,
        scalp,
        trend,
        continuation_override,
    );
    (
        new_state,
        decisions
            .into_iter()
            .next()
            .unwrap_or_else(|| Decision::hold("hold")),
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
    _continuation_override: Option<&ContinuationParams>,
) -> (EngineState, Vec<Decision>) {
    let mut state = state.clone();
    let remembered = remembered_positions(state.position.as_ref(), &state.positions);
    let (mut merged_list, mut inflight): (Vec<Position>, Vec<String>) = if snapshot.live_book {
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
                let rem = remembered
                    .iter()
                    .find(|r| r.symbol.eq_ignore_ascii_case(&live.symbol));
                coalesce_position(Some(live), rem).unwrap_or_else(|| live.clone())
            })
            .collect();
        let live_keys: HashSet<String> = merged
            .iter()
            .map(|p| p.symbol.to_ascii_uppercase())
            .collect();
        let pending = drop_stale_inflight(
            state
                .inflight_symbols
                .iter()
                .filter(|s| !live_keys.contains(&s.to_ascii_uppercase()))
                .cloned()
                .collect(),
            snapshot,
            state.last_scan_ts,
            now,
            state.retry_until,
        );
        (merged, pending)
    } else {
        let mut merged_list = remembered.clone();
        if let Some(pos) = &snapshot.position {
            if pos.qty > Decimal::ZERO
                && !merged_list
                    .iter()
                    .any(|p| p.symbol.eq_ignore_ascii_case(&pos.symbol))
            {
                let rem = remembered
                    .iter()
                    .find(|r| r.symbol.eq_ignore_ascii_case(&pos.symbol));
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

    // S4/S5: only manage opens tagged for this strategy_id (foreign → unmanaged).
    if is_continuation(state.strategy_id) {
        merged_list.retain(|p| {
            crate::openmeta::continuation_owns(&p.symbol, state.strategy_id, &state.s4_inherited)
        });
    }

    let merged = merged_list.first().cloned();
    crate::openmeta::update_from_positions(&merged_list, snapshot, now);
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
    let loss_windows: Vec<crate::sessions::HourWindow> = if is_continuation(state.strategy_id) {
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
        state.hour1_trail_bar.remove(&up);
        state.s4_inherited.remove(&up);
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
                crate::journal::symbol_cooldown_until_for(state.strategy_id, now, won, pause_sec),
            );
            if !won {
                let until = crate::sessions::pause_until_after_loss(now, &loss_windows, pause_sec);
                cooldown_until = cooldown_until.max(until);
            }
        }
    }
    // Restore latch after a transient book blip cleared scaled_one_r:
    // (1) SL already at/above entry (BE done), or (2) open_meta scaled_at_1r for *this* entry.
    for pos in &merged_list {
        if pos.side != Side::Long || pos.qty <= Decimal::ZERO {
            continue;
        }
        let key = pos.symbol.to_ascii_uppercase();
        if let Some(sl) = pos.stop_loss {
            if sl >= pos.entry_price {
                state.scaled_one_r.insert(key.clone());
                continue;
            }
        }
        if crate::openmeta::meta_scaled_for_entry(&key, pos.entry_price, state.strategy_id) {
            state.scaled_one_r.insert(key);
        }
    }
    let now_flat = merged_list.is_empty();

    let sid = state.strategy_id;
    let limit = momentum
        .map(|m| m.daily_loss_usdt)
        .unwrap_or_else(default_daily_loss_usdt);
    let limit_r = momentum
        .map(|m| m.daily_loss_r)
        .unwrap_or_else(default_daily_loss_r);
    let risk_pct = momentum
        .map(|m| m.risk_pct)
        .unwrap_or_else(default_risk_pct);
    if snapshot.account_ok {
        apply_day_risk(
            &mut state,
            now,
            current_equity(
                snapshot.account.wallet_balance,
                snapshot.account.unrealized_pnl,
            ),
            limit,
            limit_r,
            risk_pct,
        );
    }
    let mut tail = Vec::new();
    if snapshot.live_book {
        // Flat book leftovers OR foreign-strategy longs (S4/S5 isolation).
        tail = unmanaged_positions(&snapshot.open_positions, &merged_list);
        if !now_flat {
            // Keep only non-managed rows (foreign / shorts); managed stays in book.
            // unmanaged_positions already excludes managed longs.
        }
    }

    let mut next_leaders = state.recent_leaders.clone();
    let entries_paused = expire_entries_paused(state.entries_paused, state.cooldown_until, now);
    let (mut decisions, scan_ts) = if entries_paused {
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
        let inflight_f: Vec<String> = inflight
            .iter()
            .filter(|s| s.as_str() != "*")
            .cloned()
            .collect();
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
            Some(snapshot),
        )
    } else if is_continuation(sid) {
        // S4 and S5 share DAILY_LOSS: allow_enter is !daily_halt; flatten/trail still run.
        let inflight_f: Vec<String> = inflight
            .iter()
            .filter(|s| s.as_str() != "*")
            .cloned()
            .collect();
        let cont = continuation_params(sid, momentum);
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
            &state.hour1_trail_bar,
            &state.s4_inherited,
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
        // Latch on decision (before apply). try_lock skip / failed BE must not re-Reduce.
        if let Decision::ReduceLong { symbol, .. } = decision {
            let key = symbol.to_ascii_uppercase();
            state.scaled_one_r.insert(key.clone());
            crate::openmeta::mark_scaled(&key);
        }
        if let Decision::AmendStop { symbol, reason, .. } = decision {
            if sid == 5 && reason.contains("trail по минимуму") {
                if let Some(bar) = last_closed_bar(snapshot.bars_for(symbol)) {
                    state
                        .hour1_trail_bar
                        .insert(symbol.to_ascii_uppercase(), bar.open_time);
                }
            }
        }
    }

    let mut actions = state.recent_actions.clone();
    for decision in &decisions {
        if !decision.is_hold() {
            push_recent(&mut actions, now, decision.describe());
        }
    }
    let book: Vec<Position> = merged_list
        .into_iter()
        .filter(|p| p.qty > Decimal::ZERO)
        .collect();
    let mut last_error = persist_last_error(state.last_error.as_deref(), state.retry_until, now);
    if last_error.is_none() {
        last_error = crate::journal::take_last_error();
    }
    if last_error.is_none() {
        last_error = crate::telegram::take_last_error();
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
        entries_paused,
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
        hour1_trail_bar: state.hour1_trail_bar,
        s4_inherited: state.s4_inherited,
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

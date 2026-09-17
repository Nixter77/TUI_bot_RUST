//! Desk orchestrator — one process, UTC owner for entries, manage by opening sid.
//!
//! Flag `DESK_ORCHESTRATOR=1` (default **off**). Paper-first. Live multi-process
//! remains **BLOCK**. Edge? **no claim**.
//!
//! Reuses `desk_schedule::desk_owner` — do not invent a second clock.
//!
//! Env knobs (sensible defaults):
//! - `DESK_ORCHESTRATOR` — `1`/`true`/`yes` enables (default off)
//! - `DESK_HEAT_MAX` — max open desk longs across all sids (default **3**)
//! - `DESK_HEAT_MAX_PER_SID` — max open longs per opening sid (default **2**)

use crate::continuation::continuation_decisions;
use crate::desk_schedule::{desk_owner_at, DeskSid};
use crate::engine::{continuation_params_for, MomentumParams};
use crate::models::{self, Decision, EngineState, MarketSnapshot, Position, Side};
use crate::momentum::mark_for;
use crate::openmeta;
use crate::scalp::{scalp_decision, ScalpParams};
use crate::sessions::utc_datetime;
use crate::trend::{trend_decision, TrendParams};
use chrono::Timelike;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Mutex;

/// Env flag tests must not race under `--test-threads > 1`.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

pub fn desk_orchestrator_enabled() -> bool {
    matches!(
        env::var("DESK_ORCHESTRATOR").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

fn parse_usize_env(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n >= 1 && n <= 20)
        .unwrap_or(default)
}

/// Desk-wide open long cap. Default 3.
pub fn desk_heat_max() -> usize {
    parse_usize_env("DESK_HEAT_MAX", 3)
}

/// Per opening-sid open long cap. Default 2.
pub fn desk_heat_max_per_sid() -> usize {
    parse_usize_env("DESK_HEAT_MAX_PER_SID", 2)
}

pub fn is_desk_sid(sid: i32) -> bool {
    matches!(sid, 2 | 3 | 4)
}

/// Opening strategy for a symbol from durable meta (fail-open → None).
pub fn opening_sid(symbol: &str) -> Option<i32> {
    openmeta::get(symbol).map(|m| m.strategy_id)
}

/// Opening sid for manage, falling back to active owner when untagged.
pub fn opening_sid_or(symbol: &str, fallback: i32) -> i32 {
    opening_sid(symbol).unwrap_or(fallback)
}

/// Keep longs the desk owns: tagged desk sid, or untagged (paper first tick).
pub fn manages_long(symbol: &str) -> bool {
    match opening_sid(symbol) {
        Some(sid) => is_desk_sid(sid),
        // Untagged: orchestrator still manages (paper / before on_open).
        None => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    DoubleBook(String),
    HeatDesk { open: usize, max: usize },
    HeatSid { sid: i32, open: usize, max: usize },
}

impl SkipReason {
    pub fn as_str(&self) -> String {
        match self {
            SkipReason::DoubleBook(sym) => {
                format!("desk orch: double-book {sym} — fail-closed")
            }
            SkipReason::HeatDesk { open, max } => {
                format!("desk orch: heat {open}/{max} — no new opens")
            }
            SkipReason::HeatSid { sid, open, max } => {
                format!("desk orch: S{sid} heat {open}/{max} — no new opens")
            }
        }
    }
}

/// Fail-closed: symbol already open in this desk book.
pub fn check_double_book(symbol: &str, open_symbols: &[String]) -> Result<(), SkipReason> {
    let want = symbol.to_ascii_uppercase();
    if open_symbols
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&want))
    {
        return Err(SkipReason::DoubleBook(want));
    }
    Ok(())
}

/// Fail-closed heat gates before a new EnterLong for `entry_sid`.
pub fn check_heat(entry_sid: i32, open_sids: &[i32]) -> Result<(), SkipReason> {
    let desk_max = desk_heat_max();
    let open = open_sids.len();
    if open >= desk_max {
        return Err(SkipReason::HeatDesk {
            open,
            max: desk_max,
        });
    }
    let per = desk_heat_max_per_sid();
    let sid_open = open_sids.iter().filter(|&&s| s == entry_sid).count();
    if sid_open >= per {
        return Err(SkipReason::HeatSid {
            sid: entry_sid,
            open: sid_open,
            max: per,
        });
    }
    Ok(())
}

pub fn status_line(now: f64, skip: Option<&str>) -> String {
    let owner = desk_owner_at(now);
    let hour = utc_datetime(now).hour();
    match skip {
        Some(why) if !why.is_empty() => {
            format!("desk orch: owner S{} @{:02}h UTC — {why}", owner.as_i32(), hour)
        }
        _ => format!(
            "desk orch: owner S{} @{:02}h UTC",
            owner.as_i32(),
            hour
        ),
    }
}

fn partition_by_opening_sid<'a>(
    positions: &'a [Position],
    owner_sid: i32,
) -> HashMap<i32, Vec<&'a Position>> {
    let mut map: HashMap<i32, Vec<&Position>> = HashMap::new();
    for p in positions {
        if p.side != Side::Long || p.qty <= Decimal::ZERO {
            continue;
        }
        let sid = opening_sid_or(&p.symbol, owner_sid);
        if !is_desk_sid(sid) {
            continue;
        }
        map.entry(sid).or_default().push(p);
    }
    map
}

fn open_sid_list(positions: &[Position], owner_sid: i32) -> Vec<i32> {
    positions
        .iter()
        .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
        .map(|p| opening_sid_or(&p.symbol, owner_sid))
        .filter(|&s| is_desk_sid(s))
        .collect()
}

fn open_symbol_list(positions: &[Position]) -> Vec<String> {
    positions
        .iter()
        .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
        .map(|p| p.symbol.to_ascii_uppercase())
        .collect()
}

fn non_hold(d: Decision) -> Option<Decision> {
    if d.is_hold() {
        None
    } else {
        Some(d)
    }
}

/// Manage open book by each position's opening sid (hour switch does not flatten).
fn manage_by_opening_sid(
    snapshot: &MarketSnapshot,
    positions: &[Position],
    now: f64,
    owner_sid: i32,
    momentum: Option<&MomentumParams>,
    scalp: Option<&ScalpParams>,
    trend: Option<&TrendParams>,
    scaled_one_r: &HashSet<String>,
    hour1_trail_bar: &HashMap<String, i64>,
    inherited_s4: &HashSet<String>,
) -> Vec<Decision> {
    let parts = partition_by_opening_sid(positions, owner_sid);
    let mut out = Vec::new();

    if let Some(s4_pos) = parts.get(&4) {
        let held: Vec<Position> = s4_pos.iter().map(|p| (*p).clone()).collect();
        let cont = continuation_params_for(4, momentum);
        let (d, _, _) = continuation_decisions(
            snapshot,
            &held,
            now,
            0.0, // manage path ignores scan cadence when allow_enter=false + non-empty
            &[],
            &HashMap::new(),
            Some(&cont),
            &[],
            false,
            &[],
            0.0,
            scaled_one_r,
            hour1_trail_bar,
            inherited_s4,
        );
        for dec in d {
            if !dec.is_hold() {
                out.push(dec);
            }
        }
    }

    if let Some(s2_pos) = parts.get(&2) {
        for pos in s2_pos {
            let bars = snapshot.bars_for(&pos.symbol);
            if let Some(d) = non_hold(scalp_decision(
                bars,
                Some(pos),
                &pos.symbol,
                scalp,
                Some(now),
            )) {
                out.push(d);
            }
        }
    }

    if let Some(s3_pos) = parts.get(&3) {
        for pos in s3_pos {
            let bars = snapshot.bars_for(&pos.symbol);
            if let Some(d) = non_hold(trend_decision(
                bars,
                Some(pos),
                &pos.symbol,
                trend,
                Some(now),
            )) {
                out.push(d);
            }
        }
    }

    out
}

fn filter_enter(
    decisions: Vec<Decision>,
    entry_sid: i32,
    open_syms: &[String],
    open_sids: &[i32],
) -> (Vec<Decision>, Option<String>) {
    let mut skip: Option<String> = None;
    let mut out = Vec::new();
    for d in decisions {
        if let Decision::EnterLong { symbol, .. } = &d {
            if let Err(e) = check_double_book(symbol, open_syms) {
                skip = Some(e.as_str());
                continue;
            }
            if let Err(e) = check_heat(entry_sid, open_sids) {
                skip = Some(e.as_str());
                continue;
            }
            out.push(d);
            // One new enter per tick (desk heat updates conceptually).
            break;
        } else if !d.is_hold() {
            out.push(d);
        }
    }
    (out, skip)
}

/// Entry scan for active owner only. Manage already collected separately.
fn entry_for_owner(
    owner: DeskSid,
    snapshot: &MarketSnapshot,
    // Owner-sid positions only (so S4 manage/entry isolation stays clean).
    owner_positions: &[Position],
    now: f64,
    last_scan_ts: f64,
    inflight: &[String],
    cooldowns: &HashMap<String, f64>,
    momentum: Option<&MomentumParams>,
    scalp: Option<&ScalpParams>,
    trend: Option<&TrendParams>,
    exclude: &[String],
    allow_enter: bool,
    cooldown_until: f64,
    scaled_one_r: &HashSet<String>,
    hour1_trail_bar: &HashMap<String, i64>,
    inherited_s4: &HashSet<String>,
    recent_leaders: &[String],
) -> (Vec<Decision>, f64, Vec<String>) {
    if !allow_enter {
        return (vec![], last_scan_ts, recent_leaders.to_vec());
    }
    let sid = owner.as_i32();
    match owner {
        DeskSid::Continuation => {
            let cont = continuation_params_for(sid, momentum);
            let (d, ts, leaders) = continuation_decisions(
                snapshot,
                owner_positions,
                now,
                last_scan_ts,
                inflight,
                cooldowns,
                Some(&cont),
                exclude,
                true,
                recent_leaders,
                cooldown_until,
                scaled_one_r,
                hour1_trail_bar,
                inherited_s4,
            );
            let enters: Vec<Decision> = d.into_iter().filter(|x| x.is_enter_long()).collect();
            (enters, ts, leaders)
        }
        DeskSid::Scalp | DeskSid::Trend => {
            // Flat entry scan via engine::decide (manage already done).
            let mut work = snapshot.clone();
            work.position = None;
            work.open_positions = owner_positions.to_vec();
            match crate::engine::decide(
                sid,
                &work,
                now,
                last_scan_ts,
                momentum,
                scalp,
                trend,
                None,
                exclude,
                cooldowns,
            ) {
                Ok((d, ts)) if d.is_enter_long() => (vec![d], ts, recent_leaders.to_vec()),
                Ok((d, ts)) => {
                    // Surface hold reason for TUI visibility.
                    (vec![d], ts, recent_leaders.to_vec())
                }
                Err(e) => (vec![Decision::hold(e)], last_scan_ts, recent_leaders.to_vec()),
            }
        }
    }
}

/// Orchestrated tick body (called from `tick_decisions` when flag on).
///
/// - `state.strategy_id` becomes active owner (entries + journal tagging).
/// - Manage/exit/trail use opening sid from open_meta.
/// - Heat + double-book fail-closed on EnterLong.
pub fn tick_orchestrated(
    state: &EngineState,
    snapshot: &MarketSnapshot,
    now: f64,
    momentum: Option<&MomentumParams>,
    scalp: Option<&ScalpParams>,
    trend: Option<&TrendParams>,
    merged_list: Vec<Position>,
    mut inflight: Vec<String>,
    mut cooldowns: HashMap<String, f64>,
    mut cooldown_until: f64,
    entries_paused: bool,
    pause_sec: f64,
    loss_windows: &[crate::sessions::HourWindow],
) -> (EngineState, Vec<Decision>) {
    let owner = desk_owner_at(now);
    let owner_sid = owner.as_i32();
    let mut state = state.clone();
    // Soft switch: do not clear cooldowns / scaled latches (hour rotate must keep manage).
    state.strategy_id = owner_sid;

    let remembered = models::remembered_positions(state.position.as_ref(), &state.positions);
    let prev_syms: HashSet<String> = remembered
        .iter()
        .map(|p| p.symbol.to_ascii_uppercase())
        .collect();
    let now_syms: HashSet<String> = merged_list
        .iter()
        .map(|p| p.symbol.to_ascii_uppercase())
        .collect();

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
        let close_sid = opening_sid_or(symbol, owner_sid);
        let sid_pause = match close_sid {
            2 => scalp.map(|s| s.cooldown_sec).unwrap_or(1200.0),
            3 => trend.map(|t| t.cooldown_sec).unwrap_or(3600.0),
            _ => pause_sec,
        };
        if sid_pause > 0.0 {
            let until =
                crate::journal::symbol_cooldown_until_for(close_sid, now, won, sid_pause);
            let key = symbol.to_ascii_uppercase();
            let cur = cooldowns.get(&key).copied().unwrap_or(0.0);
            cooldowns.insert(key, cur.max(until));
            if !won {
                let until = crate::sessions::pause_until_after_loss(now, loss_windows, sid_pause);
                cooldown_until = cooldown_until.max(until);
            }
        }
    }

    for pos in &merged_list {
        if pos.side != Side::Long || pos.qty <= Decimal::ZERO {
            continue;
        }
        let key = pos.symbol.to_ascii_uppercase();
        let sid = opening_sid_or(&pos.symbol, owner_sid);
        if let Some(sl) = pos.stop_loss {
            if sl >= pos.entry_price {
                state.scaled_one_r.insert(key.clone());
                continue;
            }
        }
        if openmeta::meta_scaled_for_entry(&key, pos.entry_price, sid) {
            state.scaled_one_r.insert(key);
        }
    }

    let now_flat = merged_list.is_empty();
    let open_syms = open_symbol_list(&merged_list);
    let open_sids = open_sid_list(&merged_list, owner_sid);

    let mut skip_reason: Option<String> = None;
    let mut decisions: Vec<Decision> = Vec::new();
    let mut scan_ts = state.last_scan_ts;
    let mut next_leaders = state.recent_leaders.clone();

    if entries_paused {
        decisions = vec![Decision::hold(status_line(
            now,
            Some("вход на паузе после закрытия всех"),
        ))];
    } else if inflight == ["*".to_string()] && now_flat {
        decisions = vec![Decision::hold(status_line(now, Some("entry in flight")))];
    } else {
        // (1) Manage by opening sid — never flatten foreign sids on hour switch.
        let managed = manage_by_opening_sid(
            snapshot,
            &merged_list,
            now,
            owner_sid,
            momentum,
            scalp,
            trend,
            &state.scaled_one_r,
            &state.hour1_trail_bar,
            &state.s4_inherited,
        );
        decisions.extend(managed);

        // (2) Entry for active owner only (after heat / double-book).
        let allow_enter = !state.daily_halt;
        let heat_block = check_heat(owner_sid, &open_sids).err().map(|e| e.as_str());
        if let Some(ref why) = heat_block {
            skip_reason = Some(why.clone());
        }

        if allow_enter && heat_block.is_none() {
            let owner_positions: Vec<Position> = merged_list
                .iter()
                .filter(|p| opening_sid_or(&p.symbol, owner_sid) == owner_sid)
                .cloned()
                .collect();
            let inflight_f: Vec<String> = inflight
                .iter()
                .filter(|s| s.as_str() != "*")
                .cloned()
                .collect();
            let (entry_decs, ts, leaders) = entry_for_owner(
                owner,
                snapshot,
                &owner_positions,
                now,
                state.last_scan_ts,
                &inflight_f,
                &cooldowns,
                momentum,
                scalp,
                trend,
                &state.skip_symbols,
                true,
                cooldown_until,
                &state.scaled_one_r,
                &state.hour1_trail_bar,
                &state.s4_inherited,
                &state.recent_leaders,
            );
            scan_ts = ts;
            next_leaders = leaders;
            let (kept, skip) = filter_enter(entry_decs, owner_sid, &open_syms, &open_sids);
            if skip.is_some() {
                skip_reason = skip;
            }
            for d in kept {
                if d.is_enter_long() {
                    decisions.push(d);
                } else if decisions.is_empty() && d.is_hold() {
                    // Visible owner + strategy hold reason when flat.
                    decisions.push(Decision::hold(status_line(now, Some(d.reason()))));
                }
            }
        }

        if decisions.is_empty() {
            decisions.push(Decision::hold(status_line(
                now,
                skip_reason.as_deref(),
            )));
        } else if let Some(ref why) = skip_reason {
            // Annotate a pure-hold tip so TUI shows skip even when manage ran.
            if decisions.iter().all(|d| d.is_hold()) {
                decisions = vec![Decision::hold(status_line(now, Some(why)))];
            }
        }
    }

    if state.daily_halt {
        let kept: Vec<Decision> = decisions
            .into_iter()
            .filter(|d| !d.is_enter_long())
            .collect();
        decisions = if kept.is_empty() {
            vec![Decision::hold(status_line(
                now,
                Some("стоп дня. Новых входов нет до 00:00 UTC."),
            ))]
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
            vec![Decision::hold(status_line(
                now,
                Some("сеть: повтор входа после сбоя"),
            ))]
        };
    }

    for decision in &decisions {
        if let Decision::EnterLong { symbol, .. } = decision {
            if !inflight.iter().any(|s| s.eq_ignore_ascii_case(symbol)) {
                inflight.push(symbol.clone());
            }
        }
        if let Decision::ReduceLong { symbol, .. } = decision {
            let key = symbol.to_ascii_uppercase();
            state.scaled_one_r.insert(key.clone());
            openmeta::mark_scaled(&key);
        }
        if let Decision::AmendStop { symbol, reason, .. } = decision {
            if owner_sid == 5 && reason.contains("trail по минимуму") {
                if let Some(bar) = models::last_closed_bar(snapshot.bars_for(symbol)) {
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
            models::push_recent(&mut actions, now, decision.describe());
        }
    }
    let book: Vec<Position> = merged_list
        .into_iter()
        .filter(|p| p.qty > Decimal::ZERO)
        .collect();
    let mut last_error = if let Some(s) = state.last_error.as_deref() {
        if crate::errors::is_retry_error(Some(s)) && now >= state.retry_until {
            None
        } else {
            Some(s.to_string())
        }
    } else {
        None
    };
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
        strategy_id: owner_sid,
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
    (new_state, decisions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desk_schedule::{desk_owner, DeskSid};

    #[test]
    fn owner_reuses_desk_schedule_clock() {
        assert_eq!(desk_owner(0), DeskSid::Continuation);
        assert_eq!(desk_owner(3), DeskSid::Scalp);
        assert_eq!(desk_owner(22), DeskSid::Trend);
        // orchestrator must not invent a second table
        assert_eq!(desk_owner(14).as_i32(), 4);
        assert_eq!(desk_owner(11).as_i32(), 2);
    }

    #[test]
    fn flag_off_by_default() {
        let _g = ENV_LOCK.lock().unwrap();
        env::remove_var("DESK_ORCHESTRATOR");
        assert!(!desk_orchestrator_enabled());
        env::set_var("DESK_ORCHESTRATOR", "1");
        assert!(desk_orchestrator_enabled());
        env::remove_var("DESK_ORCHESTRATOR");
    }

    #[test]
    fn double_book_fail_closed() {
        let open = vec!["BTCUSDT".into(), "ETHUSDT".into()];
        assert!(check_double_book("SOLUSDT", &open).is_ok());
        assert!(matches!(
            check_double_book("btcusdt", &open),
            Err(SkipReason::DoubleBook(_))
        ));
    }

    #[test]
    fn heat_desk_and_per_sid() {
        let _g = ENV_LOCK.lock().unwrap();
        env::remove_var("DESK_HEAT_MAX");
        env::remove_var("DESK_HEAT_MAX_PER_SID");
        // defaults 3 / 2
        assert!(check_heat(4, &[2, 3]).is_ok());
        assert!(matches!(
            check_heat(4, &[2, 3, 4]),
            Err(SkipReason::HeatDesk { .. })
        ));
        assert!(matches!(
            check_heat(4, &[4, 4]),
            Err(SkipReason::HeatSid { sid: 4, .. })
        ));
    }

    #[test]
    fn status_shows_owner() {
        // 2024-01-01 03:00 UTC → hour 3 → S2
        let now = 1_704_078_000.0_f64;
        let s = status_line(now, None);
        assert!(s.contains("owner S2"), "{s}");
        let s2 = status_line(now, Some("heat 3/3"));
        assert!(s2.contains("heat 3/3"), "{s2}");
    }

    #[test]
    fn manages_long_desk_sids_only_when_tagged() {
        // Without meta → managed (paper).
        assert!(manages_long("ZZZUSDT"));
    }

    #[test]
    fn orchestrator_owner_switch_changes_strategy_id() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("DESK_ORCHESTRATOR", "1");
        env::remove_var("DESK_SCHEDULE");

        let state = EngineState::new(4); // start as S4
        let snap = MarketSnapshot::empty(Decimal::from(10000));
        // 03:00 UTC → S2 owner
        let now = 1_704_078_000.0_f64;
        let (new_state, decisions) = crate::engine::tick_decisions(
            &state, &snap, now, None, None, None, None,
        );
        assert_eq!(new_state.strategy_id, 2, "owner switch to S2");
        let reason = decisions.first().map(|d| d.reason().to_string()).unwrap_or_default();
        assert!(
            reason.contains("desk orch: owner S2") || reason.contains("owner S2"),
            "visible owner in hold: {reason}"
        );

        // 00:30 UTC → S4
        let now_s4 = 1_704_067_800.0_f64; // 2024-01-01 00:30 UTC approx
        let (st2, d2) = crate::engine::tick_decisions(
            &new_state, &snap, now_s4, None, None, None, None,
        );
        assert_eq!(st2.strategy_id, 4, "owner switch to S4");
        let r2 = d2.first().map(|d| d.reason().to_string()).unwrap_or_default();
        assert!(r2.contains("owner S4") || r2.contains("S4"), "visible S4: {r2}");

        env::remove_var("DESK_ORCHESTRATOR");
    }

    #[test]
    fn flag_off_leaves_strategy_id_unchanged() {
        let _g = ENV_LOCK.lock().unwrap();
        env::remove_var("DESK_ORCHESTRATOR");
        env::remove_var("DESK_SCHEDULE");
        let state = EngineState::new(4);
        let snap = MarketSnapshot::empty(Decimal::from(10000));
        let now = 1_704_078_000.0_f64; // would be S2 if orch on
        let (new_state, decisions) = crate::engine::tick_decisions(
            &state, &snap, now, None, None, None, None,
        );
        assert_eq!(new_state.strategy_id, 4);
        let reason = decisions.first().map(|d| d.reason().to_string()).unwrap_or_default();
        assert!(
            !reason.contains("desk orch:"),
            "flag off must not emit orch status: {reason}"
        );
    }

    #[test]
    fn manage_stays_on_opening_sid_across_owner_hour() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("DESK_ORCHESTRATOR", "1");
        let dir = tempfile::tempdir().unwrap();
        openmeta::set_active_path(Some(dir.path().join("open_meta.json")));
        // Seed open_meta as S4 on ETH while hour owner is S2.
        openmeta::on_open(
            4,
            "ETHUSDT",
            Decimal::from(2000),
            Decimal::from(1900),
            Decimal::from(1),
            1_704_070_000.0,
            None,
        );
        let mut state = EngineState::new(2);
        let pos = Position::long(
            "ETHUSDT",
            Decimal::from(1),
            Decimal::from(2000),
            Some(Decimal::from(1900)),
            Some(Decimal::from(2200)),
        );
        state.positions = vec![pos.clone()];
        state.position = Some(pos.clone());
        let mut snap = MarketSnapshot::empty(Decimal::from(10000));
        snap.position = Some(pos.clone());
        snap.open_positions = vec![pos];
        // S2 hour — must NOT flatten / drop S4-tagged ETH from book
        let now = 1_704_078_000.0_f64;
        let (new_state, decisions) = crate::engine::tick_decisions(
            &state, &snap, now, None, None, None, None,
        );
        assert_eq!(new_state.strategy_id, 2);
        assert!(
            new_state
                .positions
                .iter()
                .any(|p| p.symbol.eq_ignore_ascii_case("ETHUSDT")),
            "S4 position must survive S2 owner hour"
        );
        // No Exit forced solely because owner changed
        assert!(
            !decisions.iter().any(|d| matches!(d, Decision::ExitPosition { symbol, .. } if symbol.eq_ignore_ascii_case("ETHUSDT"))),
            "hour switch must not exit opening-sid position: {decisions:?}"
        );
        openmeta::remove("ETHUSDT");
        openmeta::set_active_path(None);
        env::remove_var("DESK_ORCHESTRATOR");
    }

    #[test]
    fn filter_enter_rejects_double_book() {
        let open = vec!["ETHUSDT".into()];
        let decs = vec![Decision::EnterLong {
            symbol: "ETHUSDT".into(),
            reason: "x".into(),
            take_profit: Decimal::from(1),
            stop_loss: Decimal::from(1),
        }];
        let (kept, skip) = filter_enter(decs, 4, &open, &[2]);
        assert!(kept.is_empty());
        assert!(skip.unwrap().contains("double-book"));
    }
}

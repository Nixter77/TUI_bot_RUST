//! Append-only trade journal. JSONL under .state/; never raises into the TUI.

use crate::errors::{loss_symbol_cooldown_sec, COOLDOWN_SEC};
use crate::models::{EngineState, Position};
use crate::money::{dec, fmt_fixed, long_pnl as money_long_pnl, taker_fee as money_taker_fee};

pub use crate::money::{long_pnl, round_trip_taker_pct, taker_fee};
use crate::sessions::{pause_until_after_loss, HourWindow, DEFAULT_ENTRY_WINDOWS};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const DEFAULT_JOURNAL_PATH: &str = ".state/trades.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TradeEvent {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub strategy_id: i32,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub pnl: Option<String>,
    #[serde(default)]
    pub fee: Option<String>,
    /// Funding paid/received over the hold (USDT). Optional — soak cost, not WR.
    #[serde(default)]
    pub funding: Option<String>,
    #[serde(default)]
    pub stop_loss: Option<String>,
    #[serde(default)]
    pub take_profit: Option<String>,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub leverage: Option<String>,
    #[serde(default)]
    pub notional: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    // Phase 1 analytics (optional — old JSONL lines still parse).
    #[serde(default)]
    pub initial_risk_usdt: Option<String>,
    #[serde(default)]
    pub initial_r: Option<String>,
    #[serde(default)]
    pub final_r: Option<String>,
    #[serde(default)]
    pub mfe_r: Option<String>,
    #[serde(default)]
    pub mae_r: Option<String>,
    #[serde(default)]
    pub mfe_usdt: Option<String>,
    #[serde(default)]
    pub mae_usdt: Option<String>,
    #[serde(default)]
    pub hold_sec: Option<i64>,
    #[serde(default)]
    pub time_to_mfe_sec: Option<i64>,
    #[serde(default)]
    pub time_to_1r_sec: Option<i64>,
    #[serde(default)]
    pub scaled_at_1r: Option<bool>,
    // Entry snapshot (on open / entry_snapshot).
    #[serde(default)]
    pub ret_24h: Option<String>,
    #[serde(default)]
    pub ret_1h: Option<String>,
    #[serde(default)]
    pub ret_4h: Option<String>,
    #[serde(default)]
    pub quote_volume: Option<String>,
    #[serde(default)]
    pub near_high_frac: Option<String>,
    #[serde(default)]
    pub pullback_pct: Option<String>,
    #[serde(default)]
    pub stop_distance: Option<String>,
    #[serde(default)]
    pub risk_pct: Option<String>,
    #[serde(default)]
    pub btc_ret_1h: Option<String>,
    #[serde(default)]
    pub btc_regime: Option<String>,
}

fn journal_long_pnl(entry: Decimal, exit_price: Decimal, qty: Decimal) -> (Decimal, Decimal) {
    money_long_pnl(entry, exit_price, qty, money_taker_fee())
}

/// TP fill or net-green after round-trip taker. Scratch above entry that fees
/// flip red is a loss (live S5 1R flatten then same-symbol reprint).
pub fn long_close_was_win(entry: Decimal, exit_px: Decimal, take_profit: Option<Decimal>) -> bool {
    if let Some(tp) = take_profit {
        if exit_px >= tp {
            return true;
        }
    }
    if entry <= Decimal::ZERO || exit_px <= entry {
        return false;
    }
    let (pnl, _) = journal_long_pnl(entry, exit_px, Decimal::ONE);
    pnl > Decimal::ZERO
}

pub struct TradeJournal {
    pub path: PathBuf,
}

impl TradeJournal {
    pub fn new(path: Option<&Path>) -> Self {
        Self {
            path: path
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_JOURNAL_PATH)),
        }
    }

    pub fn append(&self, event: &TradeEvent) {
        let json = match serde_json::to_string(event) {
            Ok(j) => j,
            Err(e) => {
                set_last_error(format!("journal serialize: {e}"));
                return;
            }
        };
        let io_err = {
            let _io = lock_poison(&JOURNAL_IO);
            if let Some(parent) = self.path.parent() {
                crate::errors::ensure_private_dir(parent);
            }
            match OpenOptions::new().create(true).append(true).open(&self.path) {
                Ok(mut f) => {
                    crate::errors::restrict_private_file(&self.path);
                    let line = format!("{json}\n");
                    f.write_all(line.as_bytes())
                        .and_then(|_| f.flush())
                        .err()
                        .map(|e| format!("journal write: {e}"))
                }
                Err(e) => Some(format!("journal open: {e}")),
            }
        };
        if let Some(e) = io_err {
            set_last_error(e);
        }
    }

    pub fn read_events(&self) -> Vec<TradeEvent> {
        let _io = lock_poison(&JOURNAL_IO);
        let Ok(text) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<TradeEvent>(line) {
                out.push(ev);
            }
        }
        out
    }
}

pub fn parse_pnl(raw: Option<&str>) -> Option<Decimal> {
    raw.and_then(|s| dec(s).ok())
}

pub fn fmt_dec(value: Decimal) -> String {
    fmt_fixed(value)
}

static ACTIVE: Mutex<Option<PathBuf>> = Mutex::new(None);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
/// Serializes in-process journal read/write so JSONL lines cannot tear.
static JOURNAL_IO: Mutex<()> = Mutex::new(());

fn lock_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn set_active(path: Option<PathBuf>) {
    *lock_poison(&ACTIVE) = path.clone();
    let meta_path = path.as_ref().and_then(|p| {
        p.parent().map(|dir| dir.join("open_meta.json"))
    });
    crate::openmeta::set_active_path(meta_path);
}

fn set_last_error(msg: String) {
    *lock_poison(&LAST_ERROR) = Some(msg);
}

pub fn take_last_error() -> Option<String> {
    lock_poison(&LAST_ERROR).take()
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn with_active(f: impl FnOnce(&TradeJournal)) {
    let path = lock_poison(&ACTIVE).clone();
    if let Some(path) = path {
        f(&TradeJournal::new(Some(&path)));
    }
}

impl TradeJournal {
    pub fn record_close(
        &self,
        strategy_id: i32,
        symbol: &str,
        qty: Decimal,
        entry: Decimal,
        exit_price: Decimal,
        reason: &str,
        live: bool,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
        partial: bool,
    ) {
        let strategy_id = self.opener_strategy_id(symbol, strategy_id);
        let (pnl, fee) = journal_long_pnl(entry, exit_price, qty);
        let now = crate::sessions::unix_now();
        let m = crate::openmeta::metrics_for_close(symbol, Some(pnl), now, partial);
        self.append(&TradeEvent {
            ts: iso_now(),
            event: "close".into(),
            strategy_id,
            symbol: symbol.into(),
            qty: format!("{qty}"),
            price: format!("{exit_price}"),
            reason: reason.into(),
            pnl: Some(format!("{pnl}")),
            fee: Some(format!("{fee}")),
            stop_loss: stop_loss.map(|v| format!("{v}")),
            take_profit: take_profit.map(|v| format!("{v}")),
            live,
            leverage: None,
            notional: None,
            code: None,
            initial_risk_usdt: m.initial_risk_usdt,
            initial_r: m.initial_r,
            final_r: m.final_r,
            mfe_r: m.mfe_r,
            mae_r: m.mae_r,
            mfe_usdt: m.mfe_usdt,
            mae_usdt: m.mae_usdt,
            hold_sec: m.hold_sec,
            time_to_mfe_sec: m.time_to_mfe_sec,
            time_to_1r_sec: m.time_to_1r_sec,
            scaled_at_1r: m.scaled_at_1r,
            btc_regime: m.btc_regime,
            ..TradeEvent::default()
        });
    }

    pub fn record_open(
        &self,
        strategy_id: i32,
        symbol: &str,
        qty: Decimal,
        price: Decimal,
        reason: &str,
        live: bool,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
        entry_snap: Option<&crate::openmeta::EntrySnapshot>,
    ) {
        let snap = entry_snap.cloned().unwrap_or_default();
        let mut ev = TradeEvent {
            ts: iso_now(),
            event: "open".into(),
            strategy_id,
            symbol: symbol.into(),
            qty: format!("{qty}"),
            price: format!("{price}"),
            reason: reason.into(),
            pnl: None,
            fee: None,
            stop_loss: stop_loss.map(|v| format!("{v}")),
            take_profit: take_profit.map(|v| format!("{v}")),
            live,
            leverage: None,
            notional: None,
            code: None,
            ret_24h: snap.ret_24h,
            ret_1h: snap.ret_1h,
            ret_4h: snap.ret_4h,
            quote_volume: snap.quote_volume,
            near_high_frac: snap.near_high_frac,
            pullback_pct: snap.pullback_pct,
            stop_distance: snap.stop_distance,
            risk_pct: snap.risk_pct,
            btc_ret_1h: snap.btc_ret_1h,
            btc_regime: snap.btc_regime,
            ..TradeEvent::default()
        };
        if let Some(sl) = stop_loss {
            if let Some(risk) = crate::openmeta::initial_risk_usdt(price, sl, qty) {
                ev.initial_risk_usdt = Some(format!("{risk}"));
                ev.initial_r = Some("1".into());
            }
        }
        self.append(&ev);
    }

    pub fn record_flatten(&self, strategy_id: i32, closed: &[String], live: bool, reason: &str) {
        let stamp = iso_now();
        for item in closed {
            let strategy_id = self.opener_strategy_id(item, strategy_id);
            self.append(&TradeEvent {
                ts: stamp.clone(),
                event: "flatten".into(),
                strategy_id,
                symbol: item.clone(),
                qty: "0".into(),
                price: "0".into(),
                reason: reason.into(),
                pnl: None,
                fee: None,
                stop_loss: None,
                take_profit: None,
                live,
                leverage: None,
                notional: None,
                code: None,
                ..TradeEvent::default()
            });
        }
    }

    /// Strategy that opened this symbol, not the lens running at close.
    fn opener_strategy_id(&self, symbol: &str, fallback: i32) -> i32 {
        opener_strategy_id_from(&self.read_events(), symbol, fallback)
    }
}

/// Prefer unmatched journal open, then open_meta, then the running lens.
pub fn opener_strategy_id_from(events: &[TradeEvent], symbol: &str, fallback: i32) -> i32 {
    let want = journal_symbol(symbol);
    if want.is_empty() {
        return fallback;
    }
    if let Some(sid) = unmatched_open_strategy_from(events, &want) {
        return sid;
    }
    if let Some(m) = crate::openmeta::get(&want) {
        if (1..=5).contains(&m.strategy_id) {
            return m.strategy_id;
        }
    }
    fallback
}

fn unmatched_open_strategy_from(events: &[TradeEvent], want: &str) -> Option<i32> {
    let mut sid: HashMap<String, (i32, Decimal)> = HashMap::new();
    for ev in events {
        let symbol = journal_symbol(&ev.symbol);
        if symbol.is_empty() {
            continue;
        }
        match ev.event.as_str() {
            "open" => {
                let qty = dec(&ev.qty).unwrap_or(Decimal::ZERO);
                if qty > Decimal::ZERO && (1..=5).contains(&ev.strategy_id) {
                    sid.insert(symbol, (ev.strategy_id, qty));
                }
            }
            "close" => {
                let close_qty = dec(&ev.qty).unwrap_or(Decimal::ZERO);
                let keep_partial = sid
                    .get(&symbol)
                    .is_some_and(|(_, q)| close_qty > Decimal::ZERO && close_qty < *q);
                if keep_partial {
                    if let Some((_, q)) = sid.get_mut(&symbol) {
                        *q -= close_qty;
                    }
                } else {
                    sid.remove(&symbol);
                }
            }
            "flatten" => {
                sid.remove(&symbol);
            }
            _ => {}
        }
    }
    sid.get(want).map(|(s, _)| *s)
}

pub fn record_close(
    strategy_id: i32,
    symbol: &str,
    qty: Decimal,
    entry: Decimal,
    exit_price: Decimal,
    reason: &str,
    live: bool,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
    partial: bool,
) {
    with_active(|j| {
        j.record_close(
            strategy_id,
            symbol,
            qty,
            entry,
            exit_price,
            reason,
            live,
            stop_loss,
            take_profit,
            partial,
        )
    });
}

pub fn record_open(
    strategy_id: i32,
    symbol: &str,
    qty: Decimal,
    price: Decimal,
    reason: &str,
    live: bool,
    stop_loss: Option<Decimal>,
    take_profit: Option<Decimal>,
    entry_snap: Option<&crate::openmeta::EntrySnapshot>,
) {
    with_active(|j| {
        j.record_open(
            strategy_id,
            symbol,
            qty,
            price,
            reason,
            live,
            stop_loss,
            take_profit,
            entry_snap,
        )
    });
}

pub fn record_flatten(strategy_id: i32, closed: &[String], live: bool, reason: &str) {
    with_active(|j| j.record_flatten(strategy_id, closed, live, reason));
}

pub fn record_amend(
    strategy_id: i32,
    symbol: &str,
    stop_loss: Decimal,
    take_profit: Option<Decimal>,
    live: bool,
    reason: &str,
) {
    with_active(|j| {
        j.append(&TradeEvent {
            ts: iso_now(),
            event: "amend".into(),
            strategy_id,
            symbol: symbol.into(),
            qty: String::new(),
            price: String::new(),
            reason: reason.into(),
            pnl: None,
            fee: None,
            stop_loss: Some(format!("{stop_loss}")),
            take_profit: take_profit.map(|v| format!("{v}")),
            live,
            leverage: None,
            notional: None,
            code: None,
            ..TradeEvent::default()
        })
    });
}

/// `SHORT BTCUSDT` / `LONG ETHUSDT` / `SUPERUSDT` → `SUPERUSDT`.
pub fn journal_symbol(raw: &str) -> String {
    let upper = raw.trim().to_ascii_uppercase();
    upper
        .strip_prefix("SHORT ")
        .or_else(|| upper.strip_prefix("LONG "))
        .unwrap_or(&upper)
        .trim()
        .to_string()
}

pub fn event_unix(ts: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|d| d.timestamp() as f64)
}

/// Pause for this symbol after a close. Losses sit out 12h so a loser
/// skips the next UTC session window; wins keep the base pause. S5 is 24h.
pub fn symbol_pause_sec(won: bool, pause_sec: f64) -> f64 {
    symbol_pause_sec_for(0, won, pause_sec)
}

pub fn symbol_pause_sec_for(strategy_id: i32, won: bool, pause_sec: f64) -> f64 {
    if pause_sec <= 0.0 {
        return 0.0;
    }
    if won {
        pause_sec
    } else {
        pause_sec.max(loss_symbol_cooldown_sec(strategy_id))
    }
}

pub fn symbol_cooldown_until(now: f64, won: bool, pause_sec: f64) -> f64 {
    symbol_cooldown_until_for(0, now, won, pause_sec)
}

pub fn symbol_cooldown_until_for(strategy_id: i32, now: f64, won: bool, pause_sec: f64) -> f64 {
    now + symbol_pause_sec_for(strategy_id, won, pause_sec)
}

/// After a close/flatten, keep the name off the buy list.
/// Restarts otherwise re-buy the same SL tape (SUPERUSDT three times in 15m).
pub fn cooldowns_from_events(events: &[TradeEvent], now: f64, pause_sec: f64) -> HashMap<String, f64> {
    cooldowns_from_events_for(events, now, pause_sec, None)
}

/// When `strategy_id` is set, only that strategy's closes seed cooldowns (S4 must not block S5).
pub fn cooldowns_from_events_for(
    events: &[TradeEvent],
    now: f64,
    pause_sec: f64,
    strategy_id: Option<i32>,
) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    if pause_sec <= 0.0 {
        return out;
    }
    for ev in events {
        if ev.event != "close" && ev.event != "flatten" {
            continue;
        }
        if let Some(sid) = strategy_id {
            if ev.strategy_id != sid {
                continue;
            }
        }
        let Some(ts) = event_unix(&ev.ts) else {
            continue;
        };
        let won = parse_pnl(ev.pnl.as_deref()).is_some_and(|p| p > Decimal::ZERO);
        let wait = if ev.event == "flatten" {
            pause_sec
        } else {
            symbol_pause_sec_for(strategy_id.unwrap_or(0), won, pause_sec)
        };
        let until = ts + wait;
        if until <= now {
            continue;
        }
        let symbol = journal_symbol(&ev.symbol);
        if symbol.is_empty() {
            continue;
        }
        let cur = out.get(&symbol).copied().unwrap_or(0.0);
        out.insert(symbol, cur.max(until));
    }
    out
}

pub fn desk_cooldown_from_events(events: &[TradeEvent], now: f64, pause_sec: f64) -> f64 {
    desk_cooldown_from_events_windows(events, now, pause_sec, &DEFAULT_ENTRY_WINDOWS)
}

pub fn desk_cooldown_from_events_windows(
    events: &[TradeEvent],
    now: f64,
    pause_sec: f64,
    windows: &[HourWindow],
) -> f64 {
    desk_cooldown_from_events_windows_for(events, now, pause_sec, windows, None)
}

pub fn desk_cooldown_from_events_windows_for(
    events: &[TradeEvent],
    now: f64,
    pause_sec: f64,
    windows: &[HourWindow],
    strategy_id: Option<i32>,
) -> f64 {
    if pause_sec <= 0.0 {
        return 0.0;
    }
    let mut until: f64 = 0.0;
    for ev in events {
        if ev.event != "close" {
            continue;
        }
        if let Some(sid) = strategy_id {
            if ev.strategy_id != sid {
                continue;
            }
        }
        let pnl = parse_pnl(ev.pnl.as_deref()).unwrap_or(Decimal::ZERO);
        if pnl > Decimal::ZERO {
            continue;
        }
        let Some(ts) = event_unix(&ev.ts) else {
            continue;
        };
        until = until.max(pause_until_after_loss(ts, windows, pause_sec));
    }
    if until > now {
        until
    } else {
        0.0
    }
}

/// Last unmatched `open` per symbol (no later full close/flatten).
/// A partial `close` (scale-out) keeps the remainder so a restart still
/// overlays SL/TP onto the live long. Restarts otherwise paint SL=—.
pub fn unmatched_open_positions_from(events: &[TradeEvent]) -> Vec<Position> {
    let mut by_sym: HashMap<String, Position> = HashMap::new();
    for ev in events {
        let symbol = journal_symbol(&ev.symbol);
        if symbol.is_empty() {
            continue;
        }
        match ev.event.as_str() {
            "open" => {
                let qty = dec(&ev.qty).unwrap_or(Decimal::ZERO);
                let entry = dec(&ev.price).unwrap_or(Decimal::ZERO);
                if qty <= Decimal::ZERO || entry <= Decimal::ZERO {
                    continue;
                }
                let sl = ev
                    .stop_loss
                    .as_deref()
                    .and_then(|s| dec(s).ok())
                    .filter(|v| *v > Decimal::ZERO);
                let tp = ev
                    .take_profit
                    .as_deref()
                    .and_then(|s| dec(s).ok())
                    .filter(|v| *v > Decimal::ZERO);
                let mut pos = Position::long(symbol, qty, entry, sl, tp);
                pos.opened_bar_time = event_unix(&ev.ts).map(|t| (t * 1000.0) as i64);
                by_sym.insert(pos.symbol.clone(), pos);
            }
            "amend" => {
                let sl = ev
                    .stop_loss
                    .as_deref()
                    .and_then(|s| dec(s).ok())
                    .filter(|v| *v > Decimal::ZERO);
                if let (Some(pos), Some(sl)) = (by_sym.get_mut(&symbol), sl) {
                    pos.stop_loss = Some(sl);
                    if let Some(tp) = ev
                        .take_profit
                        .as_deref()
                        .and_then(|s| dec(s).ok())
                        .filter(|v| *v > Decimal::ZERO)
                    {
                        pos.take_profit = Some(tp);
                    }
                }
            }
            "close" => {
                // Scale-out records a partial close. Empty/zero/oversize qty is a
                // full close (legacy lines and flatten-style exits).
                let close_qty = dec(&ev.qty).unwrap_or(Decimal::ZERO);
                let keep_partial = by_sym
                    .get(&symbol)
                    .is_some_and(|p| close_qty > Decimal::ZERO && close_qty < p.qty);
                if keep_partial {
                    if let Some(pos) = by_sym.get_mut(&symbol) {
                        pos.qty -= close_qty;
                    }
                } else {
                    by_sym.remove(&symbol);
                }
            }
            "flatten" => {
                by_sym.remove(&symbol);
            }
            _ => {}
        }
    }
    by_sym.into_values().collect()
}

pub fn unmatched_open_positions() -> Vec<Position> {
    unmatched_open_positions_for(None)
}

/// Unmatched opens for one strategy lens (S4 journal must not seed S5 overlays).
pub fn unmatched_open_positions_for(strategy_id: Option<i32>) -> Vec<Position> {
    let path = lock_poison(&ACTIVE).clone();
    let Some(path) = path else {
        return Vec::new();
    };
    let events = TradeJournal::new(Some(&path)).read_events();
    unmatched_open_positions_from_for(&events, strategy_id)
}

pub fn unmatched_open_positions_from_for(
    events: &[TradeEvent],
    strategy_id: Option<i32>,
) -> Vec<Position> {
    let scoped: Vec<TradeEvent> = match strategy_id {
        Some(sid) => events
            .iter()
            .filter(|e| e.strategy_id == sid)
            .cloned()
            .collect(),
        None => events.to_vec(),
    };
    unmatched_open_positions_from(&scoped)
}

pub fn seed_cooldowns(state: &mut EngineState, now: f64, pause_sec: f64) {
    let pause = if pause_sec > 0.0 { pause_sec } else { COOLDOWN_SEC };
    let events = TradeJournal::new(Some(Path::new(DEFAULT_JOURNAL_PATH))).read_events();
    let sid = Some(state.strategy_id);
    for (sym, until) in cooldowns_from_events_for(&events, now, pause, sid) {
        let cur = state.cooldowns.get(&sym).copied().unwrap_or(0.0);
        state.cooldowns.insert(sym, cur.max(until));
    }
    let desk = desk_cooldown_from_events_windows_for(
        &events,
        now,
        pause,
        &DEFAULT_ENTRY_WINDOWS,
        sid,
    );
    if desk > state.cooldown_until {
        state.cooldown_until = desk;
    }
}

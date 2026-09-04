//! Append-only trade journal. JSONL under .state/; never raises into the TUI.

use crate::errors::{COOLDOWN_SEC, LOSS_SYMBOL_COOLDOWN_SEC};
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
}

fn journal_long_pnl(entry: Decimal, exit_price: Decimal, qty: Decimal) -> (Decimal, Decimal) {
    money_long_pnl(entry, exit_price, qty, money_taker_fee())
}

/// TP fill or price above entry. Equal-to-entry / missing mark = not a win (fail-closed).
pub fn long_close_was_win(entry: Decimal, exit_px: Decimal, take_profit: Option<Decimal>) -> bool {
    if let Some(tp) = take_profit {
        if exit_px >= tp {
            return true;
        }
    }
    entry > Decimal::ZERO && exit_px > entry
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
    *lock_poison(&ACTIVE) = path;
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
    ) {
        let (pnl, fee) = journal_long_pnl(entry, exit_price, qty);
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
    ) {
        self.append(&TradeEvent {
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
        });
    }

    pub fn record_flatten(&self, strategy_id: i32, closed: &[String], live: bool, reason: &str) {
        let stamp = iso_now();
        for item in closed {
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
            });
        }
    }
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
/// skips the next UTC session window; wins keep the base pause.
pub fn symbol_pause_sec(won: bool, pause_sec: f64) -> f64 {
    if pause_sec <= 0.0 {
        return 0.0;
    }
    if won {
        pause_sec
    } else {
        pause_sec.max(LOSS_SYMBOL_COOLDOWN_SEC)
    }
}

pub fn symbol_cooldown_until(now: f64, won: bool, pause_sec: f64) -> f64 {
    now + symbol_pause_sec(won, pause_sec)
}

/// After a close/flatten, keep the name off the buy list.
/// Restarts otherwise re-buy the same SL tape (SUPERUSDT three times in 15m).
pub fn cooldowns_from_events(events: &[TradeEvent], now: f64, pause_sec: f64) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    if pause_sec <= 0.0 {
        return out;
    }
    for ev in events {
        if ev.event != "close" && ev.event != "flatten" {
            continue;
        }
        let Some(ts) = event_unix(&ev.ts) else {
            continue;
        };
        let won = parse_pnl(ev.pnl.as_deref()).is_some_and(|p| p > Decimal::ZERO);
        let wait = if ev.event == "flatten" {
            pause_sec
        } else {
            symbol_pause_sec(won, pause_sec)
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
    if pause_sec <= 0.0 {
        return 0.0;
    }
    let mut until: f64 = 0.0;
    for ev in events {
        if ev.event != "close" {
            continue;
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
    let path = lock_poison(&ACTIVE).clone();
    let Some(path) = path else {
        return Vec::new();
    };
    let events = TradeJournal::new(Some(&path)).read_events();
    unmatched_open_positions_from(&events)
}

pub fn seed_cooldowns(state: &mut EngineState, now: f64, pause_sec: f64) {
    let pause = if pause_sec > 0.0 { pause_sec } else { COOLDOWN_SEC };
    let events = TradeJournal::new(Some(Path::new(DEFAULT_JOURNAL_PATH))).read_events();
    for (sym, until) in cooldowns_from_events(&events, now, pause) {
        let cur = state.cooldowns.get(&sym).copied().unwrap_or(0.0);
        state.cooldowns.insert(sym, cur.max(until));
    }
    let desk = desk_cooldown_from_events(&events, now, pause);
    if desk > state.cooldown_until {
        state.cooldown_until = desk;
    }
}

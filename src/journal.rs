//! Append-only trade journal. JSONL under .state/; never raises into the TUI.

use crate::errors::COOLDOWN_SEC;
use crate::models::EngineState;
use crate::money::{dec, fmt_fixed};
use crate::sessions::{pause_until_after_loss, HourWindow, DEFAULT_ENTRY_WINDOWS};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const DEFAULT_JOURNAL_PATH: &str = ".state/trades.jsonl";
pub fn taker_fee() -> Decimal {
    Decimal::new(4, 4) // 0.0004 Binance USDT-M taker, one side
}

pub fn round_trip_taker_pct() -> Decimal {
    taker_fee() + taker_fee()
}

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

pub fn long_pnl(entry: Decimal, exit_price: Decimal, qty: Decimal, fee_rate: Decimal) -> (Decimal, Decimal) {
    let fee = (entry + exit_price) * qty * fee_rate;
    ((exit_price - entry) * qty - fee, fee)
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
        let Ok(json) = serde_json::to_string(event) else {
            return;
        };
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{json}");
        }
    }

    pub fn read_events(&self) -> Vec<TradeEvent> {
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

pub fn set_active(path: Option<PathBuf>) {
    if let Ok(mut guard) = ACTIVE.lock() {
        *guard = path;
    }
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn with_active(f: impl FnOnce(&TradeJournal)) {
    let path = ACTIVE.lock().ok().and_then(|g| g.clone());
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
    ) {
        let (pnl, fee) = long_pnl(entry, exit_price, qty, taker_fee());
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
            stop_loss: None,
            take_profit: None,
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
) {
    with_active(|j| j.record_close(strategy_id, symbol, qty, entry, exit_price, reason, live));
}

pub fn record_flatten(strategy_id: i32, closed: &[String], live: bool, reason: &str) {
    with_active(|j| j.record_flatten(strategy_id, closed, live, reason));
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

/// After a close/flatten, keep the name off the buy list for `pause_sec`.
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
        let until = ts + pause_sec;
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

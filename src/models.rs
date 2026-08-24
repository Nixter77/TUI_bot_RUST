//! Shared market, account, and decision types.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Long,
    Short,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Long => "LONG",
            Side::Short => "SHORT",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_uppercase().as_str() {
            "LONG" | "BUY" => Some(Side::Long),
            "SHORT" | "SELL" => Some(Side::Short),
            _ => None,
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticker {
    pub symbol: String,
    pub last_price: Decimal,
    pub price_change_percent: Decimal,
    pub quote_volume: Decimal,
    pub high_price: Decimal,
    pub low_price: Decimal,
    pub week_change_percent: Decimal,
}

impl Ticker {
    pub fn new(
        symbol: impl Into<String>,
        last_price: Decimal,
        price_change_percent: Decimal,
        quote_volume: Decimal,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            last_price,
            price_change_percent,
            quote_volume,
            high_price: Decimal::ZERO,
            low_price: Decimal::ZERO,
            week_change_percent: Decimal::ZERO,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    pub open_time: i64,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

pub fn last_closed_bar(bars: &[Bar]) -> Option<&Bar> {
    if bars.len() >= 2 {
        Some(&bars[bars.len() - 2])
    } else {
        bars.first()
    }
}

pub fn bar_is_red(bar: Option<&Bar>) -> bool {
    bar.map(|b| b.close < b.open).unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub symbol: String,
    pub side: Side,
    pub qty: Decimal,
    pub entry_price: Decimal,
    pub stop_loss: Option<Decimal>,
    pub take_profit: Option<Decimal>,
    pub unrealized_pnl: Decimal,
    pub opened_bar_time: Option<i64>,
    pub leverage: i32,
}

impl Position {
    pub fn long(
        symbol: impl Into<String>,
        qty: Decimal,
        entry: Decimal,
        stop_loss: Option<Decimal>,
        take_profit: Option<Decimal>,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            side: Side::Long,
            qty,
            entry_price: entry,
            stop_loss,
            take_profit,
            unrealized_pnl: Decimal::ZERO,
            opened_bar_time: None,
            leverage: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub wallet_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub available_balance: Decimal,
    pub starting_equity: Decimal,
}

impl Account {
    pub fn equity(&self) -> Decimal {
        self.wallet_balance + self.unrealized_pnl
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Hold {
        reason: String,
    },
    EnterLong {
        symbol: String,
        reason: String,
        take_profit: Decimal,
        stop_loss: Decimal,
    },
    ExitPosition {
        reason: String,
        symbol: String,
    },
    AmendStop {
        stop_loss: Decimal,
        reason: String,
        symbol: String,
    },
}

impl Decision {
    pub fn hold(reason: impl Into<String>) -> Self {
        Decision::Hold {
            reason: reason.into(),
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Decision::Hold { reason }
            | Decision::EnterLong { reason, .. }
            | Decision::ExitPosition { reason, .. }
            | Decision::AmendStop { reason, .. } => reason,
        }
    }

    pub fn symbol(&self) -> &str {
        match self {
            Decision::Hold { .. } => "",
            Decision::EnterLong { symbol, .. }
            | Decision::ExitPosition { symbol, .. }
            | Decision::AmendStop { symbol, .. } => symbol,
        }
    }

    pub fn is_hold(&self) -> bool {
        matches!(self, Decision::Hold { .. })
    }

    pub fn is_enter_long(&self) -> bool {
        matches!(self, Decision::EnterLong { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketSnapshot {
    pub tickers: Vec<Ticker>,
    pub bars: Vec<Bar>,
    pub account: Account,
    pub position: Option<Position>,
    pub chart_symbol: String,
    pub fetched: bool,
    pub last_error: Option<String>,
    pub live_book: bool,
    pub open_positions: Vec<Position>,
    pub account_ok: bool,
    pub account_fresh: bool,
    pub last_bars: HashMap<String, Bar>,
    pub universe_bars: HashMap<String, Vec<Bar>>,
}

impl MarketSnapshot {
    pub fn empty(starting: Decimal) -> Self {
        Self {
            tickers: Vec::new(),
            bars: Vec::new(),
            account: Account {
                wallet_balance: Decimal::ZERO,
                unrealized_pnl: Decimal::ZERO,
                available_balance: Decimal::ZERO,
                starting_equity: starting,
            },
            position: None,
            chart_symbol: "BTCUSDT".into(),
            fetched: false,
            last_error: None,
            live_book: false,
            open_positions: Vec::new(),
            account_ok: false,
            account_fresh: false,
            last_bars: HashMap::new(),
            universe_bars: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineState {
    pub strategy_id: i32,
    pub last_scan_ts: f64,
    pub position: Option<Position>,
    pub last_error: Option<String>,
    pub recent_actions: Vec<String>,
    pub entry_inflight: bool,
    pub entries_paused: bool,
    pub cooldown_until: f64,
    pub positions: Vec<Position>,
    pub inflight_symbols: Vec<String>,
    pub cooldowns: HashMap<String, f64>,
    pub skip_symbols: Vec<String>,
    pub skip_reasons: HashMap<String, String>,
    pub day_utc: String,
    pub day_start_equity: Option<Decimal>,
    pub daily_halt: bool,
    pub sized_stops: HashSet<String>,
    pub recent_leaders: Vec<String>,
}

impl EngineState {
    pub fn new(strategy_id: i32) -> Self {
        Self {
            strategy_id,
            last_scan_ts: 0.0,
            position: None,
            last_error: None,
            recent_actions: Vec::new(),
            entry_inflight: false,
            entries_paused: false,
            cooldown_until: 0.0,
            positions: Vec::new(),
            inflight_symbols: Vec::new(),
            cooldowns: HashMap::new(),
            skip_symbols: Vec::new(),
            skip_reasons: HashMap::new(),
            day_utc: String::new(),
            day_start_equity: None,
            daily_halt: false,
            sized_stops: HashSet::new(),
            recent_leaders: Vec::new(),
        }
    }
}

pub fn coalesce_position(live: Option<&Position>, remembered: Option<&Position>) -> Option<Position> {
    let live = live?;
    let Some(remembered) = remembered else {
        return Some(live.clone());
    };
    if remembered.symbol != live.symbol {
        return Some(live.clone());
    }
    Some(Position {
        symbol: live.symbol.clone(),
        side: live.side,
        qty: if live.qty > Decimal::ZERO {
            live.qty
        } else {
            remembered.qty
        },
        entry_price: if live.entry_price > Decimal::ZERO {
            live.entry_price
        } else {
            remembered.entry_price
        },
        stop_loss: live.stop_loss.or(remembered.stop_loss),
        take_profit: live.take_profit.or(remembered.take_profit),
        unrealized_pnl: live.unrealized_pnl,
        opened_bar_time: live.opened_bar_time.or(remembered.opened_bar_time),
        leverage: if live.leverage != 0 {
            live.leverage
        } else {
            remembered.leverage
        },
    })
}

pub fn pick_managed_long(positions: &[Position], remembered: Option<&Position>) -> Option<Position> {
    let longs: Vec<&Position> = positions
        .iter()
        .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
        .collect();
    if longs.is_empty() {
        return None;
    }
    if let Some(rem) = remembered {
        if rem.side == Side::Long {
            if let Some(live) = longs.iter().find(|p| p.symbol == rem.symbol) {
                return coalesce_position(Some(live), Some(rem));
            }
        }
    }
    coalesce_position(Some(longs[0]), remembered)
}

pub fn pick_managed_longs(positions: &[Position], remembered: &[Position]) -> Vec<Position> {
    let mem: HashMap<&str, &Position> = remembered
        .iter()
        .filter(|p| p.qty > Decimal::ZERO)
        .map(|p| (p.symbol.as_str(), p))
        .collect();
    positions
        .iter()
        .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
        .map(|live| {
            coalesce_position(Some(live), mem.get(live.symbol.as_str()).copied()).unwrap_or_else(|| live.clone())
        })
        .collect()
}

pub fn unmanaged_positions(open_book: &[Position], managed: &[Position]) -> Vec<Position> {
    let managed_syms: HashSet<&str> = managed
        .iter()
        .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
        .map(|p| p.symbol.as_str())
        .collect();
    open_book
        .iter()
        .filter(|pos| {
            pos.qty > Decimal::ZERO && (pos.side != Side::Long || !managed_syms.contains(pos.symbol.as_str()))
        })
        .cloned()
        .collect()
}

pub fn near_24h_high(ticker: &Ticker, frac: Decimal) -> bool {
    if ticker.high_price <= Decimal::ZERO || ticker.last_price <= Decimal::ZERO {
        return false;
    }
    ticker.last_price >= ticker.high_price * (Decimal::ONE - frac)
}

pub fn remembered_positions(position: Option<&Position>, extra: &[Position]) -> Vec<Position> {
    let mut by_sym: HashMap<String, Position> = HashMap::new();
    for pos in extra {
        if pos.qty > Decimal::ZERO {
            by_sym.insert(pos.symbol.clone(), pos.clone());
        }
    }
    if let Some(pos) = position {
        if pos.qty > Decimal::ZERO {
            by_sym.insert(pos.symbol.clone(), pos.clone());
        }
    }
    by_sym.into_values().collect()
}

pub fn ticker_from_mapping(item: &serde_json::Value) -> Result<Ticker, String> {
    let obj = item.as_object().ok_or_else(|| "ticker must be an object".to_string())?;
    let symbol = obj
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    if symbol.is_empty() {
        return Err("ticker missing symbol".into());
    }
    let last = json_dec(obj.get("lastPrice"))?;
    let pct = json_dec(obj.get("priceChangePercent"))?;
    let vol = json_dec(obj.get("quoteVolume"))?;
    let high = obj
        .get("highPrice")
        .or_else(|| obj.get("high"))
        .and_then(|v| json_dec(Some(v)).ok())
        .unwrap_or(Decimal::ZERO);
    let low = obj
        .get("lowPrice")
        .or_else(|| obj.get("low"))
        .and_then(|v| json_dec(Some(v)).ok())
        .unwrap_or(Decimal::ZERO);
    Ok(Ticker {
        symbol,
        last_price: last,
        price_change_percent: pct,
        quote_volume: vol,
        high_price: high,
        low_price: low,
        week_change_percent: Decimal::ZERO,
    })
}

pub fn bar_from_kline(row: &serde_json::Value) -> Result<Bar, String> {
    let arr = row.as_array().ok_or_else(|| "kline row too short".to_string())?;
    if arr.len() < 6 {
        return Err("kline row too short".into());
    }
    let open_time = json_i64(&arr[0])?;
    Ok(Bar {
        open_time,
        open: json_dec(Some(&arr[1]))?,
        high: json_dec(Some(&arr[2]))?,
        low: json_dec(Some(&arr[3]))?,
        close: json_dec(Some(&arr[4]))?,
        volume: json_dec(Some(&arr[5]))?,
    })
}

fn json_dec(v: Option<&serde_json::Value>) -> Result<Decimal, String> {
    let v = v.ok_or_else(|| "missing numeric value".to_string())?;
    match v {
        serde_json::Value::String(s) => crate::money::dec(s).map_err(|e| e.to_string()),
        serde_json::Value::Number(n) => crate::money::dec(&n.to_string()).map_err(|e| e.to_string()),
        _ => Err("not a decimal".into()),
    }
}

fn json_i64(v: &serde_json::Value) -> Result<i64, String> {
    match v {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| u as i64))
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| "not an int".to_string()),
        serde_json::Value::String(s) => s.parse().map_err(|_| "not an int".to_string()),
        _ => Err("not an int".into()),
    }
}

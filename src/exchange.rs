//! Exchange helpers: sizing, protective-order shape, optional HTTP client.

use crate::config::Config;
use crate::errors::{is_retry_error, redact_secrets, RETRY_BACKOFF_SEC};
use crate::models::{bar_from_kline, Account, Bar, Position, Side, Ticker};
use crate::money::{dec, quantize_to_step};
use crate::profit::current_equity;
use crate::ranking::{is_tradable_symbol, parse_tickers};
use crate::signing::{canonical_query, signed_query_string, SignError};
use rust_decimal::Decimal;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct ExchangeError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolFilters {
    pub tick_size: Decimal,
    pub step_size: Decimal,
    pub min_qty: Decimal,
    pub min_notional: Decimal,
}

pub fn size_market_order(notional: Decimal, price: Decimal, filters: &SymbolFilters) -> Result<Decimal, ExchangeError> {
    if price <= Decimal::ZERO {
        return Err(ExchangeError("cannot size order at non-positive price".into()));
    }
    if notional <= Decimal::ZERO {
        return Err(ExchangeError("cannot size order at non-positive notional".into()));
    }
    let target = if notional >= filters.min_notional {
        notional
    } else {
        filters.min_notional
    };
    let mut qty = quantize_to_step(target / price, filters.step_size, false)
        .map_err(|e| ExchangeError(e.to_string()))?;
    if qty < filters.min_qty {
        qty = filters.min_qty;
    }
    if qty * price < filters.min_notional {
        qty = quantize_to_step(filters.min_notional / price, filters.step_size, true)
            .map_err(|e| ExchangeError(e.to_string()))?;
    }
    if qty < filters.min_qty {
        qty = filters.min_qty;
    }
    if qty <= Decimal::ZERO || qty * price < filters.min_notional {
        return Err(ExchangeError(format!(
            "notional below exchange minimum ({} USDT)",
            filters.min_notional
        )));
    }
    Ok(qty)
}

/// Risk-% plan: `notional = (equity * risk_pct) * entry / (entry - stop)`.
/// Leverage is not in the formula. `risk_pct == 0` is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskSizePlan {
    pub notional: Decimal,
    pub risk_usdt: Decimal,
    pub dist: Decimal,
}

/// Account equity = wallet + uPnL. Returns `None` when risk-% is off or stop is invalid.
pub fn risk_position_notional(
    equity: Decimal,
    risk_pct: Decimal,
    entry: Decimal,
    stop_loss: Decimal,
) -> Option<RiskSizePlan> {
    if risk_pct <= Decimal::ZERO || equity <= Decimal::ZERO || entry <= Decimal::ZERO {
        return None;
    }
    let dist = entry - stop_loss;
    if dist <= Decimal::ZERO {
        return None;
    }
    let risk_usdt = equity * risk_pct;
    if risk_usdt <= Decimal::ZERO {
        return None;
    }
    Some(RiskSizePlan {
        notional: risk_usdt * entry / dist,
        risk_usdt,
        dist,
    })
}

/// Size by risk, then refuse if exchange mins would spend more than `risk_usdt` at `dist`.
pub fn size_risk_market_order(
    equity: Decimal,
    risk_pct: Decimal,
    entry: Decimal,
    stop_loss: Decimal,
    filters: &SymbolFilters,
) -> Result<Option<Decimal>, ExchangeError> {
    let Some(plan) = risk_position_notional(equity, risk_pct, entry, stop_loss) else {
        return Ok(None);
    };
    let qty = size_market_order(plan.notional, entry, filters)?;
    if qty * plan.dist > plan.risk_usdt {
        return Ok(None);
    }
    Ok(Some(qty))
}

/// TestNet `/fapi/v1/algoOrder` requires `algoType=CONDITIONAL` (−1102 otherwise)
/// and `triggerPrice` instead of the legacy `stopPrice`.
pub fn prepare_algo_params(params: &mut BTreeMap<String, String>) {
    params
        .entry("algoType".into())
        .or_insert_with(|| "CONDITIONAL".into());
    if !params.contains_key("triggerPrice") {
        if let Some(stop) = params.remove("stopPrice") {
            params.insert("triggerPrice".into(), stop);
        }
    } else {
        params.remove("stopPrice");
    }
}

/// Protective SELL TP/SL: reduce-only, sized to the open long — never a naked closePosition SELL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectiveSell {
    pub side: &'static str,
    pub order_type: &'static str,
    pub quantity: Decimal,
    pub reduce_only: bool,
    pub close_position: bool,
    pub trigger_price: Decimal,
}

pub fn sized_long_protectives(qty: Decimal, stop_loss: Decimal, take_profit: Decimal) -> Result<Vec<ProtectiveSell>, ExchangeError> {
    if qty <= Decimal::ZERO {
        return Err(ExchangeError("protective qty must be positive".into()));
    }
    if stop_loss <= Decimal::ZERO || take_profit <= Decimal::ZERO || stop_loss >= take_profit {
        return Err(ExchangeError("protective prices invalid".into()));
    }
    Ok(vec![
        ProtectiveSell {
            side: "SELL",
            order_type: "STOP_MARKET",
            quantity: qty,
            reduce_only: true,
            close_position: false,
            trigger_price: stop_loss,
        },
        ProtectiveSell {
            side: "SELL",
            order_type: "TAKE_PROFIT_MARKET",
            quantity: qty,
            reduce_only: true,
            close_position: false,
            trigger_price: take_profit,
        },
    ])
}

fn flag_true(value: &Value) -> bool {
    match value {
        Value::Bool(true) => true,
        Value::String(s) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// True when open SELL algos are qty-sized TP+SL, not closePosition.
pub fn sell_protectives_are_sized(rows: &[Value]) -> bool {
    let sells: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            row.get("side")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .eq_ignore_ascii_case("SELL")
        })
        .collect();
    if sells.len() < 2 {
        return false;
    }
    if sells.iter().any(|row| flag_true(row.get("closePosition").unwrap_or(&Value::Null))) {
        return false;
    }
    let types: Vec<String> = sells
        .iter()
        .map(|row| {
            row.get("orderType")
                .or_else(|| row.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_uppercase()
        })
        .collect();
    let has_stop = types.iter().any(|t| t == "STOP_MARKET" || t == "STOP");
    let has_tp = types
        .iter()
        .any(|t| t == "TAKE_PROFIT_MARKET" || t == "TAKE_PROFIT");
    if !(has_stop && has_tp) {
        return false;
    }
    for row in &sells {
        let qty = row
            .get("quantity")
            .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| v.as_f64().map(|f| f.to_string())))
            .unwrap_or_else(|| "0".into());
        let ok = dec(&qty).map(|d| d > Decimal::ZERO).unwrap_or(false);
        if !ok {
            return false;
        }
    }
    true
}

fn json_decimal(row: &Value, keys: &[&str]) -> Option<Decimal> {
    for key in keys {
        let Some(v) = row.get(*key) else {
            continue;
        };
        let raw = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => continue,
        };
        if let Ok(d) = dec(&raw) {
            return Some(d);
        }
    }
    None
}

pub fn protective_trigger_price(row: &Value) -> Option<Decimal> {
    json_decimal(row, &["triggerPrice", "stopPrice"])
}

pub fn algo_id_of(row: &Value) -> Option<String> {
    match row.get("algoId")? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub fn order_id_of(row: &Value) -> Option<i64> {
    let v = row.get("orderId")?;
    v.as_i64()
        .or_else(|| v.as_u64().map(|n| n as i64))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectiveKind {
    Stop,
    TakeProfit,
}

pub fn sell_protective_kind(row: &Value) -> Option<ProtectiveKind> {
    let side = row
        .get("side")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    if side != "SELL" {
        return None;
    }
    let otype = row
        .get("orderType")
        .or_else(|| row.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    match otype.as_str() {
        "STOP_MARKET" | "STOP" => Some(ProtectiveKind::Stop),
        "TAKE_PROFIT_MARKET" | "TAKE_PROFIT" => Some(ProtectiveKind::TakeProfit),
        _ => None,
    }
}

/// True when this open SELL stop/TP is not the live pair (old trail left behind).
pub fn stale_sell_protective(row: &Value, stop_loss: Decimal, take_profit: Decimal) -> bool {
    match sell_protective_kind(row) {
        Some(ProtectiveKind::Stop) => protective_trigger_price(row) != Some(stop_loss),
        Some(ProtectiveKind::TakeProfit) => protective_trigger_price(row) != Some(take_profit),
        None => false,
    }
}

pub fn parse_positions(raw: &Value) -> Result<Vec<Position>, ExchangeError> {
    let list = raw
        .as_array()
        .ok_or_else(|| ExchangeError("position payload is not a list".into()))?;
    let mut out = Vec::new();
    for item in list {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let amt_raw = obj.get("positionAmt").cloned().unwrap_or(Value::Null);
        let amt_s = match amt_raw {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            _ => continue,
        };
        let Ok(amt) = dec(&amt_s) else {
            continue;
        };
        if amt == Decimal::ZERO {
            continue;
        }
        let symbol = obj
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_uppercase();
        if !is_tradable_symbol(&symbol) {
            continue;
        }
        let side = if amt > Decimal::ZERO {
            Side::Long
        } else {
            Side::Short
        };
        let leverage = obj
            .get("leverage")
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0) as i32;
        let entry = obj
            .get("entryPrice")
            .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| Some(v.to_string())))
            .and_then(|s| dec(&s).ok())
            .unwrap_or(Decimal::ZERO);
        let upnl = obj
            .get("unRealizedProfit")
            .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| Some(v.to_string())))
            .and_then(|s| dec(&s).ok())
            .unwrap_or(Decimal::ZERO);
        out.push(Position {
            symbol,
            side,
            qty: amt.abs(),
            entry_price: entry,
            stop_loss: None,
            take_profit: None,
            unrealized_pnl: upnl,
            opened_bar_time: None,
            leverage,
        });
    }
    Ok(out)
}

fn json_num(v: Option<&Value>) -> Result<Decimal, ExchangeError> {
    let v = v.ok_or_else(|| ExchangeError("missing numeric value".into()))?;
    match v {
        Value::String(s) => dec(s).map_err(|e| ExchangeError(e.to_string())),
        Value::Number(n) => dec(&n.to_string()).map_err(|e| ExchangeError(e.to_string())),
        _ => Err(ExchangeError("not a decimal".into())),
    }
}

pub fn parse_account(raw: &Value, starting_equity: Decimal) -> Result<Account, ExchangeError> {
    let obj = raw
        .as_object()
        .ok_or_else(|| ExchangeError("account payload is not an object".into()))?;
    let wallet = json_num(obj.get("totalWalletBalance"))
        .map_err(|e| ExchangeError(format!("account fields invalid: {e}")))?;
    let unreal = json_num(obj.get("totalUnrealizedProfit"))
        .map_err(|e| ExchangeError(format!("account fields invalid: {e}")))?;
    let available = json_num(obj.get("availableBalance"))
        .map_err(|e| ExchangeError(format!("account fields invalid: {e}")))?;
    Ok(Account {
        wallet_balance: wallet,
        unrealized_pnl: unreal,
        available_balance: available,
        starting_equity,
    })
}

pub fn parse_balances(raw: &Value, starting_equity: Decimal) -> Result<Account, ExchangeError> {
    let list = raw
        .as_array()
        .ok_or_else(|| ExchangeError("balance payload is not a list".into()))?;
    let usdt = list.iter().find(|row| {
        row.get("asset")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .eq_ignore_ascii_case("USDT")
    });
    let usdt = usdt.ok_or_else(|| ExchangeError("USDT balance missing".into()))?;
    let wallet = json_num(usdt.get("balance")).map_err(|e| ExchangeError(format!("balance fields invalid: {e}")))?;
    let unreal = json_num(usdt.get("crossUnPnl")).unwrap_or(Decimal::ZERO);
    let available = json_num(usdt.get("availableBalance"))
        .or_else(|_| json_num(usdt.get("balance")))
        .map_err(|e| ExchangeError(format!("balance fields invalid: {e}")))?;
    Ok(Account {
        wallet_balance: wallet,
        unrealized_pnl: unreal,
        available_balance: available,
        starting_equity,
    })
}

fn with_start(account: Account, starting_equity: Option<Decimal>) -> Account {
    if starting_equity.is_some() {
        return account;
    }
    let start = current_equity(account.wallet_balance, account.unrealized_pnl);
    Account {
        starting_equity: start,
        ..account
    }
}

pub fn load_account(client: &mut dyn SnapshotClient, starting_equity: Option<Decimal>) -> Result<Account, ExchangeError> {
    let fallback = starting_equity.unwrap_or(Decimal::ZERO);
    match client.balances() {
        Ok(raw) => {
            let parsed = parse_balances(&raw, fallback)?;
            return Ok(with_start(parsed, starting_equity));
        }
        Err(exc) if is_retry_error(Some(&exc.0)) => return Err(exc),
        Err(_) => {}
    }
    let parsed = parse_account(&client.account()?, fallback)?;
    Ok(with_start(parsed, starting_equity))
}

pub fn account_with_position_upnl(account: Account, positions: &[Position]) -> Account {
    let usdt: Vec<&Position> = positions
        .iter()
        .filter(|p| is_tradable_symbol(&p.symbol))
        .collect();
    if usdt.is_empty() {
        return account;
    }
    let upnl: Decimal = usdt.iter().map(|p| p.unrealized_pnl).sum();
    if account.unrealized_pnl != Decimal::ZERO || upnl == Decimal::ZERO {
        return account;
    }
    Account {
        unrealized_pnl: upnl,
        ..account
    }
}

/// Market + signed book for `fetch_snapshot`. Tests implement this; live uses BinanceFutures.
pub trait SnapshotClient {
    fn ticker_24h(&mut self) -> Result<Vec<Ticker>, ExchangeError>;
    fn klines(&mut self, symbol: &str, interval: &str, limit: usize) -> Result<Vec<Bar>, ExchangeError>;
    fn account(&mut self) -> Result<Value, ExchangeError>;
    fn balances(&mut self) -> Result<Value, ExchangeError> {
        Err(ExchangeError("balances not supported".into()))
    }
    fn position_risk(&mut self) -> Result<Value, ExchangeError>;
    fn tradfi_symbols(&mut self) -> Result<Vec<String>, ExchangeError> {
        Ok(Vec::new())
    }
}

pub trait FlattenClient {
    fn cancel_protectives(&mut self, symbol: &str) -> Result<(), ExchangeError>;
    fn market_close(&mut self, symbol: &str, side: &str, qty: Decimal) -> Result<(), ExchangeError>;
    fn position_risk(&mut self) -> Result<Value, ExchangeError>;
}

pub trait LiveClient: FlattenClient {
    fn filters_for(&mut self, symbol: &str) -> Result<SymbolFilters, ExchangeError>;
    fn market_buy(&mut self, symbol: &str, qty: Decimal) -> Result<(), ExchangeError>;
    fn place_tp_sl(
        &mut self,
        symbol: &str,
        take_profit: Decimal,
        stop_loss: Decimal,
        qty: Option<Decimal>,
    ) -> Result<(), ExchangeError>;
    fn replace_stop(
        &mut self,
        symbol: &str,
        stop_loss: Decimal,
        take_profit: Option<Decimal>,
        qty: Option<Decimal>,
    ) -> Result<(), ExchangeError>;
    fn cancel_algo_order(&mut self, _symbol: &str, _algo_id: &str) -> Result<(), ExchangeError> {
        Ok(())
    }
    fn cancel_plain_order(&mut self, _symbol: &str, _order_id: i64) -> Result<(), ExchangeError> {
        Ok(())
    }
    fn set_leverage(&mut self, _symbol: &str, _leverage: i32) -> Result<(), ExchangeError> {
        Ok(())
    }
    fn max_notional(&mut self, _symbol: &str, _leverage: i32) -> Result<Option<Decimal>, ExchangeError> {
        Ok(None)
    }
    fn open_algo_orders(&mut self, _symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        Ok(Vec::new())
    }
    fn open_orders(&mut self, _symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        Ok(Vec::new())
    }
}

/// Place the new pair first, then drop leftover TP/SL.
/// Cancel-all-then-place leaves a naked long (VVVUSDT 2026-08-31).
pub fn replace_stop_place_first(
    client: &mut dyn LiveClient,
    symbol: &str,
    stop_loss: Decimal,
    take_profit: Decimal,
    qty: Option<Decimal>,
) -> Result<(), ExchangeError> {
    client.place_tp_sl(symbol, take_profit, stop_loss, qty)?;
    prune_stale_protectives(client, symbol, stop_loss, take_profit);
    Ok(())
}

/// Drop leftover TP/SL after the new pair is already on the book.
/// Never `cancel_protectives` first: that window is a naked long.
pub fn prune_stale_protectives(
    client: &mut dyn LiveClient,
    symbol: &str,
    stop_loss: Decimal,
    take_profit: Decimal,
) {
    let mut keep_stop = false;
    let mut keep_tp = false;
    if let Ok(mut rows) = client.open_algo_orders(Some(symbol)) {
        rows.sort_by(|a, b| algo_id_of(b).cmp(&algo_id_of(a)));
        for row in rows {
            let drop = if stale_sell_protective(&row, stop_loss, take_profit) {
                true
            } else {
                match sell_protective_kind(&row) {
                    Some(ProtectiveKind::Stop) if keep_stop => true,
                    Some(ProtectiveKind::TakeProfit) if keep_tp => true,
                    Some(ProtectiveKind::Stop) => {
                        keep_stop = true;
                        false
                    }
                    Some(ProtectiveKind::TakeProfit) => {
                        keep_tp = true;
                        false
                    }
                    None => false,
                }
            };
            if drop {
                if let Some(id) = algo_id_of(&row) {
                    let _ = client.cancel_algo_order(symbol, &id);
                }
            }
        }
    }
    if let Ok(mut rows) = client.open_orders(Some(symbol)) {
        rows.sort_by(|a, b| order_id_of(b).cmp(&order_id_of(a)));
        for row in rows {
            let drop = if stale_sell_protective(&row, stop_loss, take_profit) {
                true
            } else {
                match sell_protective_kind(&row) {
                    Some(ProtectiveKind::Stop) if keep_stop => true,
                    Some(ProtectiveKind::TakeProfit) if keep_tp => true,
                    Some(ProtectiveKind::Stop) => {
                        keep_stop = true;
                        false
                    }
                    Some(ProtectiveKind::TakeProfit) => {
                        keep_tp = true;
                        false
                    }
                    None => false,
                }
            };
            if drop {
                if let Some(id) = order_id_of(&row) {
                    let _ = client.cancel_plain_order(symbol, id);
                }
            }
        }
    }
}

const FILTER_TTL: Duration = Duration::from_secs(60);

const KLINE_INTERVALS: &[&str] = &[
    "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "8h", "12h", "1d", "3d", "1w", "1M",
];

pub fn is_kline_interval(interval: &str) -> bool {
    KLINE_INTERVALS.contains(&interval)
}

/// Idempotent BUY id. Binance `newClientOrderId` is 1–36 of `[.A-Za-z0-9_:-]`.
/// Bucketed to `RETRY_BACKOFF_SEC` so a timed-out POST and the next enter
/// reuse the same id (duplicate rejected) instead of double-filling.
pub fn buy_client_order_id(symbol: &str, now_ms: i64) -> String {
    let base: String = symbol
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    let bucket_ms = (RETRY_BACKOFF_SEC * 1000.0) as i64;
    let bucket_ms = bucket_ms.max(1);
    let bucket = now_ms.div_euclid(bucket_ms).max(0);
    format!("tui{base}{bucket}").chars().take(36).collect()
}

pub fn parse_symbol_filters(raw: &Value, symbol: &str) -> Result<SymbolFilters, ExchangeError> {
    let want = symbol.to_ascii_uppercase();
    let symbols = raw
        .get("symbols")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ExchangeError("exchangeInfo missing symbols".into()))?;
    let row = symbols
        .iter()
        .find(|s| {
            s.get("symbol")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .eq_ignore_ascii_case(&want)
        })
        .ok_or_else(|| ExchangeError(format!("symbol {want} not listed")))?;
    let filters = row
        .get("filters")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ExchangeError("incomplete filters".into()))?;
    let mut tick = None;
    let mut step = None;
    let mut min_qty = None;
    let mut min_notional = None;
    for flt in filters {
        let ftype = flt.get("filterType").and_then(|v| v.as_str()).unwrap_or("");
        if ftype == "PRICE_FILTER" {
            tick = flt.get("tickSize").and_then(|v| v.as_str()).and_then(|s| dec(s).ok());
        } else if ftype == "LOT_SIZE" {
            step = flt.get("stepSize").and_then(|v| v.as_str()).and_then(|s| dec(s).ok());
            min_qty = flt.get("minQty").and_then(|v| v.as_str()).and_then(|s| dec(s).ok());
        } else if ftype == "MIN_NOTIONAL" || ftype == "NOTIONAL" {
            min_notional = flt
                .get("notional")
                .or_else(|| flt.get("minNotional"))
                .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| Some(v.to_string())))
                .and_then(|s| dec(&s).ok());
        }
    }
    Ok(SymbolFilters {
        tick_size: tick.ok_or_else(|| ExchangeError("incomplete filters".into()))?,
        step_size: step.ok_or_else(|| ExchangeError("incomplete filters".into()))?,
        min_qty: min_qty.ok_or_else(|| ExchangeError("incomplete filters".into()))?,
        min_notional: min_notional.unwrap_or(Decimal::from(5)),
    })
}

#[derive(Clone)]
pub struct BinanceFutures {
    pub base_url: String,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
    pub recv_window: i32,
    pub timeout: f64,
    time_offset_ms: Cell<i64>,
    time_synced: Cell<bool>,
    info_cache: RefCell<Option<(Instant, Value)>>,
    filter_cache: RefCell<HashMap<String, (Instant, SymbolFilters)>>,
    agent: ureq::Agent,
}

impl std::fmt::Debug for BinanceFutures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinanceFutures")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|k| format!("{}…", k.chars().take(4).collect::<String>())))
            .field("api_secret", &self.api_secret.as_ref().map(|_| "[redacted]"))
            .field("recv_window", &self.recv_window)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl BinanceFutures {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            base_url: cfg.base_url.clone(),
            api_key: cfg.credentials.as_ref().map(|c| c.api_key.clone()),
            api_secret: cfg.credentials.as_ref().map(|c| c.api_secret.clone()),
            recv_window: cfg.recv_window,
            timeout: cfg.http_timeout,
            time_offset_ms: Cell::new(0),
            time_synced: Cell::new(false),
            info_cache: RefCell::new(None),
            filter_cache: RefCell::new(HashMap::new()),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs_f64(cfg.http_timeout.max(0.1)))
                .build(),
        }
    }

    fn local_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn map_ureq(err: ureq::Error, path: &str) -> ExchangeError {
        let raw = match err {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                format!("HTTP {code} {path}: {body}")
            }
            other => format!("HTTP {path}: {other}"),
        };
        ExchangeError(redact_secrets(&raw))
    }

    pub fn sync_time(&self) -> Result<(), ExchangeError> {
        let raw = self.public_get("/fapi/v1/time", "")?;
        let server = raw
            .get("serverTime")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ExchangeError("server time unavailable".into()))?;
        self.time_offset_ms.set(server - Self::local_ms());
        self.time_synced.set(true);
        Ok(())
    }

    fn public_get(&self, path: &str, query: &str) -> Result<Value, ExchangeError> {
        match self.public_get_once(path, query) {
            Ok(v) => Ok(v),
            Err(exc) if is_retry_error(Some(&exc.0)) => self.public_get_once(path, query),
            Err(exc) => Err(exc),
        }
    }

    fn public_get_once(&self, path: &str, query: &str) -> Result<Value, ExchangeError> {
        let url = if query.is_empty() {
            format!("{}{path}", self.base_url)
        } else {
            format!("{}{path}?{query}", self.base_url)
        };
        let resp = self
            .agent
            .get(&url)
            .set("User-Agent", "tui-bot-rust")
            .call()
            .map_err(|e| Self::map_ureq(e, path))?;
        resp.into_json().map_err(|e| ExchangeError(e.to_string()))
    }

    fn signed(&self, params: &BTreeMap<String, String>) -> Result<String, ExchangeError> {
        let secret = self
            .api_secret
            .as_deref()
            .ok_or_else(|| ExchangeError("no api secret".into()))?;
        if !self.time_synced.get() {
            let _ = self.sync_time();
        }
        let ts = Self::local_ms() + self.time_offset_ms.get();
        signed_query_string(params, secret, ts, self.recv_window as i64).map_err(|e: SignError| ExchangeError(e.to_string()))
    }

    pub fn ticker_24h(&self) -> Result<Vec<Ticker>, ExchangeError> {
        let raw = self.public_get("/fapi/v1/ticker/24hr", "")?;
        Ok(parse_tickers(&raw))
    }

    pub fn account(&self) -> Result<Value, ExchangeError> {
        self.signed_request("GET", "/fapi/v2/account", &BTreeMap::new())
    }

    pub fn balances(&self) -> Result<Value, ExchangeError> {
        self.signed_request("GET", "/fapi/v2/balance", &BTreeMap::new())
    }

    pub fn klines(&self, symbol: &str, interval: &str, limit: usize) -> Result<Vec<crate::models::Bar>, ExchangeError> {
        if !is_kline_interval(interval) {
            return Err(ExchangeError(format!("bad kline interval {interval}")));
        }
        let mut q = BTreeMap::new();
        q.insert("symbol".into(), symbol.to_string());
        q.insert("interval".into(), interval.to_string());
        q.insert("limit".into(), limit.to_string());
        let raw = self.public_get("/fapi/v1/klines", &canonical_query(&q))?;
        let Some(arr) = raw.as_array() else {
            return Ok(Vec::new());
        };
        let mut bars = Vec::new();
        for row in arr {
            if let Ok(b) = bar_from_kline(row) {
                bars.push(b);
            }
        }
        Ok(bars)
    }

    fn signed_request(&self, method: &str, path: &str, params: &BTreeMap<String, String>) -> Result<Value, ExchangeError> {
        self.signed_request_retry(method, path, params, 0)
    }

    fn signed_request_retry(
        &self,
        method: &str,
        path: &str,
        params: &BTreeMap<String, String>,
        retries: u8,
    ) -> Result<Value, ExchangeError> {
        let mut prepared;
        let params = if method.eq_ignore_ascii_case("POST") && path == "/fapi/v1/algoOrder" {
            prepared = params.clone();
            prepare_algo_params(&mut prepared);
            &prepared
        } else {
            params
        };
        let query = self.signed(params)?;
        let url = format!("{}{path}?{query}", self.base_url);
        let key = self
            .api_key
            .as_deref()
            .ok_or_else(|| ExchangeError("no api key".into()))?;
        let req = match method {
            "GET" => self.agent.get(&url),
            "POST" => self.agent.post(&url),
            "DELETE" => self.agent.request("DELETE", &url),
            other => return Err(ExchangeError(format!("bad method {other}"))),
        };
        let result = req
            .set("X-MBX-APIKEY", key)
            .set("User-Agent", "tui-bot-rust")
            .call()
            .map_err(|e| Self::map_ureq(e, path))
            .and_then(|resp| resp.into_json().map_err(|e| ExchangeError(e.to_string())));
        match result {
            // −1021 is rejected before matching. Retry after clock sync.
            // Timeouts after a POST fill are NOT retried; enter_live re-reads positionRisk.
            Err(exc) if retries < 1 && Self::should_retry_signed(method, &exc) => {
                if exc.0.contains("-1021") {
                    let _ = self.sync_time();
                }
                self.signed_request_retry(method, path, params, retries + 1)
            }
            other => other,
        }
    }

    fn should_retry_signed(method: &str, exc: &ExchangeError) -> bool {
        if exc.0.contains("-1021") {
            return true;
        }
        if !is_retry_error(Some(&exc.0)) {
            return false;
        }
        let m = method.to_ascii_uppercase();
        m == "GET" || m == "DELETE"
    }
}

impl FlattenClient for BinanceFutures {
    fn cancel_protectives(&mut self, symbol: &str) -> Result<(), ExchangeError> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), symbol.into());
        let _ = self.signed_request("DELETE", "/fapi/v1/allOpenOrders", &p);
        let _ = self.signed_request("DELETE", "/fapi/v1/algoOpenOrders", &p);
        Ok(())
    }

    fn market_close(&mut self, symbol: &str, side: &str, qty: Decimal) -> Result<(), ExchangeError> {
        let close_side = if side.eq_ignore_ascii_case("LONG") {
            "SELL"
        } else {
            "BUY"
        };
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), symbol.into());
        p.insert("side".into(), close_side.into());
        p.insert("type".into(), "MARKET".into());
        p.insert("quantity".into(), qty.normalize().to_string());
        p.insert("reduceOnly".into(), "true".into());
        p.insert("newOrderRespType".into(), "RESULT".into());
        match self.signed_request("POST", "/fapi/v1/order", &p) {
            Ok(_) => Ok(()),
            Err(exc) => {
                let t = exc.0.to_ascii_lowercase();
                // Already flat: success. Never retry as a naked order (opens a leftover short).
                if t.contains("-2022") || t.contains("reduceonly") {
                    Ok(())
                } else {
                    Err(exc)
                }
            }
        }
    }

    fn position_risk(&mut self) -> Result<Value, ExchangeError> {
        self.signed_request("GET", "/fapi/v2/positionRisk", &BTreeMap::new())
    }
}

impl LiveClient for BinanceFutures {
    fn filters_for(&mut self, symbol: &str) -> Result<SymbolFilters, ExchangeError> {
        let want = symbol.to_ascii_uppercase();
        if let Some((at, cached)) = self.filter_cache.borrow().get(&want) {
            if at.elapsed() < FILTER_TTL {
                return Ok(cached.clone());
            }
        }
        let raw = {
            if let Some((at, cached)) = self.info_cache.borrow().as_ref() {
                if at.elapsed() < FILTER_TTL {
                    Some(cached.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };
        let raw = match raw {
            Some(v) => v,
            None => {
                let fetched = self.public_get("/fapi/v1/exchangeInfo", "")?;
                *self.info_cache.borrow_mut() = Some((Instant::now(), fetched.clone()));
                fetched
            }
        };
        let parsed = parse_symbol_filters(&raw, &want)?;
        self.filter_cache
            .borrow_mut()
            .insert(want, (Instant::now(), parsed.clone()));
        Ok(parsed)
    }

    fn market_buy(&mut self, symbol: &str, qty: Decimal) -> Result<(), ExchangeError> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), symbol.into());
        p.insert("side".into(), "BUY".into());
        p.insert("type".into(), "MARKET".into());
        p.insert("quantity".into(), qty.normalize().to_string());
        p.insert("newOrderRespType".into(), "RESULT".into());
        p.insert(
            "newClientOrderId".into(),
            buy_client_order_id(symbol, Self::local_ms()),
        );
        self.signed_request("POST", "/fapi/v1/order", &p)?;
        Ok(())
    }

    fn place_tp_sl(
        &mut self,
        symbol: &str,
        take_profit: Decimal,
        stop_loss: Decimal,
        qty: Option<Decimal>,
    ) -> Result<(), ExchangeError> {
        let filters = self.filters_for(symbol)?;
        let tp = quantize_to_step(take_profit, filters.tick_size, true).map_err(|e| ExchangeError(e.to_string()))?;
        let sl = quantize_to_step(stop_loss, filters.tick_size, false).map_err(|e| ExchangeError(e.to_string()))?;
        if sl <= Decimal::ZERO || tp <= Decimal::ZERO || sl >= tp {
            return Err(ExchangeError("protective prices invalid after quantize".into()));
        }
        // Never send closePosition: a naked SELL stop with no long opens a leftover short.
        let qty_s = match qty {
            Some(q) if q > Decimal::ZERO => {
                let sized = quantize_to_step(q, filters.step_size, false).map_err(|e| ExchangeError(e.to_string()))?;
                if sized <= Decimal::ZERO {
                    return Err(ExchangeError("protective qty invalid after quantize".into()));
                }
                sized.normalize().to_string()
            }
            _ => {
                return Err(ExchangeError(
                    "refuse closePosition TP/SL (naked SELL opens leftover short)".into(),
                ));
            }
        };
        for (otype, trigger) in [("STOP_MARKET", sl), ("TAKE_PROFIT_MARKET", tp)] {
            let mut p = BTreeMap::new();
            p.insert("symbol".into(), symbol.into());
            p.insert("side".into(), "SELL".into());
            p.insert("type".into(), otype.into());
            p.insert("algoType".into(), "CONDITIONAL".into());
            p.insert("triggerPrice".into(), trigger.normalize().to_string());
            p.insert("workingType".into(), "MARK_PRICE".into());
            p.insert("quantity".into(), qty_s.clone());
            p.insert("reduceOnly".into(), "true".into());
            match self.signed_request("POST", "/fapi/v1/algoOrder", &p) {
                Ok(_) => {}
                Err(exc) => {
                    let t = exc.0.to_ascii_lowercase();
                    if t.contains("-4130") || t.contains("closeposition in the direction") {
                        continue;
                    }
                    if t.contains("-2026") || t.contains("reduceonly order type") {
                        let mut q = BTreeMap::new();
                        q.insert("symbol".into(), symbol.into());
                        q.insert("side".into(), "SELL".into());
                        q.insert("type".into(), otype.into());
                        q.insert("stopPrice".into(), trigger.normalize().to_string());
                        q.insert("quantity".into(), qty_s.clone());
                        q.insert("reduceOnly".into(), "true".into());
                        q.insert("workingType".into(), "MARK_PRICE".into());
                        match self.signed_request("POST", "/fapi/v1/order", &q) {
                            Ok(_) => {}
                            Err(e2) => {
                                let t2 = e2.0.to_ascii_lowercase();
                                if t2.contains("-4130") || t2.contains("closeposition in the direction") {
                                    continue;
                                }
                                return Err(e2);
                            }
                        }
                    } else {
                        return Err(exc);
                    }
                }
            }
        }
        Ok(())
    }

    fn replace_stop(
        &mut self,
        symbol: &str,
        stop_loss: Decimal,
        take_profit: Option<Decimal>,
        qty: Option<Decimal>,
    ) -> Result<(), ExchangeError> {
        let tp = take_profit.ok_or_else(|| {
            ExchangeError("refuse replace_stop without take_profit (would drop TP)".into())
        })?;
        replace_stop_place_first(self, symbol, stop_loss, tp, qty)
    }

    fn cancel_algo_order(&mut self, symbol: &str, algo_id: &str) -> Result<(), ExchangeError> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), symbol.into());
        p.insert("algoId".into(), algo_id.into());
        self.signed_request("DELETE", "/fapi/v1/algoOrder", &p)?;
        Ok(())
    }

    fn cancel_plain_order(&mut self, symbol: &str, order_id: i64) -> Result<(), ExchangeError> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), symbol.into());
        p.insert("orderId".into(), order_id.to_string());
        self.signed_request("DELETE", "/fapi/v1/order", &p)?;
        Ok(())
    }

    fn set_leverage(&mut self, symbol: &str, leverage: i32) -> Result<(), ExchangeError> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), symbol.into());
        p.insert("leverage".into(), leverage.to_string());
        self.signed_request("POST", "/fapi/v1/leverage", &p)?;
        Ok(())
    }

    fn open_algo_orders(&mut self, symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        let mut p = BTreeMap::new();
        if let Some(s) = symbol {
            p.insert("symbol".into(), s.into());
        }
        let raw = self.signed_request("GET", "/fapi/v1/openAlgoOrders", &p)?;
        Ok(raw.as_array().cloned().unwrap_or_default())
    }

    fn open_orders(&mut self, symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        let mut p = BTreeMap::new();
        if let Some(s) = symbol {
            p.insert("symbol".into(), s.into());
        }
        let raw = self.signed_request("GET", "/fapi/v1/openOrders", &p)?;
        Ok(raw.as_array().cloned().unwrap_or_default())
    }
}

impl SnapshotClient for BinanceFutures {
    fn ticker_24h(&mut self) -> Result<Vec<Ticker>, ExchangeError> {
        BinanceFutures::ticker_24h(self)
    }

    fn klines(&mut self, symbol: &str, interval: &str, limit: usize) -> Result<Vec<Bar>, ExchangeError> {
        BinanceFutures::klines(self, symbol, interval, limit)
    }

    fn account(&mut self) -> Result<Value, ExchangeError> {
        BinanceFutures::account(self)
    }

    fn balances(&mut self) -> Result<Value, ExchangeError> {
        BinanceFutures::balances(self)
    }

    fn position_risk(&mut self) -> Result<Value, ExchangeError> {
        FlattenClient::position_risk(self)
    }
}

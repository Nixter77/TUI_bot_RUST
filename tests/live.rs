//! Drive shipped apply_live: reduce-only protective sizing, watch-mode, keys.

mod common;
use common::*;
use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use tui_bot::config::load_config;
use tui_bot::exchange::{
    prepare_algo_params, sell_protectives_are_sized, size_market_order, sized_long_protectives,
    ExchangeError, FlattenClient, LiveClient, SymbolFilters,
};
use serde_json::json;
use tui_bot::errors::COOLDOWN_SEC;
use tui_bot::engine::{tick_decisions, MomentumParams};
use tui_bot::live::{
    apply_decision, apply_live, clear_orphan_protectives, clear_vanished_longs, reconcile_live,
    sweep_rogue_shorts,
};
use tui_bot::models::{Decision, EngineState, MarketSnapshot, Position, Side, Ticker};
use tui_bot::trail::take_profit_price_net;

struct FakeClient {
    fail_protect: bool,
    pub buys: usize,
    pub protects: usize,
    pub protect_qty: Option<Decimal>,
    pub protect_cancels: Vec<String>,
    pub replaces: Vec<(String, Decimal, Option<Decimal>)>,
    pub replace_qty: Option<Decimal>,
    pub closes: Vec<(String, String, Decimal)>,
    pub leverage_calls: Vec<(String, i32)>,
    pub bought: Vec<Decimal>,
    pub bought_symbols: Vec<String>,
    pub min_notional: Decimal,
    pub dup4130: bool,
    pub risk: Value,
    pub algo_orders: Vec<Value>,
    pub open_orders: Vec<Value>,
    pub flip_to_short_on_replace: Option<(String, Decimal)>,
}

impl FakeClient {
    fn new() -> Self {
        Self {
            fail_protect: false,
            buys: 0,
            protects: 0,
            protect_qty: None,
            protect_cancels: Vec::new(),
            replaces: Vec::new(),
            replace_qty: None,
            closes: Vec::new(),
            leverage_calls: Vec::new(),
            bought: Vec::new(),
            bought_symbols: Vec::new(),
            min_notional: Decimal::from(5),
            dup4130: false,
            risk: Value::Array(vec![]),
            algo_orders: Vec::new(),
            open_orders: Vec::new(),
            flip_to_short_on_replace: None,
        }
    }
}

impl FlattenClient for FakeClient {
    fn cancel_protectives(&mut self, symbol: &str) -> Result<(), ExchangeError> {
        self.protect_cancels.push(symbol.into());
        Ok(())
    }
    fn market_close(&mut self, symbol: &str, side: &str, qty: Decimal) -> Result<(), ExchangeError> {
        self.closes.push((symbol.into(), side.into(), qty));
        Ok(())
    }
    fn position_risk(&mut self) -> Result<Value, ExchangeError> {
        Ok(self.risk.clone())
    }
}

impl LiveClient for FakeClient {
    fn filters_for(&mut self, _symbol: &str) -> Result<SymbolFilters, ExchangeError> {
        Ok(SymbolFilters {
            tick_size: d("0.1"),
            step_size: d("0.001"),
            min_qty: d("0.001"),
            min_notional: self.min_notional,
        })
    }
    fn market_buy(&mut self, symbol: &str, qty: Decimal) -> Result<(), ExchangeError> {
        self.buys += 1;
        self.bought.push(qty);
        self.bought_symbols.push(symbol.into());
        Ok(())
    }
    fn place_tp_sl(
        &mut self,
        _symbol: &str,
        _tp: Decimal,
        _sl: Decimal,
        qty: Option<Decimal>,
    ) -> Result<(), ExchangeError> {
        self.protects += 1;
        self.protect_qty = qty;
        if self.dup4130 {
            return Err(ExchangeError(
                r#"HTTP 400 /fapi/v1/algoOrder: {"code":-4130,"msg":"An open stop or take profit order with GTE and closePosition in the direction is existing."}"#.into(),
            ));
        }
        if self.fail_protect {
            return Err(ExchangeError("protect rejected".into()));
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
        self.replaces.push((symbol.into(), stop_loss, take_profit));
        self.replace_qty = qty;
        if let Some((sym, amt)) = &self.flip_to_short_on_replace {
            self.risk = json!([{
                "symbol": sym,
                "positionAmt": format!("-{amt}"),
                "entryPrice": "68600.9",
                "unRealizedProfit": "-2",
            }]);
        }
        Ok(())
    }
    fn set_leverage(&mut self, symbol: &str, leverage: i32) -> Result<(), ExchangeError> {
        self.leverage_calls.push((symbol.into(), leverage));
        Ok(())
    }
    fn open_algo_orders(&mut self, symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        Ok(filter_symbol_rows(&self.algo_orders, symbol))
    }
    fn open_orders(&mut self, symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        Ok(filter_symbol_rows(&self.open_orders, symbol))
    }
}

fn filter_symbol_rows(rows: &[Value], symbol: Option<&str>) -> Vec<Value> {
    let Some(want) = symbol else {
        return rows.to_vec();
    };
    let want = want.to_ascii_uppercase();
    rows.iter()
        .filter(|row| {
            row.get("symbol")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .eq_ignore_ascii_case(&want)
        })
        .cloned()
        .collect()
}

fn cfg_live() -> tui_bot::config::Config {
    let mut env = HashMap::new();
    env.insert("BINANCE_API_KEY".into(), "A".repeat(32));
    env.insert("BINANCE_API_SECRET".into(), "B".repeat(32));
    load_config(true, None, Some(&env)).unwrap()
}

fn snap(position: Option<Position>) -> MarketSnapshot {
    let mut s = MarketSnapshot::empty(d("1000"));
    s.tickers = vec![Ticker::new("BTCUSDT", d("1000"), d("5"), d("10"))];
    s.account.wallet_balance = d("1000");
    s.account.available_balance = d("1000");
    s.account.starting_equity = d("1000");
    s.position = position;
    s.chart_symbol = "BTCUSDT".into();
    s
}

fn enter() -> Decision {
    Decision::EnterLong {
        symbol: "BTCUSDT".into(),
        reason: "x".into(),
        take_profit: d("51000"),
        stop_loss: d("49000"),
    }
}

#[test]
fn watch_mode_sends_nothing() {
    let cfg = load_config(false, None, Some(&HashMap::new())).unwrap();
    let mut client = FakeClient::new();
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert!(result.error.is_none());
    assert!(!result.filled);
    assert_eq!(client.buys, 0);
}

#[test]
fn no_credentials_refuses() {
    let mut cfg = load_config(false, None, Some(&HashMap::new())).unwrap();
    cfg.live = true;
    cfg.credentials = None;
    let mut client = FakeClient::new();
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert_eq!(result.error.as_deref(), Some("live refused: no credentials"));
    assert!(!result.filled);
    assert_eq!(client.buys, 0);
}

#[test]
fn fill_then_protect_failure_reports_filled() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_protect = true;
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert!(result.filled);
    assert!(result.error.as_deref().unwrap_or("").contains("filled"));
    assert_eq!(client.buys, 1);
    assert_eq!(client.protects, 1);
    assert_eq!(result.mark, Some(d("1000")));
    assert!(result.qty.is_some());
}

#[test]
fn duplicate_protectives_are_not_a_failed_fill() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.dup4130 = true;
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert!(result.filled);
    assert!(result.error.is_none());
    assert_eq!(client.buys, 1);
}

#[test]
fn daily_halt_refuses_buy() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let mut state = EngineState::new(1);
    state.daily_halt = true;
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), Some(&state));
    assert!(!result.filled);
    assert!(result.error.is_none());
    assert_eq!(client.buys, 0);
}

#[test]
fn skip_enter_when_already_in_position() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let pos = Position::long("BTCUSDT", d("0.01"), d("50000"), Some(d("49000")), Some(d("51000")));
    let result = apply_live(&cfg, &mut client, &snap(Some(pos)), &enter(), None);
    assert!(!result.filled);
    assert!(result.error.as_deref().unwrap_or("").contains("already in position"));
    assert_eq!(client.buys, 0);
}

#[test]
fn sets_leverage_only_when_configured() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert_eq!(client.buys, 1);
    assert!(client.leverage_calls.is_empty());
    let mut levered = cfg.clone();
    levered.leverage = Some(5);
    let mut client2 = FakeClient::new();
    let result = apply_live(&levered, &mut client2, &snap(None), &enter(), None);
    assert!(result.filled);
    assert_eq!(client2.leverage_calls, vec![("BTCUSDT".into(), 5)]);
    assert_eq!(client2.buys, 1);
}

#[test]
fn default_notional_bumps_to_btc_min() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.min_notional = Decimal::from(100);
    let mut s = snap(None);
    s.tickers = vec![Ticker::new("BTCUSDT", d("115000"), d("1"), d("1"))];
    s.account.wallet_balance = d("3000");
    let result = apply_live(
        &cfg,
        &mut client,
        &s,
        &Decision::EnterLong {
            symbol: "BTCUSDT".into(),
            reason: "x".into(),
            take_profit: d("117875"),
            stop_loss: d("112700"),
        },
        None,
    );
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(result.filled);
    assert_eq!(client.bought, vec![d("0.001")]);
}

#[test]
fn amend_without_stored_tp_uses_entry_take_profit() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let pos = Position::long("BTCUSDT", d("0.02"), d("1000"), None, None);
    let result = apply_live(
        &cfg,
        &mut client,
        &snap(Some(pos)),
        &Decision::AmendStop {
            stop_loss: d("994"),
            reason: "trail".into(),
            symbol: String::new(),
        },
        None,
    );
    assert!(result.error.is_none());
    assert_eq!(client.replaces.len(), 1);
    let (symbol, sl, tp) = &client.replaces[0];
    assert_eq!(symbol, "BTCUSDT");
    assert_eq!(*sl, d("994"));
    assert_eq!(*tp, Some(take_profit_price_net(d("1000"), "LONG", cfg.tp_pct).unwrap()));
}

#[test]
fn exit_uses_shared_market_close() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let pos = Position {
        symbol: "ETHUSDT".into(),
        side: Side::Short,
        qty: d("0.071"),
        entry_price: d("2000"),
        stop_loss: None,
        take_profit: None,
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: None,
        leverage: 0,
    };
    let result = apply_live(
        &cfg,
        &mut client,
        &snap(Some(pos)),
        &Decision::ExitPosition {
            reason: "panic".into(),
            symbol: String::new(),
        },
        None,
    );
    assert!(result.error.is_none());
    assert_eq!(client.protect_cancels, vec!["ETHUSDT", "ETHUSDT"]);
    assert_eq!(client.closes, vec![("ETHUSDT".into(), "SHORT".into(), d("0.071"))]);
}

#[test]
fn algo_orders_send_conditional_algo_type() {
    let mut p = BTreeMap::new();
    p.insert("symbol".into(), "BTCUSDT".into());
    p.insert("side".into(), "SELL".into());
    p.insert("type".into(), "STOP_MARKET".into());
    p.insert("stopPrice".into(), "90".into());
    prepare_algo_params(&mut p);
    assert_eq!(p.get("algoType").map(String::as_str), Some("CONDITIONAL"));
    assert_eq!(p.get("triggerPrice").map(String::as_str), Some("90"));
    assert!(!p.contains_key("stopPrice"));
    p.insert("algoType".into(), "CONDITIONAL".into());
    p.insert("triggerPrice".into(), "110".into());
    p.insert("stopPrice".into(), "99".into());
    prepare_algo_params(&mut p);
    assert_eq!(p.get("algoType").map(String::as_str), Some("CONDITIONAL"));
    assert_eq!(p.get("triggerPrice").map(String::as_str), Some("110"));
    assert!(!p.contains_key("stopPrice"));
}

#[test]
fn protectives_are_reduce_only_sized_not_close_position() {
    let qty = d("0.01");
    let sl = d("49000");
    let tp = d("51000");
    let orders = sized_long_protectives(qty, sl, tp).unwrap();
    assert_eq!(orders.len(), 2);
    for o in &orders {
        assert_eq!(o.side, "SELL");
        assert!(o.reduce_only);
        assert!(!o.close_position);
        assert_eq!(o.quantity, qty);
    }
    assert_eq!(orders[0].order_type, "STOP_MARKET");
    assert_eq!(orders[1].order_type, "TAKE_PROFIT_MARKET");
    let live_qty = apply_live(&cfg_live(), &mut {
        let mut c = FakeClient::new();
        let _ = apply_live(&cfg_live(), &mut c, &snap(None), &enter(), None);
        assert_eq!(c.protect_qty, Some(c.bought[0]));
        c
    }, &snap(None), &enter(), None);
    let _ = live_qty;

    let rows = vec![
        serde_json::json!({"side":"SELL","orderType":"STOP_MARKET","quantity":"0.01","closePosition":false}),
        serde_json::json!({"side":"SELL","orderType":"TAKE_PROFIT_MARKET","quantity":"0.01","closePosition":"false"}),
    ];
    assert!(sell_protectives_are_sized(&rows));
    let naked = vec![
        serde_json::json!({"side":"SELL","orderType":"STOP_MARKET","quantity":"0","closePosition":true}),
        serde_json::json!({"side":"SELL","orderType":"TAKE_PROFIT_MARKET","closePosition":true}),
    ];
    assert!(!sell_protectives_are_sized(&naked));
}

#[test]
fn size_market_order_uses_shipped_formula() {
    let filters = SymbolFilters {
        tick_size: d("0.1"),
        step_size: d("0.001"),
        min_qty: d("0.001"),
        min_notional: d("100"),
    };
    let qty = size_market_order(d("20"), d("115000"), &filters).unwrap();
    assert_eq!(qty, d("0.001"));
}

fn short_pos(symbol: &str, qty: &str, entry: &str, upnl: &str) -> Position {
    Position {
        symbol: symbol.into(),
        side: Side::Short,
        qty: d(qty),
        entry_price: d(entry),
        stop_loss: None,
        take_profit: None,
        unrealized_pnl: d(upnl),
        opened_bar_time: None,
        leverage: 0,
    }
}

fn live_book(position: Option<Position>, open: Vec<Position>) -> MarketSnapshot {
    let mut s = snap(position.clone());
    s.live_book = true;
    s.open_positions = open;
    s.position = position;
    s
}

#[test]
fn sweep_closes_leftover_short_and_cools_symbol() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let short = short_pos("BTCUSDT", "0.004", "68600", "-2");
    let mut snap = live_book(None, vec![short.clone()]);
    snap.chart_symbol = "ETHUSDT".into();
    let mut state = EngineState::new(2);
    let now = 1_700_000_000.0;
    let result = sweep_rogue_shorts(&cfg, &mut client, &mut state, &snap, Some(now));
    assert_eq!(result.closed, vec!["SHORT BTCUSDT".to_string()]);
    assert_eq!(client.closes, vec![("BTCUSDT".into(), "SHORT".into(), d("0.004"))]);
    assert!(state.cooldowns.get("BTCUSDT").copied().unwrap_or(0.0) > now);
    assert!(state.cooldowns.get("BTCUSDT").copied().unwrap() >= now + COOLDOWN_SEC);
    assert!(state.recent_actions.iter().any(|a| a.contains("чужой шорт")));
    assert!(!state.entries_paused);
}

#[test]
fn vanished_long_cancels_leftover_protectives() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let long = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let mut state = EngineState::new(2);
    state.position = Some(long.clone());
    state.positions = vec![long];
    let mut snap = live_book(None, Vec::new());
    snap.tickers = vec![Ticker::new("BTCUSDT", d("995"), d("1"), d("10"))];
    let now = london_ts();
    let cleared = clear_vanished_longs(&cfg, &mut client, &mut state, &snap, now);
    assert_eq!(cleared, vec!["BTCUSDT".to_string()]);
    assert_eq!(client.protect_cancels, vec!["BTCUSDT"]);
    assert!(state.recent_actions.iter().any(|a| a.contains("снял TP/SL")));
    assert!(state.position.is_none());
    assert!(state.cooldowns.get("BTCUSDT").copied().unwrap_or(0.0) > 0.0);
    assert!(
        state.cooldown_until > now,
        "losing vanish must pause the desk, got {}",
        state.cooldown_until
    );
}

#[test]
fn vanished_long_cools_even_when_inflight() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let long = Position::long("SUPERUSDT", d("367"), d("0.109"), Some(d("0.107")), Some(d("0.112")));
    let mut state = EngineState::new(4);
    state.position = Some(long.clone());
    state.positions = vec![long];
    state.inflight_symbols = vec!["SUPERUSDT".into()];
    state.entry_inflight = true;
    let mut snap = live_book(None, Vec::new());
    snap.tickers = vec![Ticker::new("SUPERUSDT", d("0.109"), d("3.2"), d("180000"))];
    let now = london_ts();
    let cleared = clear_vanished_longs(&cfg, &mut client, &mut state, &snap, now);
    assert_eq!(cleared, vec!["SUPERUSDT".to_string()]);
    assert!(state.position.is_none());
    assert!(!state.inflight_symbols.iter().any(|s| s.eq_ignore_ascii_case("SUPERUSDT")));
    assert!(state.cooldowns.get("SUPERUSDT").copied().unwrap_or(0.0) > 0.0);
    assert!(
        state.cooldown_until > now,
        "scratch/loss vanish must pause the desk, got {}",
        state.cooldown_until
    );
}

#[test]
fn vanished_long_not_cleared_when_live_long_remains() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let long = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let mut state = EngineState::new(2);
    state.position = Some(long.clone());
    state.positions = vec![long.clone()];
    let snap = live_book(Some(long.clone()), vec![long]);
    let cleared = clear_vanished_longs(&cfg, &mut client, &mut state, &snap, london_ts());
    assert!(cleared.is_empty());
    assert!(client.protect_cancels.is_empty());
}

#[test]
fn reconcile_skips_tick_only_after_short_sweep() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let short = short_pos("BTCUSDT", "0.004", "68600", "-2");
    let mut snap = live_book(None, vec![short]);
    snap.chart_symbol = "ETHUSDT".into();
    let mut state = EngineState::new(2);
    let rec = reconcile_live(&cfg, &mut client, &mut state, &snap, Some(1_700_000_000.0));
    assert!(rec.skip_tick);
    assert!(rec.last_text.contains("чужой шорт"));

    let live = Position::long("ETHUSDT", d("0.015"), d("2552.32"), Some(d("2477")), Some(d("2616")));
    client.algo_orders = vec![
        json!({"symbol":"ETHUSDT","side":"SELL","orderType":"STOP_MARKET","closePosition":true,"quantity":"0"}),
        json!({"symbol":"ETHUSDT","side":"SELL","orderType":"TAKE_PROFIT_MARKET","closePosition":true,"quantity":"0"}),
    ];
    let mut long_snap = live_book(Some(live.clone()), vec![live]);
    long_snap.tickers = vec![Ticker::new("ETHUSDT", d("2527"), d("1"), d("10"))];
    let rec2 = reconcile_live(&cfg, &mut client, &mut EngineState::new(1), &long_snap, None);
    assert!(!rec2.skip_tick);
    assert!(rec2.last_text.contains("ETHUSDT"));
}

#[test]
fn trail_that_opens_a_short_is_flattened() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.flip_to_short_on_replace = Some(("BTCUSDT".into(), d("0.004")));
    let long = Position::long(
        "BTCUSDT",
        d("0.0008"),
        d("68421"),
        Some(d("68249")),
        Some(d("68763")),
    );
    let mut snap = live_book(Some(long.clone()), vec![long]);
    snap.tickers = vec![Ticker::new("BTCUSDT", d("68650"), d("1"), d("10"))];
    let result = apply_live(
        &cfg,
        &mut client,
        &snap,
        &Decision::AmendStop {
            stop_loss: d("68604"),
            reason: "trail".into(),
            symbol: "BTCUSDT".into(),
        },
        None,
    );
    assert_eq!(client.replaces.len(), 1);
    assert!(result.error.as_deref().unwrap_or("").contains("шорт"));
    assert_eq!(client.closes, vec![("BTCUSDT".into(), "SHORT".into(), d("0.004"))]);
    assert_eq!(result.forget_symbol, "BTCUSDT");
}

#[test]
fn amend_without_live_long_flattens_leftover_short() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let short = short_pos("BTCUSDT", "0.0007", "77263.3", "-0.0957");
    client.risk = json!([{
        "symbol": "BTCUSDT",
        "positionAmt": "-0.0007",
        "entryPrice": "77263.3",
        "unRealizedProfit": "-0.0957",
    }]);
    let mut snap = live_book(None, vec![short]);
    snap.tickers = vec![Ticker::new("BTCUSDT", d("77240"), d("-0.1"), d("10"))];
    let result = apply_live(
        &cfg,
        &mut client,
        &snap,
        &Decision::AmendStop {
            stop_loss: d("77237"),
            reason: "scalp attach stop".into(),
            symbol: String::new(),
        },
        None,
    );
    assert!(result.error.as_deref().unwrap_or("").contains("шорт"));
    assert_eq!(client.closes, vec![("BTCUSDT".into(), "SHORT".into(), d("0.0007"))]);
    assert_eq!(result.forget_symbol, "BTCUSDT");
}

#[test]
fn orphan_stop_on_flat_symbol_is_cancelled_after_restart() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.algo_orders = vec![json!({
        "symbol": "BTCUSDT",
        "side": "SELL",
        "orderType": "STOP_MARKET",
        "closePosition": true,
        "quantity": "0"
    })];
    let live = Position::long("SOLUSDT", d("0.41"), d("95.66"), Some(d("93.74")), Some(d("98.04")));
    let mut snap = live_book(Some(live.clone()), vec![live]);
    snap.chart_symbol = "SOLUSDT".into();
    let mut state = EngineState::new(1);
    let cleared = clear_orphan_protectives(&cfg, &mut client, &mut state, &snap);
    assert_eq!(cleared, vec!["BTCUSDT".to_string()]);
    assert_eq!(client.protect_cancels, vec!["BTCUSDT"]);
    assert!(state.recent_actions.iter().any(|a| a.contains("сиротский стоп")));
}

#[test]
fn reconcile_skips_tick_after_orphan_stop_so_it_does_not_enter() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.algo_orders = vec![json!({
        "symbol": "BTCUSDT",
        "side": "SELL",
        "orderType": "STOP_MARKET",
        "triggerPrice": "77237",
        "closePosition": true
    })];
    let mut snap = live_book(None, Vec::new());
    snap.chart_symbol = "ETHUSDT".into();
    let mut state = EngineState::new(1);
    let rec = reconcile_live(&cfg, &mut client, &mut state, &snap, Some(1_700_000_000.0));
    assert!(rec.skip_tick);
    assert!(rec.last_text.contains("сиротский стоп"));
    assert!(rec.last_text.contains("BTCUSDT"));
    assert_eq!(client.protect_cancels, vec!["BTCUSDT"]);
    assert!(client.closes.is_empty());
}

#[test]
fn orphan_stop_on_live_long_is_left_for_rearm() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.algo_orders = vec![json!({
        "symbol": "SOLUSDT",
        "side": "SELL",
        "orderType": "STOP_MARKET",
        "quantity": "0.41",
        "closePosition": false
    })];
    let live = Position::long("SOLUSDT", d("0.41"), d("95.66"), Some(d("93.74")), Some(d("98.04")));
    let snap = live_book(Some(live.clone()), vec![live]);
    let mut state = EngineState::new(1);
    let cleared = clear_orphan_protectives(&cfg, &mut client, &mut state, &snap);
    assert!(cleared.is_empty());
    assert!(client.protect_cancels.is_empty());
}

fn cfg_live_three() -> tui_bot::config::Config {
    let mut env = HashMap::new();
    env.insert("BINANCE_API_KEY".into(), "A".repeat(32));
    env.insert("BINANCE_API_SECRET".into(), "B".repeat(32));
    env.insert("STRATEGY1_MAX_POSITIONS".into(), "3".into());
    env.insert("STRATEGY1_ALWAYS_ENTER".into(), "1".into());
    load_config(true, None, Some(&env)).unwrap()
}

fn three_ticker_snap() -> MarketSnapshot {
    let mut s = MarketSnapshot::empty(d("10000"));
    s.tickers = tickers();
    s.account.wallet_balance = d("10000");
    s.account.available_balance = d("10000");
    s.account.starting_equity = d("10000");
    s.chart_symbol = "BTCUSDT".into();
    s.live_book = true;
    s.account_ok = true;
    s
}

fn enter_sym(symbol: &str, tp: &str, sl: &str) -> Decision {
    Decision::EnterLong {
        symbol: symbol.into(),
        reason: "x".into(),
        take_profit: d(tp),
        stop_loss: d(sl),
    }
}

#[test]
fn live_tui_loop_fills_three_majors_then_refuses_fourth() {
    let cfg = cfg_live_three();
    assert_eq!(cfg.max_positions, 3);
    let snap = three_ticker_snap();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let mut state = EngineState::new(1);
    let mut client = FakeClient::new();
    let mut snap = snap;
    for step in 0..3 {
        let (new_state, decisions) =
            tick_decisions(&state, &snap, london_ts() + step as f64 * 60.0, Some(&mom), None, None);
        state = new_state;
        let enters: Vec<Decision> = decisions
            .into_iter()
            .filter(|d| matches!(d, Decision::EnterLong { .. }))
            .collect();
        assert_eq!(enters.len(), 1, "step {step}: {enters:?}");
        for d in &enters {
            let result = apply_decision(&cfg, &mut client, &mut state, &snap, d);
            assert!(result.filled, "{} {:?}", d.symbol(), result.error);
            assert!(result.error.is_none(), "{} {:?}", d.symbol(), result.error);
        }
        snap.open_positions = state.positions.clone();
        snap.position = state.position.clone();
        snap.live_book = true;
    }
    assert_eq!(client.buys, 3);
    let mut bought = client.bought_symbols.clone();
    bought.sort();
    assert_eq!(bought, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
    let mut held: Vec<String> = state.positions.iter().map(|p| p.symbol.clone()).collect();
    held.sort();
    assert_eq!(held, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
    assert_eq!(client.protects, 3);

    let fourth = apply_live(
        &cfg,
        &mut client,
        &snap,
        &enter_sym("XRPUSDT", "0.6", "0.4"),
        Some(&state),
    );
    assert!(!fourth.filled);
    assert!(
        fourth.error.as_deref().unwrap_or("").contains("book full"),
        "{:?}",
        fourth.error
    );
    assert_eq!(client.buys, 3);
}

#[test]
fn live_tui_loop_fills_three_fastest_alts() {
    let cfg = cfg_live_three();
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        Ticker::new("MORPHOUSDT", d("2.87"), d("9.8"), d("300000")),
        Ticker::new("SPKUSDT", d("0.0225"), d("8.4"), d("200000")),
        Ticker::new("GRASSUSDT", d("0.364"), d("7.1"), d("150000")),
        Ticker::new("BTCUSDT", d("77600"), d("0.8"), d("800000")),
        Ticker::new("ETHUSDT", d("2450"), d("1.6"), d("700000")),
        Ticker::new("SOLUSDT", d("95.4"), d("2.0"), d("200000")),
    ];
    snap.account.wallet_balance = d("10000");
    snap.account.available_balance = d("10000");
    snap.account.starting_equity = d("10000");
    snap.chart_symbol = "MORPHOUSDT".into();
    snap.live_book = true;
    snap.account_ok = true;
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let mut state = EngineState::new(1);
    let mut client = FakeClient::new();
    for step in 0..3 {
        let (new_state, decisions) =
            tick_decisions(&state, &snap, london_ts() + step as f64 * 60.0, Some(&mom), None, None);
        state = new_state;
        let enters: Vec<Decision> = decisions
            .into_iter()
            .filter(|d| matches!(d, Decision::EnterLong { .. }))
            .collect();
        assert_eq!(enters.len(), 1, "step {step}: {enters:?}");
        for d in &enters {
            let result = apply_decision(&cfg, &mut client, &mut state, &snap, d);
            assert!(result.filled, "{} {:?}", d.symbol(), result.error);
        }
        snap.open_positions = state.positions.clone();
        snap.position = state.position.clone();
    }
    let mut bought = client.bought_symbols.clone();
    bought.sort();
    assert_eq!(bought, vec!["GRASSUSDT", "MORPHOUSDT", "SPKUSDT"]);
    assert_eq!(client.buys, 3);
    assert_eq!(client.protects, 3);
}

#[test]
fn live_tui_loop_strategy4_fills_three_liquid() {
    let cfg = cfg_live_three();
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        Ticker::new("BTCUSDT", d("50000"), d("3.0"), d("50000000")),
        Ticker::new("ETHUSDT", d("3000"), d("2.0"), d("40000000")),
        Ticker::new("SOLUSDT", d("140"), d("1.5"), d("20000000")),
        Ticker::new("GPSUSDT", d("0.02"), d("25"), d("100")),
    ];
    snap.account.wallet_balance = d("10000");
    snap.account.available_balance = d("10000");
    snap.account.starting_equity = d("10000");
    snap.chart_symbol = "BTCUSDT".into();
    snap.live_book = true;
    snap.account_ok = true;
    for (sym, px) in [("BTCUSDT", 50000.0), ("ETHUSDT", 3000.0), ("SOLUSDT", 140.0)] {
        let seq = pullback_5m_at(px);
        let last = seq.last().cloned().expect("pullback");
        snap.last_bars.insert(sym.into(), last);
        snap.universe_bars.insert(sym.into(), seq.clone());
        if sym == "BTCUSDT" {
            snap.bars = seq;
        }
    }
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let mut state = EngineState::new(4);
    let mut client = FakeClient::new();
    for step in 0..3 {
        let (new_state, decisions) =
            tick_decisions(&state, &snap, london_ts() + step as f64 * 60.0, Some(&mom), None, None);
        state = new_state;
        let enters: Vec<Decision> = decisions
            .into_iter()
            .filter(|d| matches!(d, Decision::EnterLong { .. }))
            .collect();
        assert_eq!(enters.len(), 1, "step {step}: {enters:?}");
        for d in &enters {
            let result = apply_decision(&cfg, &mut client, &mut state, &snap, d);
            assert!(result.filled, "{} {:?}", d.symbol(), result.error);
        }
        snap.open_positions = state.positions.clone();
        snap.position = state.position.clone();
    }
    let mut bought = client.bought_symbols.clone();
    bought.sort();
    assert_eq!(bought, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
    assert_eq!(client.buys, 3);
    assert_eq!(client.protects, 3);
    let fourth = apply_live(
        &cfg,
        &mut client,
        &snap,
        &enter_sym("XRPUSDT", "0.6", "0.4"),
        Some(&state),
    );
    assert!(!fourth.filled);
    assert!(
        fourth.error.as_deref().unwrap_or("").contains("book full"),
        "{:?}",
        fourth.error
    );
}

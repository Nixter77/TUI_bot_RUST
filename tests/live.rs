//! Drive shipped apply_live: reduce-only protective sizing, watch-mode, keys.

mod common;
use common::*;
use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use tui_bot::config::load_config;
use tui_bot::exchange::{
    buy_client_order_id, prepare_algo_params, prune_stale_protectives, replace_stop_place_first,
    risk_position_notional, sell_protectives_are_sized, size_market_order, size_risk_market_order,
    sized_long_protectives, stale_sell_protective, ExchangeError, FlattenClient, LiveClient,
    SymbolFilters,
};
use tui_bot::live::{rearm_live_protectives, REARM_FAIL_BUDGET_SEC, REARM_FAIL_MAX};
use serde_json::json;
use tui_bot::errors::{COOLDOWN_SEC, RETRY_BACKOFF_SEC};
use tui_bot::journal::{set_active, TradeJournal};
use tui_bot::engine::{tick_decisions, MomentumParams};
use tui_bot::live::{
    apply_decision, apply_live, apply_paper_decision, clear_orphan_protectives, clear_vanished_longs,
    reconcile_live, sweep_rogue_shorts,
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
    pub fail_buy: Option<String>,
    pub fill_on_buy_fail: bool,
    pub fail_algo: bool,
    pub fail_replace: bool,
    pub fail_risk: bool,
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
            fail_buy: None,
            fill_on_buy_fail: false,
            fail_algo: false,
            fail_replace: false,
            fail_risk: false,
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
        if self.fail_risk {
            return Err(ExchangeError("HTTP 502 /fapi/v2/positionRisk: gateway".into()));
        }
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
        if let Some(msg) = &self.fail_buy {
            if self.fill_on_buy_fail {
                self.risk = json!([{
                    "symbol": symbol,
                    "positionAmt": qty.to_string(),
                    "entryPrice": "1000",
                    "unRealizedProfit": "0",
                }]);
            }
            return Err(ExchangeError(msg.clone()));
        }
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
        if self.fail_replace {
            return Err(ExchangeError("HTTP 502 /fapi/v1/algoOrder: gateway".into()));
        }
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
        if self.fail_algo {
            return Err(ExchangeError("algo timeout".into()));
        }
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
fn fill_then_protect_failure_flattens_naked() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_protect = true;
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert!(!result.filled);
    assert!(
        result.error.as_deref().unwrap_or("").contains("flattened naked fill"),
        "{:?}",
        result.error
    );
    assert_eq!(client.buys, 1);
    assert_eq!(client.protects, 1);
    assert_eq!(client.closes.len(), 1);
    assert_eq!(client.closes[0].0, "BTCUSDT");
    assert_eq!(client.closes[0].1, "LONG");
    assert_eq!(result.forget_symbol, "BTCUSDT");
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
fn skip_enter_when_live_book_has_long_but_snapshot_is_flat() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.risk = json!([{
        "symbol": "BTCUSDT",
        "positionAmt": "0.01",
        "entryPrice": "50000",
        "unRealizedProfit": "0",
    }]);
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert!(!result.filled);
    assert!(
        result.error.as_deref().unwrap_or("").contains("already in position"),
        "{:?}",
        result.error
    );
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

#[test]
fn risk_pct_notional_is_equity_times_pct_over_stop_distance() {
    // 0.25% of 3100 equity, 2% SL → risk_usdt=7.75, notional=387.5
    let plan = risk_position_notional(d("3100"), d("0.0025"), d("100000"), d("98000")).unwrap();
    assert_eq!(plan.risk_usdt, d("7.75"));
    assert_eq!(plan.dist, d("2000"));
    assert_eq!(plan.notional, d("387.5"));
}

#[test]
fn risk_size_btc_min_does_not_bump_past_risk() {
    // BTC TestNet minNotional comes from exchangeInfo (50), never hardcoded 100.
    let filters = SymbolFilters {
        tick_size: d("0.1"),
        step_size: d("0.001"),
        min_qty: d("0.001"),
        min_notional: d("50"),
    };
    let equity = d("3100");
    let risk_pct = d("0.0025");
    let entry = d("100000");
    let sl = d("98000");
    let plan = risk_position_notional(equity, risk_pct, entry, sl).unwrap();
    assert_eq!(plan.notional, d("387.5"));
    assert!(plan.notional > filters.min_notional);
    let qty = size_market_order(plan.notional, entry, &filters).unwrap();
    assert!(qty * entry >= filters.min_notional);
    assert!(
        qty * plan.dist <= plan.risk_usdt,
        "qty*dist={} risk={}",
        qty * plan.dist,
        plan.risk_usdt
    );
    let sized = size_risk_market_order(equity, risk_pct, entry, sl, &filters)
        .unwrap()
        .expect("387.5 already above BTC min 50");
    assert_eq!(sized, qty);
    assert!(sized * entry < d("1000"), "must not bump toward a 100 USDT floor");
}

#[test]
fn risk_size_skips_when_min_notional_would_inflate_risk() {
    let filters = SymbolFilters {
        tick_size: d("0.1"),
        step_size: d("0.001"),
        min_qty: d("0.001"),
        min_notional: d("50"),
    };
    // Tiny wallet: risk_usdt=0.25, 2% SL → raw notional=12.5 < BTC min 50.
    let qty = size_risk_market_order(d("100"), d("0.0025"), d("100000"), d("98000"), &filters).unwrap();
    assert!(qty.is_none(), "minNotional 50 would spend 1 USDT at 2% SL > 0.25 risk");
}

#[test]
fn s4_live_risk_pct_fills_387_and_does_not_bump() {
    let mut env = HashMap::new();
    env.insert("BINANCE_API_KEY".into(), "A".repeat(32));
    env.insert("BINANCE_API_SECRET".into(), "B".repeat(32));
    let cfg = load_config(true, None, Some(&env)).unwrap();
    assert_eq!(cfg.risk_pct, d("0.0025"));
    let mut client = FakeClient::new();
    client.min_notional = d("50");
    let mut s = snap(None);
    s.tickers = vec![Ticker::new("BTCUSDT", d("100000"), d("1"), d("1"))];
    s.account.wallet_balance = d("3000");
    s.account.unrealized_pnl = d("100");
    s.account.available_balance = d("50");
    s.account.starting_equity = d("3100");
    let state = EngineState::new(4);
    let result = apply_live(
        &cfg,
        &mut client,
        &s,
        &Decision::EnterLong {
            symbol: "BTCUSDT".into(),
            reason: "x".into(),
            take_profit: d("104000"),
            stop_loss: d("98000"),
        },
        Some(&state),
    );
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(result.filled);
    let qty = client.bought[0];
    assert!(qty * d("2000") <= d("7.75"), "qty*dist must stay inside risk_usdt");
    assert!(qty * d("100000") < d("1000"), "must not bump past risk toward a 100 floor");
}

#[test]
fn s4_live_skips_symbol_when_min_notional_inflates_risk() {
    let mut env = HashMap::new();
    env.insert("BINANCE_API_KEY".into(), "A".repeat(32));
    env.insert("BINANCE_API_SECRET".into(), "B".repeat(32));
    let cfg = load_config(true, None, Some(&env)).unwrap();
    let mut client = FakeClient::new();
    client.min_notional = d("50");
    let mut s = snap(None);
    s.tickers = vec![Ticker::new("BTCUSDT", d("100000"), d("1"), d("1"))];
    s.account.wallet_balance = d("100");
    s.account.unrealized_pnl = Decimal::ZERO;
    s.account.available_balance = d("100");
    s.account.starting_equity = d("100");
    let mut state = EngineState::new(4);
    let decision = Decision::EnterLong {
        symbol: "BTCUSDT".into(),
        reason: "x".into(),
        take_profit: d("104000"),
        stop_loss: d("98000"),
    };
    let result = apply_decision(&cfg, &mut client, &mut state, &s, &decision);
    assert!(!result.filled);
    assert_eq!(client.buys, 0);
    assert!(
        result.error.as_deref().unwrap_or("").contains("inflates risk"),
        "{:?}",
        result.error
    );
    assert!(
        state.skip_symbols.iter().any(|s| s.eq_ignore_ascii_case("BTCUSDT")),
        "skip the symbol, got {:?}",
        state.skip_symbols
    );
}

#[test]
fn s4_live_risk_pct_zero_falls_back_to_order_notional() {
    let mut env = HashMap::new();
    env.insert("BINANCE_API_KEY".into(), "A".repeat(32));
    env.insert("BINANCE_API_SECRET".into(), "B".repeat(32));
    env.insert("RISK_PCT".into(), "0".into());
    env.insert("ORDER_NOTIONAL_USDT".into(), "20".into());
    let cfg = load_config(true, None, Some(&env)).unwrap();
    assert_eq!(cfg.risk_pct, Decimal::ZERO);
    let mut client = FakeClient::new();
    client.min_notional = d("50");
    let mut s = snap(None);
    s.tickers = vec![Ticker::new("BTCUSDT", d("100000"), d("1"), d("1"))];
    let state = EngineState::new(4);
    let result = apply_live(
        &cfg,
        &mut client,
        &s,
        &Decision::EnterLong {
            symbol: "BTCUSDT".into(),
            reason: "x".into(),
            take_profit: d("104000"),
            stop_loss: d("98000"),
        },
        Some(&state),
    );
    assert!(result.filled, "{:?}", result.error);
    // ORDER_NOTIONAL 20 bumps to exchange min 50 (legacy path).
    assert_eq!(client.bought, vec![d("0.001")]);
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
    assert!(state.recent_actions.iter().any(|a| a.text.contains("чужой шорт")));
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
    assert!(state.recent_actions.iter().any(|a| a.text.contains("снял TP/SL")));
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
fn reconcile_does_not_freeze_desk_after_short_sweep() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let short = short_pos("BTCUSDT", "0.004", "68600", "-2");
    let mut snap = live_book(None, vec![short]);
    snap.chart_symbol = "ETHUSDT".into();
    let mut state = EngineState::new(2);
    let rec = reconcile_live(&cfg, &mut client, &mut state, &snap, Some(1_700_000_000.0));
    assert!(!rec.skip_tick, "cleanup must not freeze entries: {rec:?}");
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
    assert!(state.recent_actions.iter().any(|a| a.text.contains("сиротский стоп")));
}

#[test]
fn reconcile_cleans_orphan_stop_without_freezing_entries() {
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
    assert!(!rec.skip_tick, "orphan cancel must not freeze entries: {rec:?}");
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
    env.insert("STRATEGY4_MAX_POSITIONS".into(), "3".into());
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
        Ticker::new("AVAXUSDT", d("50"), d("3.0"), d("50000000")),
        Ticker::new("LINKUSDT", d("20"), d("2.0"), d("40000000")),
        Ticker::new("DOTUSDT", d("10"), d("1.5"), d("20000000")),
        Ticker::new("GPSUSDT", d("0.02"), d("25"), d("100")),
    ];
    snap.account.wallet_balance = d("10000");
    snap.account.available_balance = d("10000");
    snap.account.starting_equity = d("10000");
    snap.chart_symbol = "AVAXUSDT".into();
    snap.live_book = true;
    snap.account_ok = true;
    for (sym, px) in [("AVAXUSDT", 50.0), ("LINKUSDT", 20.0), ("DOTUSDT", 10.0)] {
        let seq = pullback_5m_at(px);
        let last = seq.last().cloned().expect("pullback");
        snap.last_bars.insert(sym.into(), last);
        snap.universe_bars.insert(sym.into(), seq.clone());
        snap.htf_bars.insert(sym.into(), htf_up_4h_at(px));
        if sym == "AVAXUSDT" {
            snap.bars = seq;
        }
    }
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        s4_max_positions: 3,
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
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
        for p in &mut state.positions {
            p.unrealized_pnl = d("1");
        }
        snap.open_positions = state.positions.clone();
        snap.position = state.position.clone();
    }
    let mut bought = client.bought_symbols.clone();
    bought.sort();
    assert_eq!(bought, vec!["AVAXUSDT", "DOTUSDT", "LINKUSDT"]);
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

#[test]
fn buy_timeout_after_fill_still_places_protectives() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_buy = Some("HTTP /fapi/v1/order: timeout".into());
    client.fill_on_buy_fail = true;
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert!(result.filled, "{:?}", result.error);
    assert_eq!(client.buys, 1);
    assert_eq!(client.protects, 1);
    assert!(client.closes.is_empty());
}

#[test]
fn buy_error_without_fill_does_not_place_protectives() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_buy = Some("HTTP 400 /fapi/v1/order: {\"code\":-2010}".into());
    let result = apply_live(&cfg, &mut client, &snap(None), &enter(), None);
    assert!(!result.filled);
    assert!(result.error.is_some());
    assert_eq!(client.protects, 0);
}

#[test]
fn transport_timeout_on_enter_sets_retry_backoff() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_buy = Some("HTTP 408 /fapi/v1/order: gateway".into());
    let mut state = EngineState::new(1);
    let before = tui_bot::sessions::unix_now();
    apply_decision(&cfg, &mut client, &mut state, &snap(None), &enter());
    assert!(
        state.retry_until >= before + RETRY_BACKOFF_SEC - 1.0,
        "retry_until={}",
        state.retry_until
    );
    assert_eq!(client.buys, 1);
    assert_eq!(state.retry_strikes, 1);
    apply_decision(&cfg, &mut client, &mut state, &snap(None), &enter());
    assert_eq!(state.retry_strikes, 2);
    assert!(
        state.retry_until >= before + 40.0 - 1.0,
        "second retry should back off ~40s, retry_until={}",
        state.retry_until
    );
}

#[test]
fn enter_without_position_risk_does_not_buy() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_risk = true;
    let mut state = EngineState::new(1);
    let before = tui_bot::sessions::unix_now();
    let result = apply_decision(&cfg, &mut client, &mut state, &snap(None), &enter());
    assert!(!result.filled);
    assert_eq!(client.buys, 0);
    assert!(
        result.error.as_deref().unwrap_or("").contains("снимка позиций"),
        "{:?}",
        result.error
    );
    assert!(
        state.retry_until >= before + RETRY_BACKOFF_SEC - 1.0,
        "502 book must back off entries, retry_until={}",
        state.retry_until
    );
}

#[test]
fn exit_when_live_book_already_flat_does_not_journal_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trades.jsonl");
    set_active(Some(path.clone()));
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let long = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let mut snap = live_book(None, vec![]);
    snap.live_book = true;
    let mut state = EngineState::new(2);
    state.positions = vec![long];
    apply_decision(
        &cfg,
        &mut client,
        &mut state,
        &snap,
        &Decision::ExitPosition {
            symbol: "BTCUSDT".into(),
            reason: "continuation stop loss".into(),
        },
    );
    let events = TradeJournal::new(Some(&path)).read_events();
    set_active(None);
    assert!(
        !events.iter().any(|e| e.event == "close"
            && e.symbol.to_ascii_uppercase().contains("BTCUSDT")
            && e.reason.contains("continuation stop")),
        "duplicate close would double-count PnL: {events:?}"
    );
    assert!(client.closes.is_empty());
}

#[test]
fn rearm_replaces_when_sized_stops_lie_and_algos_are_gone() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let live = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let snap = live_book(Some(live.clone()), vec![live.clone()]);
    let mut state = EngineState::new(2);
    state.sized_stops.insert("BTCUSDT".into());
    state.positions = vec![live];
    let done = rearm_live_protectives(&cfg, &mut client, &mut state, &snap);
    assert_eq!(done, vec!["BTCUSDT".to_string()]);
    assert_eq!(client.replaces.len(), 1);
}

#[test]
fn rearm_skips_when_sized_protectives_are_on_the_book() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.algo_orders = vec![
        json!({"symbol":"BTCUSDT","side":"SELL","orderType":"STOP_MARKET","quantity":"0.02","closePosition":false}),
        json!({"symbol":"BTCUSDT","side":"SELL","orderType":"TAKE_PROFIT_MARKET","quantity":"0.02","closePosition":false}),
    ];
    let live = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let snap = live_book(Some(live.clone()), vec![live]);
    let mut state = EngineState::new(2);
    let done = rearm_live_protectives(&cfg, &mut client, &mut state, &snap);
    assert!(done.is_empty());
    assert!(client.replaces.is_empty());
    assert!(state.sized_stops.contains("BTCUSDT"));
}

#[test]
fn amend_error_clears_sized_stops_so_rearm_can_run() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_replace = true;
    let long = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let mut snap = live_book(Some(long.clone()), vec![long.clone()]);
    snap.tickers = vec![Ticker::new("BTCUSDT", d("1010"), d("1"), d("10"))];
    let mut state = EngineState::new(2);
    state.positions = vec![long.clone()];
    state.position = Some(long);
    state.sized_stops.insert("BTCUSDT".into());
    let decision = Decision::AmendStop {
        symbol: "BTCUSDT".into(),
        stop_loss: d("995"),
        reason: "trail".into(),
    };
    let result = apply_decision(&cfg, &mut client, &mut state, &snap, &decision);
    assert!(result.error.is_some(), "{:?}", result.error);
    assert!(!state.sized_stops.contains("BTCUSDT"));
}

#[test]
fn amend_success_writes_stop_into_state() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let long = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let mut snap = live_book(Some(long.clone()), vec![long.clone()]);
    snap.tickers = vec![Ticker::new("BTCUSDT", d("1015"), d("1"), d("10"))];
    let mut state = EngineState::new(4);
    state.positions = vec![long.clone()];
    state.position = Some(long);
    let decision = Decision::AmendStop {
        symbol: "BTCUSDT".into(),
        stop_loss: d("1000.8"),
        reason: "безубыток на 1R".into(),
    };
    let result = apply_decision(&cfg, &mut client, &mut state, &snap, &decision);
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(state.positions[0].stop_loss, Some(d("1000.8")));
    assert_eq!(state.position.as_ref().unwrap().stop_loss, Some(d("1000.8")));
}

#[test]
fn amend_be_failure_flattens_long() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_replace = true;
    let long = Position::long("VVVUSDT", d("22.60"), d("17.055"), Some(d("16.7139")), Some(d("17.751")));
    let mut snap = live_book(Some(long.clone()), vec![long.clone()]);
    snap.tickers = vec![Ticker::new("VVVUSDT", d("17.40"), d("1"), d("10"))];
    let mut state = EngineState::new(4);
    state.positions = vec![long.clone()];
    state.position = Some(long);
    let decision = Decision::AmendStop {
        symbol: "VVVUSDT".into(),
        stop_loss: d("17.068"),
        reason: "безубыток на 1R".into(),
    };
    let result = apply_decision(&cfg, &mut client, &mut state, &snap, &decision);
    assert!(
        result.error.as_deref().unwrap_or("").contains("flattened naked fill"),
        "{:?}",
        result.error
    );
    assert_eq!(client.closes.len(), 1);
    assert_eq!(client.closes[0].0, "VVVUSDT");
    assert_eq!(result.forget_symbol, "VVVUSDT");
}

#[test]
fn buy_client_order_id_is_binance_safe() {
    let id = buy_client_order_id("BTCUSDT", 1_700_000_000_123);
    assert!(id.len() <= 36);
    assert!(id.starts_with("tui"));
    assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':' || c == '.'));
}

#[test]
fn buy_client_order_id_is_stable_inside_retry_bucket() {
    let t = 1_700_000_000_000i64;
    let a = buy_client_order_id("BTCUSDT", t);
    let b = buy_client_order_id("BTCUSDT", t + 19_999);
    let c = buy_client_order_id("BTCUSDT", t + 20_000);
    assert_eq!(a, b, "same 20s bucket must reuse clientOrderId");
    assert_ne!(a, c, "next bucket must not collide");
}

struct PlaceFirstClient {
    protects: usize,
    protect_cancels: Vec<String>,
    algo_cancels: Vec<String>,
    order_cancels: Vec<i64>,
    algo_orders: Vec<Value>,
    open_orders: Vec<Value>,
}

impl PlaceFirstClient {
    fn new() -> Self {
        Self {
            protects: 0,
            protect_cancels: Vec::new(),
            algo_cancels: Vec::new(),
            order_cancels: Vec::new(),
            algo_orders: Vec::new(),
            open_orders: Vec::new(),
        }
    }
}

impl FlattenClient for PlaceFirstClient {
    fn cancel_protectives(&mut self, symbol: &str) -> Result<(), ExchangeError> {
        self.protect_cancels.push(symbol.into());
        Ok(())
    }
    fn market_close(&mut self, _symbol: &str, _side: &str, _qty: Decimal) -> Result<(), ExchangeError> {
        Ok(())
    }
    fn position_risk(&mut self) -> Result<Value, ExchangeError> {
        Ok(Value::Array(vec![]))
    }
}

impl LiveClient for PlaceFirstClient {
    fn filters_for(&mut self, _symbol: &str) -> Result<SymbolFilters, ExchangeError> {
        Ok(SymbolFilters {
            tick_size: d("0.1"),
            step_size: d("0.001"),
            min_qty: d("0.001"),
            min_notional: d("5"),
        })
    }
    fn market_buy(&mut self, _symbol: &str, _qty: Decimal) -> Result<(), ExchangeError> {
        Ok(())
    }
    fn place_tp_sl(
        &mut self,
        _symbol: &str,
        _tp: Decimal,
        _sl: Decimal,
        _qty: Option<Decimal>,
    ) -> Result<(), ExchangeError> {
        self.protects += 1;
        Ok(())
    }
    fn replace_stop(
        &mut self,
        symbol: &str,
        stop_loss: Decimal,
        take_profit: Option<Decimal>,
        qty: Option<Decimal>,
    ) -> Result<(), ExchangeError> {
        let tp = take_profit.ok_or_else(|| ExchangeError("missing tp".into()))?;
        replace_stop_place_first(self, symbol, stop_loss, tp, qty)
    }
    fn cancel_algo_order(&mut self, _symbol: &str, algo_id: &str) -> Result<(), ExchangeError> {
        self.algo_cancels.push(algo_id.into());
        Ok(())
    }
    fn cancel_plain_order(&mut self, _symbol: &str, order_id: i64) -> Result<(), ExchangeError> {
        self.order_cancels.push(order_id);
        Ok(())
    }
    fn open_algo_orders(&mut self, symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        Ok(filter_symbol_rows(&self.algo_orders, symbol))
    }
    fn open_orders(&mut self, symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        Ok(filter_symbol_rows(&self.open_orders, symbol))
    }
}

#[test]
fn stale_sell_protective_detects_old_stop() {
    let old = json!({"side":"SELL","orderType":"STOP_MARKET","triggerPrice":"990"});
    let fresh = json!({"side":"SELL","orderType":"STOP_MARKET","triggerPrice":"1000"});
    let tp = json!({"side":"SELL","orderType":"TAKE_PROFIT_MARKET","triggerPrice":"1030"});
    assert!(stale_sell_protective(&old, d("1000"), d("1030")));
    assert!(!stale_sell_protective(&fresh, d("1000"), d("1030")));
    assert!(!stale_sell_protective(&tp, d("1000"), d("1030")));
}

#[test]
fn replace_stop_places_before_cancelling_old_pair() {
    let mut client = PlaceFirstClient::new();
    client.algo_orders = vec![
        json!({"symbol":"BTCUSDT","side":"SELL","orderType":"STOP_MARKET","triggerPrice":"990","algoId":1,"quantity":"0.02"}),
        json!({"symbol":"BTCUSDT","side":"SELL","orderType":"TAKE_PROFIT_MARKET","triggerPrice":"1030","algoId":2,"quantity":"0.02"}),
        json!({"symbol":"BTCUSDT","side":"SELL","orderType":"STOP_MARKET","triggerPrice":"1000","algoId":3,"quantity":"0.02"}),
    ];
    replace_stop_place_first(&mut client, "BTCUSDT", d("1000"), d("1030"), Some(d("0.02")))
        .unwrap();
    assert_eq!(client.protects, 1, "new pair must be placed first");
    assert!(
        client.protect_cancels.is_empty(),
        "must not cancel-all (naked long window): {:?}",
        client.protect_cancels
    );
    assert_eq!(client.algo_cancels, vec!["1".to_string()]);
}

#[test]
fn prune_keeps_matching_tp_and_drops_old_stop() {
    let mut client = PlaceFirstClient::new();
    client.algo_orders = vec![
        json!({"symbol":"ETHUSDT","side":"SELL","orderType":"STOP_MARKET","triggerPrice":"2900","algoId":"old","quantity":"0.01"}),
        json!({"symbol":"ETHUSDT","side":"SELL","orderType":"TAKE_PROFIT_MARKET","triggerPrice":"3200","algoId":"tp","quantity":"0.01"}),
    ];
    prune_stale_protectives(&mut client, "ETHUSDT", d("3000"), d("3200"));
    assert_eq!(client.algo_cancels, vec!["old".to_string()]);
}

#[test]
fn rearm_replaces_when_algo_list_errors() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_algo = true;
    let live = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let snap = live_book(Some(live.clone()), vec![live.clone()]);
    let mut state = EngineState::new(2);
    state.sized_stops.insert("BTCUSDT".into());
    state.positions = vec![live];
    let done = rearm_live_protectives(&cfg, &mut client, &mut state, &snap);
    assert_eq!(done, vec!["BTCUSDT".to_string()]);
    assert_eq!(client.replaces.len(), 1);
}

#[test]
fn reduce_long_closes_half_and_amends_be_remainder() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let long = Position::long("AVAXUSDT", d("0.02"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = live_book(Some(long.clone()), vec![long.clone()]);
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("1"), d("10"))];
    let mut state = EngineState::new(4);
    state.positions = vec![long.clone()];
    state.position = Some(long);
    let decision = Decision::ReduceLong {
        symbol: "AVAXUSDT".into(),
        reason: "частичная фиксация 50% на 1R".into(),
        qty: d("0.01"),
        stop_loss: d("100.08"),
    };
    let result = apply_decision(&cfg, &mut client, &mut state, &snap, &decision);
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(result.filled);
    assert_eq!(client.closes.len(), 1);
    assert_eq!(client.closes[0], ("AVAXUSDT".into(), "LONG".into(), d("0.01")));
    assert_eq!(client.replaces.len(), 1);
    assert_eq!(client.replaces[0].1, d("100.08"));
    assert_eq!(client.replace_qty, Some(d("0.01")));
    assert_eq!(state.positions[0].qty, d("0.01"));
    assert_eq!(state.positions[0].stop_loss, Some(d("100.08")));
    assert!(state.scaled_one_r.contains("AVAXUSDT"));
}

#[test]
fn reduce_long_full_qty_falls_through_to_be_amend_and_latches() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    let long = Position::long("AVAXUSDT", d("0.02"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = live_book(Some(long.clone()), vec![long.clone()]);
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("1"), d("10"))];
    let mut state = EngineState::new(4);
    state.positions = vec![long.clone()];
    state.position = Some(long);
    let decision = Decision::ReduceLong {
        symbol: "AVAXUSDT".into(),
        reason: "частичная фиксация 1R".into(),
        qty: d("0.02"),
        stop_loss: d("100.08"),
    };
    let result = apply_decision(&cfg, &mut client, &mut state, &snap, &decision);
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(client.closes.is_empty(), "must not market-close the whole long");
    assert_eq!(client.replaces.len(), 1);
    assert_eq!(client.replaces[0].1, d("100.08"));
    assert_eq!(state.positions[0].qty, d("0.02"), "qty unchanged on BE-only path");
    assert_eq!(state.positions[0].stop_loss, Some(d("100.08")));
    assert!(
        state.scaled_one_r.contains("AVAXUSDT"),
        "BE-only reduce must latch scaled_one_r: {:?}",
        state.scaled_one_r
    );
}

#[test]
fn rearm_flattens_after_fail_budget_exhausted() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_replace = true;
    let live = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let snap = live_book(Some(live.clone()), vec![live.clone()]);
    let mut state = EngineState::new(4);
    state.positions = vec![live];
    state.sized_stops.insert("BTCUSDT".into());
    // Miss observed longer than budget ago → fail-closed flatten on this attempt.
    state.rearm_miss_since.insert(
        "BTCUSDT".into(),
        1.0, // far in the past vs unix_now()
    );
    let done = rearm_live_protectives(&cfg, &mut client, &mut state, &snap);
    assert_eq!(done, vec!["BTCUSDT".to_string()]);
    assert_eq!(client.closes.len(), 1);
    assert_eq!(client.closes[0].0, "BTCUSDT");
    assert!(state.positions.is_empty(), "{:?}", state.positions);
    assert!(!state.rearm_miss_since.contains_key("BTCUSDT"));
    assert!(
        state
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("rearm budget exhausted"),
        "{:?}",
        state.last_error
    );
    let _ = REARM_FAIL_BUDGET_SEC;
}

#[test]
fn rearm_does_not_flatten_before_budget() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_replace = true;
    let live = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let snap = live_book(Some(live.clone()), vec![live.clone()]);
    let mut state = EngineState::new(4);
    state.positions = vec![live];
    // First miss: budget just starting — hold naked briefly, do not flatten yet.
    let done = rearm_live_protectives(&cfg, &mut client, &mut state, &snap);
    assert!(done.is_empty(), "{done:?}");
    assert!(client.closes.is_empty());
    assert!(state.rearm_miss_since.contains_key("BTCUSDT"));
    assert!(!state.positions.is_empty());
}



#[test]
fn rearm_flattens_after_three_failed_attempts() {
    let cfg = cfg_live();
    let mut client = FakeClient::new();
    client.fail_replace = true;
    let live = Position::long("BTCUSDT", d("0.02"), d("1000"), Some(d("990")), Some(d("1030")));
    let snap = live_book(Some(live.clone()), vec![live.clone()]);
    let mut state = EngineState::new(4);
    state.positions = vec![live];
    // Two prior fails already counted; third fail on this call exhausts REARM_FAIL_MAX.
    state.rearm_miss_since.insert("BTCUSDT".into(), tui_bot::sessions::unix_now());
    state.rearm_fail_count.insert("BTCUSDT".into(), REARM_FAIL_MAX.saturating_sub(1));
    let done = rearm_live_protectives(&cfg, &mut client, &mut state, &snap);
    assert_eq!(done, vec!["BTCUSDT".to_string()]);
    assert_eq!(client.closes.len(), 1);
    assert!(state.positions.is_empty(), "{:?}", state.positions);
    assert!(!state.rearm_fail_count.contains_key("BTCUSDT"));
    let _ = REARM_FAIL_BUDGET_SEC; // keep import live when only count path used
}


#[test]
fn paper_reduce_long_halves_qty_sets_be_and_latch() {
    let long = Position::long("AVAXUSDT", d("0.02"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("2.0"), d("50000000"))];
    snap.position = Some(long.clone());
    snap.open_positions = vec![long.clone()];
    snap.live_book = false;
    snap.account_ok = true;
    let mut state = EngineState::new(4);
    state.positions = vec![long.clone()];
    state.position = Some(long);
    let decision = Decision::ReduceLong {
        symbol: "AVAXUSDT".into(),
        reason: "частичная фиксация 1R".into(),
        qty: d("0.01"),
        stop_loss: d("100.08"),
    };
    apply_paper_decision(&mut state, &snap, &decision);
    assert_eq!(state.positions.len(), 1);
    assert_eq!(state.positions[0].qty, d("0.01"), "qty should be halved");
    assert_eq!(state.positions[0].stop_loss, Some(d("100.08")), "BE on remainder");
    assert!(
        state.scaled_one_r.contains("AVAXUSDT"),
        "scaled_one_r latch missing: {:?}",
        state.scaled_one_r
    );
    // Second paper reduce must no-op on already-halved book if decision repeats with half qty
    // of original — latch is what strategy uses; state qty stays half.
    assert_eq!(state.position.as_ref().map(|p| p.qty), Some(d("0.01")));
}

#[test]
fn paper_path_after_1r_tick_halves_and_be() {
    let pos = Position::long("AVAXUSDT", d("0.02"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("2.0"), d("50000000"))];
    snap.account_ok = true;
    snap.live_book = false;
    snap.position = Some(pos.clone());
    snap.open_positions = vec![pos.clone()];
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (mut new_state, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    let reduce = decisions
        .iter()
        .find(|d| matches!(d, Decision::ReduceLong { .. }))
        .cloned()
        .expect(&format!("expected ReduceLong, got {decisions:?}"));
    apply_paper_decision(&mut new_state, &snap, &reduce);
    let left = new_state
        .positions
        .iter()
        .find(|p| p.symbol == "AVAXUSDT")
        .expect("remainder");
    assert_eq!(left.qty, d("0.01"), "paper qty halved after 1R");
    assert!(left.stop_loss.unwrap() >= d("100"), "BE {left:?}");
    assert!(new_state.scaled_one_r.contains("AVAXUSDT"));
}


#[test]
fn paper_reduce_then_tick_does_not_re_reduce() {
    let pos = Position::long("AVAXUSDT", d("0.02"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("2.0"), d("50000000"))];
    snap.account_ok = true;
    snap.live_book = false;
    snap.position = Some(pos.clone());
    snap.open_positions = vec![pos.clone()];
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (mut st, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    let reduce = decisions
        .iter()
        .find(|d| matches!(d, Decision::ReduceLong { .. }))
        .cloned()
        .expect(&format!("expected ReduceLong, got {decisions:?}"));
    apply_paper_decision(&mut st, &snap, &reduce);
    // Refresh snapshot to the reduced book so manage sees half qty + BE.
    let left = st.positions.iter().find(|p| p.symbol == "AVAXUSDT").unwrap().clone();
    snap.position = Some(left.clone());
    snap.open_positions = vec![left];
    let (st2, again) = tick_decisions(&st, &snap, london_ts() + 60.0, None, None, None);
    assert!(
        !again.iter().any(|d| matches!(d, Decision::ReduceLong { .. })),
        "latched paper book must not ReduceLong again: {again:?}"
    );
    assert!(st2.scaled_one_r.contains("AVAXUSDT"));
}

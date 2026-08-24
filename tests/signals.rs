//! Buy and sell chimes are distinct; play is silent unless enabled.

use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tui_bot::config::load_config;
use tui_bot::exchange::{ExchangeError, FlattenClient, LiveClient, SymbolFilters};
use tui_bot::flatten::FlattenResult;
use tui_bot::live::{apply_decision, LiveApplyResult};
use tui_bot::models::{Decision, EngineState, MarketSnapshot, Position, Ticker};
use tui_bot::signals::{
    chime_paths, emit_decision, emit_flatten, kind_for_decision, kind_for_flatten, play, set_enabled,
    set_sink, signals_enabled, write_chime, TradeSignal, BUY_HZ, SELL_HZ,
};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

static LOCK: Mutex<()> = Mutex::new(());

fn guard() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn reset() {
    set_enabled(false);
    set_sink(None);
}

struct FakeClient;

impl FlattenClient for FakeClient {
    fn cancel_protectives(&mut self, _symbol: &str) -> Result<(), ExchangeError> {
        Ok(())
    }
    fn market_close(&mut self, _symbol: &str, _side: &str, _qty: Decimal) -> Result<(), ExchangeError> {
        Ok(())
    }
    fn position_risk(&mut self) -> Result<Value, ExchangeError> {
        Ok(Value::Array(vec![]))
    }
}

impl LiveClient for FakeClient {
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
        Ok(())
    }
    fn replace_stop(
        &mut self,
        _symbol: &str,
        _stop_loss: Decimal,
        _take_profit: Option<Decimal>,
        _qty: Option<Decimal>,
    ) -> Result<(), ExchangeError> {
        Ok(())
    }
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
    s.position = position.clone();
    if let Some(p) = position {
        s.open_positions = vec![p];
    }
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
fn live_buy_only_after_fill() {
    let _g = guard();
    reset();
    let enter = enter();
    assert_eq!(
        kind_for_decision(&enter, &LiveApplyResult { filled: true, ..Default::default() }, true, false),
        Some(TradeSignal::Buy)
    );
    assert_eq!(
        kind_for_decision(&enter, &LiveApplyResult::default(), true, false),
        None
    );
    assert_eq!(
        kind_for_decision(&enter, &LiveApplyResult::default(), false, false),
        Some(TradeSignal::Buy)
    );
}

#[test]
fn sell_on_exit_and_flatten_not_on_hold() {
    let _g = guard();
    reset();
    let exit_d = Decision::ExitPosition {
        reason: "momentum take profit".into(),
        symbol: "BTCUSDT".into(),
    };
    assert_eq!(
        kind_for_decision(&exit_d, &LiveApplyResult::default(), true, true),
        Some(TradeSignal::Sell)
    );
    assert_eq!(
        kind_for_decision(&exit_d, &LiveApplyResult::default(), true, false),
        None
    );
    assert_eq!(
        kind_for_decision(
            &exit_d,
            &LiveApplyResult {
                error: Some("nope".into()),
                ..Default::default()
            },
            true,
            true
        ),
        None
    );
    assert_eq!(
        kind_for_flatten(&FlattenResult {
            closed: vec!["LONG BTCUSDT".into()],
            errors: vec![],
        }),
        Some(TradeSignal::Sell)
    );
    assert_eq!(kind_for_flatten(&FlattenResult::default()), None);
    assert_eq!(
        kind_for_decision(&Decision::hold("wait"), &LiveApplyResult::default(), false, false),
        None
    );
    assert_eq!(
        kind_for_decision(
            &Decision::AmendStop {
                stop_loss: d("1"),
                reason: "trail".into(),
                symbol: "BTCUSDT".into(),
            },
            &LiveApplyResult::default(),
            true,
            true
        ),
        None
    );
    assert_ne!(TradeSignal::Buy, TradeSignal::Sell);
    assert!(BUY_HZ.0 > SELL_HZ.0);
    assert!(BUY_HZ.1 > BUY_HZ.0);
    assert!(SELL_HZ.1 < SELL_HZ.0);
}

#[test]
fn chimes_are_different_wavs() {
    let _g = guard();
    reset();
    let root = tempfile::tempdir().unwrap();
    let buy = write_chime(&root.path().join("buy.wav"), &[BUY_HZ.0, BUY_HZ.1], 22_050).unwrap();
    let sell = write_chime(&root.path().join("sell.wav"), &[SELL_HZ.0, SELL_HZ.1], 22_050).unwrap();
    let buy_bytes = std::fs::read(&buy).unwrap();
    let sell_bytes = std::fs::read(&sell).unwrap();
    assert_ne!(buy_bytes, sell_bytes);
    assert!(buy_bytes.starts_with(b"RIFF"));
    assert!(buy_bytes.len() > 200);
    let (p_buy, p_sell) = chime_paths().unwrap();
    assert_ne!(std::fs::read(p_buy).unwrap(), std::fs::read(p_sell).unwrap());
}

#[test]
fn play_uses_sink_when_enabled_and_is_silent_by_default() {
    let _g = guard();
    reset();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    set_sink(Some(Arc::new(move |k| h.lock().unwrap().push(k))));
    assert!(!signals_enabled());
    assert!(!play(TradeSignal::Buy, None));
    assert!(heard.lock().unwrap().is_empty());
    set_enabled(true);
    assert!(play(TradeSignal::Buy, None));
    assert!(play(TradeSignal::Sell, None));
    assert_eq!(
        *heard.lock().unwrap(),
        vec![TradeSignal::Buy, TradeSignal::Sell]
    );
    reset();
}

#[test]
fn apply_decision_emits_buy_on_fill() {
    let _g = guard();
    reset();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    set_sink(Some(Arc::new(move |k| h.lock().unwrap().push(k))));
    set_enabled(true);
    let cfg = cfg_live();
    let mut state = EngineState::new(1);
    apply_decision(&cfg, &mut FakeClient, &mut state, &snap(None), &enter());
    assert_eq!(*heard.lock().unwrap(), vec![TradeSignal::Buy]);
    heard.lock().unwrap().clear();
    let pos = Position::long("BTCUSDT", d("0.01"), d("1000"), Some(d("990")), Some(d("1020")));
    apply_decision(
        &cfg,
        &mut FakeClient,
        &mut state,
        &snap(Some(pos)),
        &Decision::ExitPosition {
            reason: "tp".into(),
            symbol: "BTCUSDT".into(),
        },
    );
    assert_eq!(*heard.lock().unwrap(), vec![TradeSignal::Sell]);
    reset();
}

#[test]
fn emit_helpers_respect_kind() {
    let _g = guard();
    reset();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    set_sink(Some(Arc::new(move |k| h.lock().unwrap().push(k))));
    set_enabled(true);
    emit_decision(
        &Decision::hold("x"),
        &LiveApplyResult::default(),
        false,
        false,
    );
    assert!(heard.lock().unwrap().is_empty());
    emit_flatten(&FlattenResult {
        closed: vec!["SHORT ETHUSDT".into()],
        errors: vec![],
    });
    assert_eq!(*heard.lock().unwrap(), vec![TradeSignal::Sell]);
    reset();
}

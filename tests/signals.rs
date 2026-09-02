//! Buy, win-close, and loss-close chimes are distinct; play is silent unless enabled.

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
    set_sink, signals_enabled, write_chime, TradeSignal, BUY_HZ, SELL_LOSS_HZ, SELL_WIN_HZ,
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
        kind_for_decision(
            &enter,
            &LiveApplyResult {
                filled: true,
                ..Default::default()
            },
            true,
            false,
            None
        ),
        Some(TradeSignal::Buy)
    );
    assert_eq!(
        kind_for_decision(&enter, &LiveApplyResult::default(), true, false, None),
        None
    );
    assert_eq!(
        kind_for_decision(&enter, &LiveApplyResult::default(), false, false, None),
        Some(TradeSignal::Buy)
    );
}

#[test]
fn exit_take_profit_is_sell_win() {
    let _g = guard();
    reset();
    let exit_d = Decision::ExitPosition {
        reason: "momentum take profit".into(),
        symbol: "BTCUSDT".into(),
    };
    assert_eq!(
        kind_for_decision(&exit_d, &LiveApplyResult::default(), true, true, Some(true)),
        Some(TradeSignal::SellWin)
    );
    assert_eq!(
        kind_for_decision(&exit_d, &LiveApplyResult::default(), true, true, None),
        Some(TradeSignal::SellWin)
    );
    let be = Decision::ExitPosition {
        reason: "безубыток".into(),
        symbol: "BTCUSDT".into(),
    };
    assert_eq!(
        kind_for_decision(&be, &LiveApplyResult::default(), false, true, None),
        Some(TradeSignal::SellWin)
    );
}

#[test]
fn exit_stop_is_sell_loss() {
    let _g = guard();
    reset();
    let exit_d = Decision::ExitPosition {
        reason: "momentum stop loss".into(),
        symbol: "BTCUSDT".into(),
    };
    assert_eq!(
        kind_for_decision(&exit_d, &LiveApplyResult::default(), true, true, Some(false)),
        Some(TradeSignal::SellLoss)
    );
    assert_eq!(
        kind_for_decision(&exit_d, &LiveApplyResult::default(), true, true, None),
        Some(TradeSignal::SellLoss)
    );
    assert_eq!(
        kind_for_decision(&exit_d, &LiveApplyResult::default(), true, false, Some(false)),
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
            true,
            Some(false)
        ),
        None
    );
    assert_eq!(
        kind_for_decision(&Decision::hold("wait"), &LiveApplyResult::default(), false, false, None),
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
            true,
            None
        ),
        None
    );
}

#[test]
fn flatten_unknown_is_sell_loss() {
    let _g = guard();
    reset();
    let closed = FlattenResult {
        closed: vec!["LONG BTCUSDT".into()],
        errors: vec![],
    };
    assert_eq!(kind_for_flatten(&closed, None), Some(TradeSignal::SellLoss));
    assert_eq!(kind_for_flatten(&closed, Some(false)), Some(TradeSignal::SellLoss));
    assert_eq!(kind_for_flatten(&closed, Some(true)), Some(TradeSignal::SellWin));
    assert_eq!(kind_for_flatten(&FlattenResult::default(), None), None);
}

#[test]
fn chimes_are_different_wavs() {
    let _g = guard();
    reset();
    assert!(BUY_HZ.1 > BUY_HZ.0);
    assert!(SELL_WIN_HZ.0 < SELL_WIN_HZ.1 && SELL_WIN_HZ.1 < SELL_WIN_HZ.2);
    assert!(SELL_LOSS_HZ.0 > SELL_LOSS_HZ.1 && SELL_LOSS_HZ.1 > SELL_LOSS_HZ.2);
    assert!(SELL_WIN_HZ.0 > SELL_LOSS_HZ.0);
    assert_ne!(TradeSignal::Buy, TradeSignal::SellWin);
    assert_ne!(TradeSignal::SellWin, TradeSignal::SellLoss);
    let root = tempfile::tempdir().unwrap();
    let buy = write_chime(&root.path().join("buy.wav"), &[BUY_HZ.0, BUY_HZ.1], 22_050).unwrap();
    let win = write_chime(
        &root.path().join("sell_win.wav"),
        &[SELL_WIN_HZ.0, SELL_WIN_HZ.1, SELL_WIN_HZ.2],
        22_050,
    )
    .unwrap();
    let loss = write_chime(
        &root.path().join("sell_loss.wav"),
        &[SELL_LOSS_HZ.0, SELL_LOSS_HZ.1, SELL_LOSS_HZ.2],
        22_050,
    )
    .unwrap();
    let buy_bytes = std::fs::read(&buy).unwrap();
    let win_bytes = std::fs::read(&win).unwrap();
    let loss_bytes = std::fs::read(&loss).unwrap();
    assert_ne!(buy_bytes, win_bytes);
    assert_ne!(win_bytes, loss_bytes);
    assert_ne!(buy_bytes, loss_bytes);
    assert!(buy_bytes.starts_with(b"RIFF"));
    assert!(win_bytes.starts_with(b"RIFF"));
    assert!(loss_bytes.starts_with(b"RIFF"));
    assert!(buy_bytes.len() > 200);
    let (p_buy, p_win, p_loss) = chime_paths().unwrap();
    let cached_buy = std::fs::read(p_buy).unwrap();
    let cached_win = std::fs::read(p_win).unwrap();
    let cached_loss = std::fs::read(p_loss).unwrap();
    assert_ne!(cached_buy, cached_win);
    assert_ne!(cached_win, cached_loss);
    assert_ne!(cached_buy, cached_loss);
}

#[test]
fn chime_paths_is_safe_from_two_threads() {
    let _g = guard();
    reset();
    let a = std::thread::spawn(|| chime_paths().unwrap());
    let b = std::thread::spawn(|| chime_paths().unwrap());
    let (a_buy, a_win, a_loss) = a.join().unwrap();
    let (b_buy, b_win, b_loss) = b.join().unwrap();
    assert_eq!(a_buy, b_buy);
    assert_eq!(a_win, b_win);
    assert_eq!(a_loss, b_loss);
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
    assert!(play(TradeSignal::SellWin, None));
    assert!(play(TradeSignal::SellLoss, None));
    assert_eq!(
        *heard.lock().unwrap(),
        vec![TradeSignal::Buy, TradeSignal::SellWin, TradeSignal::SellLoss]
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
    let mut green = Position::long("BTCUSDT", d("0.01"), d("1000"), Some(d("990")), Some(d("1020")));
    green.unrealized_pnl = d("5");
    apply_decision(
        &cfg,
        &mut FakeClient,
        &mut state,
        &snap(Some(green)),
        &Decision::ExitPosition {
            reason: "momentum take profit".into(),
            symbol: "BTCUSDT".into(),
        },
    );
    assert_eq!(*heard.lock().unwrap(), vec![TradeSignal::SellWin]);
    heard.lock().unwrap().clear();
    let mut red = Position::long("ETHUSDT", d("0.01"), d("1000"), Some(d("990")), Some(d("1020")));
    red.unrealized_pnl = d("-4");
    let mut red_snap = snap(Some(red));
    red_snap.tickers = vec![Ticker::new("ETHUSDT", d("990"), d("-1"), d("10"))];
    apply_decision(
        &cfg,
        &mut FakeClient,
        &mut state,
        &red_snap,
        &Decision::ExitPosition {
            reason: "momentum stop loss".into(),
            symbol: "ETHUSDT".into(),
        },
    );
    assert_eq!(*heard.lock().unwrap(), vec![TradeSignal::SellLoss]);
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
        None,
    );
    assert!(heard.lock().unwrap().is_empty());
    emit_flatten(
        &FlattenResult {
            closed: vec!["SHORT ETHUSDT".into()],
            errors: vec![],
        },
        None,
    );
    assert_eq!(*heard.lock().unwrap(), vec![TradeSignal::SellLoss]);
    reset();
}

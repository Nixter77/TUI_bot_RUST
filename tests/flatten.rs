//! Drive flatten_targets / close_all_positions. No live HTTP.

mod common;
use common::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use tui_bot::engine::{tick, MomentumParams};
use tui_bot::exchange::{ExchangeError, FlattenClient};
use tui_bot::flatten::{close_all_positions, flatten_open_book, flatten_targets};
use tui_bot::models::{Decision, EngineState, MarketSnapshot, Position, Side, Ticker};

struct FakeFlat {
    fail: std::collections::HashSet<String>,
    protect_cancels: Vec<String>,
    closes: Vec<(String, String, Decimal)>,
    position_raw: Value,
    risk_reads: u32,
    fail_on_read: Option<u32>,
}

impl FakeFlat {
    fn new(fail: &[&str], raw: Value) -> Self {
        Self {
            fail: fail.iter().map(|s| s.to_string()).collect(),
            protect_cancels: Vec::new(),
            closes: Vec::new(),
            position_raw: raw,
            risk_reads: 0,
            fail_on_read: None,
        }
    }
}

impl FlattenClient for FakeFlat {
    fn cancel_protectives(&mut self, symbol: &str) -> Result<(), ExchangeError> {
        self.protect_cancels.push(symbol.into());
        Ok(())
    }
    fn market_close(&mut self, symbol: &str, side: &str, qty: Decimal) -> Result<(), ExchangeError> {
        if self.fail.contains(symbol) {
            return Err(ExchangeError("reject close".into()));
        }
        self.closes.push((symbol.into(), side.into(), qty));
        if let Some(arr) = self.position_raw.as_array_mut() {
            arr.retain(|row| row.get("symbol").and_then(|v| v.as_str()) != Some(symbol));
        }
        Ok(())
    }
    fn position_risk(&mut self) -> Result<Value, ExchangeError> {
        self.risk_reads += 1;
        if self.fail_on_read == Some(self.risk_reads) {
            return Err(ExchangeError("HTTP 502 /fapi/v2/positionRisk: gateway".into()));
        }
        Ok(self.position_raw.clone())
    }
}

fn pos(symbol: &str, side: Side, qty: &str) -> Position {
    Position {
        symbol: symbol.into(),
        side,
        qty: d(qty),
        entry_price: Decimal::ONE,
        stop_loss: None,
        take_profit: None,
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: None,
        leverage: 0,
    }
}

#[test]
fn skips_zero_qty_and_keeps_shorts() {
    let targets = flatten_targets(&[
        pos("ETHUSDT", Side::Short, "0.071"),
        pos("BTCUSDT", Side::Short, "0.004"),
        pos("PORTALUSDT", Side::Long, "100"),
        pos("FLATUSDT", Side::Long, "0"),
    ]);
    let pairs: std::collections::HashSet<_> = targets
        .iter()
        .map(|t| (t.symbol.clone(), t.side, t.qty))
        .collect();
    assert_eq!(
        pairs,
        [
            ("ETHUSDT".into(), Side::Short, d("0.071")),
            ("BTCUSDT".into(), Side::Short, d("0.004")),
            ("PORTALUSDT".into(), Side::Long, d("100")),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn watch_mode_sends_nothing() {
    let mut client = FakeFlat::new(&[], json!([]));
    let result = close_all_positions(false, true, &mut client, &[pos("ETHUSDT", Side::Short, "1")]);
    assert_eq!(result.errors, vec!["flatten refused: not live"]);
    assert!(client.closes.is_empty());
}

#[test]
fn no_credentials_refuses() {
    let mut client = FakeFlat::new(&[], json!([]));
    let result = close_all_positions(true, false, &mut client, &[pos("ETHUSDT", Side::Short, "1")]);
    assert_eq!(result.errors, vec!["flatten refused: no credentials"]);
    assert!(client.closes.is_empty());
}

#[test]
fn long_sells_short_buys_and_cancels() {
    let mut client = FakeFlat::new(&[], json!([]));
    let result = close_all_positions(
        true,
        true,
        &mut client,
        &[
            pos("PORTALUSDT", Side::Long, "1112.3"),
            pos("ETHUSDT", Side::Short, "0.071"),
        ],
    );
    assert!(result.errors.is_empty());
    let closed: std::collections::HashSet<_> = result.closed.into_iter().collect();
    assert_eq!(
        closed,
        ["LONG PORTALUSDT", "SHORT ETHUSDT"]
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    );
    let closes: std::collections::HashSet<_> = client.closes.into_iter().collect();
    assert_eq!(
        closes,
        [
            ("PORTALUSDT".into(), "LONG".into(), d("1112.3")),
            ("ETHUSDT".into(), "SHORT".into(), d("0.071")),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn continues_after_one_symbol_fails() {
    let mut client = FakeFlat::new(&["BTCUSDT"], json!([]));
    let result = close_all_positions(
        true,
        true,
        &mut client,
        &[
            pos("BTCUSDT", Side::Short, "0.004"),
            pos("ETHUSDT", Side::Short, "0.071"),
        ],
    );
    assert_eq!(result.closed, vec!["SHORT ETHUSDT"]);
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains("BTCUSDT"));
    assert_eq!(client.closes, vec![("ETHUSDT".into(), "SHORT".into(), d("0.071"))]);
}

#[test]
fn flatten_open_book_reports_leftover() {
    let raw = json!([
        {"symbol": "BTCUSDT", "positionAmt": "-0.004", "entryPrice": "1", "unRealizedProfit": "0"},
        {"symbol": "ETHUSDT", "positionAmt": "-0.071", "entryPrice": "1", "unRealizedProfit": "0"},
    ]);
    let mut client = FakeFlat::new(&["BTCUSDT"], raw);
    let result = flatten_open_book(&mut client);
    assert_eq!(result.closed, vec!["SHORT ETHUSDT"]);
    assert!(result.errors.iter().any(|e| e.contains("BTCUSDT")));
    assert!(result.errors.iter().any(|e| e.contains("ещё открыты")));
}

#[test]
fn flatten_open_book_confirm_read_failure_is_error() {
    let raw = json!([
        {"symbol": "ETHUSDT", "positionAmt": "-0.071", "entryPrice": "1", "unRealizedProfit": "0"},
    ]);
    let mut client = FakeFlat::new(&[], raw);
    client.fail_on_read = Some(2);
    let result = flatten_open_book(&mut client);
    assert_eq!(result.closed, vec!["SHORT ETHUSDT"]);
    assert!(
        result.errors.iter().any(|e| e.contains("подтвердить flatten")),
        "{:?}",
        result.errors
    );
}

#[test]
fn close_cancels_protectives_again_after_fill() {
    let mut client = FakeFlat::new(&[], json!([]));
    let result = close_all_positions(true, true, &mut client, &[pos("BTCUSDT", Side::Short, "0.004")]);
    assert!(result.errors.is_empty());
    assert_eq!(result.closed, vec!["SHORT BTCUSDT"]);
    assert_eq!(client.protect_cancels, vec!["BTCUSDT", "BTCUSDT"]);
    assert_eq!(client.closes, vec![("BTCUSDT".into(), "SHORT".into(), d("0.004"))]);
}

#[test]
fn paused_tick_does_not_enter() {
    let mut snap = MarketSnapshot::empty(Decimal::ONE);
    snap.tickers = vec![Ticker::new("BTCUSDT", d("50000"), d("9.5"), d("8000"))];
    snap.chart_symbol = "BTCUSDT".into();
    let mut state = EngineState::new(1);
    state.entries_paused = true;
    let (new_state, decision) = tick(&state, &snap, 100.0, None, None, None);
    assert!(matches!(decision, Decision::Hold { .. }));
    assert!(!matches!(decision, Decision::EnterLong { .. }));
    assert!(new_state.entries_paused);
    assert!(decision.reason().contains("паузе"));
    let _ = MomentumParams::default();
}

#[test]
fn paused_tick_resumes_after_cooldown() {
    let mut snap = MarketSnapshot::empty(Decimal::ONE);
    snap.tickers = vec![Ticker::new("BTCUSDT", d("50000"), d("9.5"), d("8000"))];
    snap.chart_symbol = "BTCUSDT".into();
    let mut state = EngineState::new(1);
    state.entries_paused = true;
    state.cooldown_until = 50.0;
    let (new_state, _) = tick(&state, &snap, 100.0, None, None, None);
    assert!(!new_state.entries_paused);
}

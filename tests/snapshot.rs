//! Drive shipped pull_snapshot from a fake exchange client. No HTTP.

use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;
use tui_bot::config::load_config;
use tui_bot::exchange::{ExchangeError, SnapshotClient};
use tui_bot::models::{Bar, EngineState, Position, Side, Ticker};
use tui_bot::profit::{account_profit, current_equity, EquityPin};
use tui_bot::snapshot::{fetch_snapshot, pull_snapshot};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn cfg_with_keys() -> tui_bot::config::Config {
    let mut env = HashMap::new();
    env.insert("BINANCE_API_KEY".into(), "A".repeat(32));
    env.insert("BINANCE_API_SECRET".into(), "B".repeat(32));
    load_config(false, None, Some(&env)).unwrap()
}

fn bars(n: usize, start: f64) -> Vec<Bar> {
    let mut out = Vec::new();
    let mut px = start;
    for i in 0..n {
        let nxt = px + 0.1;
        let o = Decimal::from_str_exact(&format!("{px:.4}")).unwrap();
        let c = Decimal::from_str_exact(&format!("{nxt:.4}")).unwrap();
        out.push(Bar {
            open_time: 1_700_000_000_000 + i as i64 * 300_000,
            open: o,
            high: o.max(c) + d("0.05"),
            low: o.min(c) - d("0.05"),
            close: c,
            volume: d("20"),
        });
        px = nxt;
    }
    out
}

struct FakeSnap {
    wallet: String,
    upnl: String,
    available: String,
    positions: Value,
    tickers: Vec<Ticker>,
    klines: HashMap<String, Vec<Bar>>,
}

impl FakeSnap {
    fn book(wallet: &str, upnl: &str, positions: Value) -> Self {
        Self {
            wallet: wallet.into(),
            upnl: upnl.into(),
            available: "3000".into(),
            positions,
            tickers: vec![
                Ticker::new("BTCUSDT", d("50000"), d("2.0"), d("8000")),
                Ticker::new("ETHUSDT", d("3000"), d("1.0"), d("4000")),
                Ticker::new("SOLUSDT", d("140"), d("0.5"), d("2000")),
            ],
            klines: [
                ("BTCUSDT".into(), bars(5, 50000.0)),
                ("ETHUSDT".into(), bars(5, 3000.0)),
                ("SOLUSDT".into(), bars(5, 140.0)),
            ]
            .into_iter()
            .collect(),
        }
    }
}

impl SnapshotClient for FakeSnap {
    fn ticker_24h(&mut self) -> Result<Vec<Ticker>, ExchangeError> {
        Ok(self.tickers.clone())
    }
    fn klines(&mut self, symbol: &str, _interval: &str, _limit: usize) -> Result<Vec<Bar>, ExchangeError> {
        Ok(self
            .klines
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| bars(3, 100.0)))
    }
    fn account(&mut self) -> Result<Value, ExchangeError> {
        Ok(json!({
            "totalWalletBalance": self.wallet,
            "totalUnrealizedProfit": self.upnl,
            "availableBalance": self.available,
        }))
    }
    fn position_risk(&mut self) -> Result<Value, ExchangeError> {
        Ok(self.positions.clone())
    }
}

fn long_row(symbol: &str, qty: &str, entry: &str, pnl: &str) -> Value {
    json!({
        "symbol": symbol,
        "positionAmt": qty,
        "entryPrice": entry,
        "unRealizedProfit": pnl,
    })
}

fn short_row(symbol: &str, qty: &str, entry: &str, pnl: &str) -> Value {
    json!({
        "symbol": symbol,
        "positionAmt": format!("-{qty}"),
        "entryPrice": entry,
        "unRealizedProfit": pnl,
    })
}

#[test]
fn pull_snapshot_loads_account_pins_once_and_fills_universe() {
    let cfg = cfg_with_keys();
    let wallet = d("3039.6780");
    let upnl = d("93.0573");
    let mut client = FakeSnap::book(
        "3039.6780",
        "93.0573",
        json!([
            short_row("ETHUSDT", "0.1", "3000", "40"),
            long_row("BTCUSDT", "0.01", "50000", "3.0573"),
        ]),
    );
    let mut state = EngineState::new(2);
    let mut pin = EquityPin {
        value: None,
        persist: false,
    };
    let snap = pull_snapshot(&cfg, Some(&mut client), &mut state, &mut pin, false, None);

    assert!(snap.live_book);
    assert!(snap.account_ok);
    assert!(snap.account_fresh);
    assert_eq!(snap.account.wallet_balance, wallet);
    assert_eq!(snap.account.unrealized_pnl, upnl);
    let equity = current_equity(wallet, upnl);
    assert_eq!(snap.account.starting_equity, equity);
    assert_eq!(pin.value, Some(equity));
    assert_eq!(account_profit(wallet, upnl, pin.value.unwrap()), Decimal::ZERO);
    assert!(snap.open_positions.iter().any(|p| p.side == Side::Short && p.symbol == "ETHUSDT"));
    for major in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        assert!(
            snap.universe_bars.contains_key(major),
            "universe missing {major}: {:?}",
            snap.universe_bars.keys().collect::<Vec<_>>()
        );
        assert!(!snap.universe_bars[major].is_empty());
    }

    client.wallet = "3039.8808".into();
    client.upnl = "93.9810".into();
    let later_wallet = d("3039.8808");
    let later_upnl = d("93.9810");
    let snap2 = pull_snapshot(&cfg, Some(&mut client), &mut state, &mut pin, false, Some(&snap));
    assert_eq!(pin.value, Some(equity), "pin must not rebase on later polls");
    assert_eq!(snap2.account.starting_equity, equity);
    assert!(account_profit(later_wallet, later_upnl, pin.value.unwrap()) > Decimal::ONE);
}

#[test]
fn fetch_snapshot_overlays_remembered_tp_sl() {
    let cfg = cfg_with_keys();
    let mut client = FakeSnap::book(
        "3105.72",
        "-0.37",
        json!([long_row("ETHUSDT", "0.015", "2552.32", "-0.37")]),
    );
    let saved = Position::long(
        "ETHUSDT",
        d("0.015"),
        d("2552.32"),
        Some(d("2477.05780")),
        Some(d("2616.12800")),
    );
    let snap = fetch_snapshot(
        &cfg,
        Some(&mut client),
        &EngineState::new(1),
        false,
        None,
        None,
        None,
        &[],
        &[saved.clone()],
    );
    assert!(snap.live_book);
    let pos = snap.position.expect("managed long");
    assert_eq!(pos.symbol, "ETHUSDT");
    assert_eq!(pos.stop_loss, saved.stop_loss);
    assert_eq!(pos.take_profit, saved.take_profit);
    assert_eq!(snap.open_positions[0].stop_loss, saved.stop_loss);
}

#[test]
fn pull_snapshot_offline_skips_client() {
    let cfg = load_config(false, None, Some(&HashMap::new())).unwrap();
    let mut state = EngineState::new(1);
    let mut pin = EquityPin {
        value: None,
        persist: false,
    };
    let snap = pull_snapshot(&cfg, None, &mut state, &mut pin, true, None);
    assert!(!snap.live_book);
    assert!(!snap.account_ok);
    assert_eq!(snap.account.wallet_balance, Decimal::ZERO);
    assert!(pin.value.is_none());
}

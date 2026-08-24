//! Drive shipped engine.tick / momentum_decision / decide.

mod common;
use common::*;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tui_bot::config::STRATEGY1_POLL_SECONDS;
use tui_bot::engine::{
    decide, momentum_decision, select_strategy_str, tick, tick_decisions, MomentumParams, STRATEGY_NAMES,
};
use tui_bot::models::{coalesce_position, Bar, Decision, EngineState, MarketSnapshot, Position, Side, Ticker};
use tui_bot::ranking::rank_most_rising;
use tui_bot::trail::{candidate_stop, take_profit_price_net, trail_stop_upward};

fn is_enter(d: &Decision) -> bool {
    matches!(d, Decision::EnterLong { .. })
}
fn is_hold(d: &Decision) -> bool {
    matches!(d, Decision::Hold { .. })
}
fn is_amend(d: &Decision) -> bool {
    matches!(d, Decision::AmendStop { .. })
}

#[test]
fn poll_is_one_or_two_minutes() {
    assert!(STRATEGY1_POLL_SECONDS == 60 || STRATEGY1_POLL_SECONDS == 120);
    let p = MomentumParams::default();
    assert!(p.poll_seconds == 60 || p.poll_seconds == 120);
}

#[test]
fn scan_buys_most_rising_with_tp_and_sl() {
    let tickers = tickers();
    let winner = rank_most_rising(&tickers, &[]).unwrap();
    let params = MomentumParams {
        poll_seconds: 120,
        tp_pct: d("0.012"),
        trail_pct: d("0.006"),
        ..MomentumParams::default()
    };
    let (decision, scan_ts) = momentum_decision(&tickers, None, 1000.0, 0.0, Some(&params));
    assert_eq!(scan_ts, 1000.0);
    match decision {
        Decision::EnterLong {
            symbol,
            take_profit,
            stop_loss,
            ..
        } => {
            assert_eq!(symbol, winner.symbol);
            assert_eq!(
                take_profit,
                take_profit_price_net(winner.last_price, "LONG", params.tp_pct).unwrap()
            );
            assert_eq!(
                stop_loss,
                candidate_stop(winner.last_price, "LONG", params.trail_pct).unwrap()
            );
        }
        other => panic!("expected EnterLong, got {other:?}"),
    }
}

#[test]
fn does_not_rescan_before_poll() {
    let params = MomentumParams {
        poll_seconds: 120,
        ..MomentumParams::default()
    };
    let (decision, scan_ts) = momentum_decision(&tickers(), None, 1119.0, 1000.0, Some(&params));
    assert!(is_hold(&decision));
    assert_eq!(scan_ts, 1000.0);
    let (due, due_ts) = momentum_decision(&tickers(), None, 1120.0, 1000.0, Some(&params));
    assert!(is_enter(&due));
    assert_eq!(due_ts, 1120.0);
}

#[test]
fn trails_stop_up_and_never_down() {
    let params = MomentumParams {
        poll_seconds: 120,
        trail_pct: d("0.006"),
        ..MomentumParams::default()
    };
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: d("0.01"),
        entry_price: d("50000"),
        stop_loss: Some(d("100")),
        take_profit: Some(d("90000")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: None,
        leverage: 0,
    };
    let up = vec![Ticker::new("BTCUSDT", d("110"), d("1"), d("1"))];
    let (decision, _) = momentum_decision(&up, Some(&pos), 50.0, 1.0, Some(&params));
    match decision {
        Decision::AmendStop { stop_loss, .. } => {
            let expected = trail_stop_upward(
                Some(d("100")),
                candidate_stop(d("110"), "LONG", params.trail_pct).unwrap(),
                "LONG",
            )
            .unwrap();
            assert_eq!(stop_loss, expected);
            assert!(stop_loss > d("100"));
        }
        other => panic!("{other:?}"),
    }
    let high_sl = Position {
        stop_loss: Some(d("100")),
        take_profit: Some(d("90000")),
        entry_price: d("100"),
        ..pos
    };
    let down = vec![Ticker::new("BTCUSDT", d("100.5"), d("1"), d("1"))];
    let (hold, _) = momentum_decision(&down, Some(&high_sl), 50.0, 1.0, Some(&params));
    assert!(is_hold(&hold));
    let cand = candidate_stop(d("100.5"), "LONG", params.trail_pct).unwrap();
    assert!(cand <= d("100"));
}

#[test]
fn tick_and_three_named_strategies() {
    assert_eq!(STRATEGY_NAMES.len(), 4);
    assert_eq!(STRATEGY_NAMES[0].1, "Momentum rider (растущий + TP + SL вверх)");
    assert_eq!(STRATEGY_NAMES[1].1, "Скальп: откат к VWAP/EMA9");
    assert_eq!(STRATEGY_NAMES[2].1, "Тренд: пробой Donchian 20/10 (день)");
    assert!(STRATEGY_NAMES[3].1.contains("Continuation"));
    assert_eq!(select_strategy_str("2").unwrap(), 2);
    assert_eq!(select_strategy_str("4").unwrap(), 4);
    assert!(select_strategy_str("9").is_err());

    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    let state = EngineState::new(1);
    let (new_state, decision) = tick(&state, &snap, 10.0, None, None, None);
    assert!(is_enter(&decision));
    assert!(new_state.last_scan_ts > 0.0);
    let empty = HashMap::new();
    let (scalp_dec, _) = decide(2, &snap, 10.0, 0.0, None, None, None, None, &[], &empty).unwrap();
    assert!(is_hold(&scalp_dec));
    let (trend_dec, _) = decide(3, &snap, 10.0, 0.0, None, None, None, None, &[], &empty).unwrap();
    assert!(is_hold(&trend_dec));
}

#[test]
fn short_position_is_not_managed_as_long() {
    let short = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Short,
        qty: Decimal::ONE,
        entry_price: d("50000"),
        stop_loss: Some(d("51000")),
        take_profit: Some(d("48000")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: None,
        leverage: 0,
    };
    let (decision, _) = momentum_decision(&tickers(), Some(&short), 10.0, 0.0, None);
    assert!(is_hold(&decision));
    assert!(decision.reason().contains("buy-only"));
}

#[test]
fn missing_stop_attaches_below_mark_and_does_not_exit() {
    let params = MomentumParams {
        poll_seconds: 120,
        trail_pct: d("0.006"),
        ..MomentumParams::default()
    };
    let pos = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Long,
        qty: d("0.01"),
        entry_price: d("100"),
        stop_loss: None,
        take_profit: Some(d("200")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: None,
        leverage: 0,
    };
    let dropped = vec![Ticker::new("BTCUSDT", d("99.5"), d("1"), d("1"))];
    let (decision, _) = momentum_decision(&dropped, Some(&pos), 50.0, 1.0, Some(&params));
    match decision {
        Decision::AmendStop { stop_loss, .. } => {
            let cand = candidate_stop(d("99.5"), "LONG", params.trail_pct).unwrap();
            assert_eq!(stop_loss, cand);
            assert!(stop_loss < d("99.5"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn coalesce_does_not_resurrect_when_live_is_none() {
    let remembered = Position::long("BTCUSDT", Decimal::ONE, d("100"), Some(d("99")), Some(d("102")));
    assert!(coalesce_position(None, Some(&remembered)).is_none());
}

#[test]
fn entry_inflight_blocks_second_buy() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    let mut state = EngineState::new(1);
    state.entry_inflight = true;
    state.last_scan_ts = 10.0;
    let (_, decision) = tick(&state, &snap, 10_000.0, None, None, None);
    assert!(is_hold(&decision));
    assert_eq!(decision.reason(), "entry in flight");
}

#[test]
fn live_book_keeps_inflight_and_does_not_repeat_enter() {
    let mut snap = strategy4_ready_snap();
    snap.live_book = true;
    snap.account_ok = true;
    let mom = MomentumParams {
        max_positions: 1,
        ..MomentumParams::default()
    };
    let (filled, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), Some(&mom), None, None);
    assert!(decisions.iter().any(is_enter), "{decisions:?}");
    assert!(
        filled
            .inflight_symbols
            .iter()
            .any(|s| s.eq_ignore_ascii_case("BTCUSDT")),
        "{:?}",
        filled.inflight_symbols
    );
    let (_, again) = tick_decisions(&filled, &snap, london_ts() + 5.0, Some(&mom), None, None);
    assert!(
        !again.iter().any(is_enter),
        "repeat enter on live snapshot lag: {again:?}"
    );
}

#[test]
fn skips_new_entries_outside_start_hours_but_trails_open_long() {
    let params = MomentumParams {
        poll_seconds: 60,
        ..MomentumParams::default()
    };
    let (closed, scan_ts) = momentum_decision(&tickers(), None, dead_ts(), 0.0, Some(&params));
    assert!(is_hold(&closed));
    assert!(closed.reason().contains("вне часов старта"));
    assert_eq!(scan_ts, 0.0);
    let (opened, _) = momentum_decision(&tickers(), None, london_ts(), 0.0, Some(&params));
    assert!(is_enter(&opened));
    let pos = Position::long("BTCUSDT", d("0.01"), d("100"), Some(d("90")), Some(d("200")));
    let trail_p = MomentumParams {
        poll_seconds: 60,
        trail_pct: d("0.006"),
        ..MomentumParams::default()
    };
    let (trail, _) = momentum_decision(
        &[Ticker::new("BTCUSDT", d("110"), d("1"), d("1"))],
        Some(&pos),
        dead_ts(),
        1.0,
        Some(&trail_p),
    );
    assert!(is_amend(&trail));
    let forced_p = MomentumParams {
        poll_seconds: 60,
        always_enter: true,
        ..MomentumParams::default()
    };
    let (forced, _) = momentum_decision(&tickers(), None, dead_ts(), 0.0, Some(&forced_p));
    assert!(is_enter(&forced));
}

#[test]
fn scan_can_enter_three_rising_names() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    let state = EngineState::new(1);
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (new_state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    let enters: Vec<_> = decisions.iter().filter(|d| is_enter(d)).collect();
    assert_eq!(enters.len(), 1, "{decisions:?}");
    assert!(
        ["BTCUSDT", "ETHUSDT", "SOLUSDT"].contains(&enters[0].symbol()),
        "{decisions:?}"
    );
    let mut state = new_state;
    let mut got: std::collections::HashSet<String> = std::collections::HashSet::new();
    got.insert(enters[0].symbol().to_string());
    for step in 1..3 {
        let (ns, decs) = tick_decisions(&state, &snap, london_ts() + (step as f64) * 60.0, Some(&mom), None, None);
        state = ns;
        for d in decs.iter().filter(|d| is_enter(d)) {
            got.insert(d.symbol().to_string());
        }
    }
    assert_eq!(
        got,
        ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    );
}

#[test]
fn skips_tradfi_names_and_keeps_skip_list() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        Ticker::new("XAUUSDT", d("2400"), d("9.0"), d("900000")),
        Ticker::new("TSLAUSDT", d("200"), d("8.0"), d("800000")),
        Ticker::new("BTCUSDT", d("50000"), d("6.0"), d("700000")),
        Ticker::new("ETHUSDT", d("3000"), d("4.0"), d("100000")),
    ];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    let mut state = EngineState::new(1);
    state.skip_symbols = vec!["XAUUSDT".into(), "TSLAUSDT".into()];
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (new_state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    let enters: Vec<_> = decisions.iter().filter(|d| is_enter(d)).collect();
    assert_eq!(enters.len(), 1, "{decisions:?}");
    assert!(["BTCUSDT", "ETHUSDT"].contains(&enters[0].symbol()), "{decisions:?}");
    assert_eq!(new_state.skip_symbols, vec!["XAUUSDT", "TSLAUSDT"]);
}

#[test]
fn scan_buys_fastest_24h_leader_not_only_majors() {
    let tickers = vec![
        Ticker::new("MORPHOUSDT", d("2.87"), d("9.8"), d("300000")),
        Ticker::new("SPKUSDT", d("0.0225"), d("8.4"), d("200000")),
        Ticker::new("GRASSUSDT", d("0.364"), d("7.1"), d("150000")),
        Ticker::new("BTCUSDT", d("50000"), d("0.8"), d("800000")),
        Ticker::new("ETHUSDT", d("3000"), d("1.6"), d("700000")),
        Ticker::new("SOLUSDT", d("95"), d("2.0"), d("200000")),
    ];
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers;
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(1), &snap, london_ts(), Some(&mom), None, None);
    let enters: Vec<_> = decisions.iter().filter(|d| is_enter(d)).collect();
    assert_eq!(enters.len(), 1, "{decisions:?}");
    assert_eq!(enters[0].symbol(), "MORPHOUSDT");
}

#[test]
fn cooldown_after_position_vanishes() {
    let pos = Position::long("BTCUSDT", d("0.01"), d("50000"), Some(d("49000")), Some(d("52000")));
    let mut state = EngineState::new(1);
    state.position = Some(pos.clone());
    state.last_scan_ts = 0.0;
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("BTCUSDT", d("50000"), d("9.5"), d("800000"))];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.live_book = true;
    let london = london_ts();
    let (new_state, decision) = tick(&state, &snap, london, None, None, None);
    assert!(is_hold(&decision));
    let reason = decision.reason();
    assert!(reason.contains("пауза") || reason.contains("cooling"), "{reason}");
    assert!(new_state.cooldowns.get("BTCUSDT").copied().unwrap_or(0.0) > london);
    let (_, decision2) = tick(&new_state, &snap, london + 60.0, None, None, None);
    assert!(is_hold(&decision2));
    let r2 = decision2.reason();
    assert!(r2.contains("пауза") || r2.contains("cooling"), "{r2}");
}

#[test]
fn poll_timeout_is_not_sticky() {
    let raw = r#"HTTP 408 /fapi/v2/account: {"code":-1007,"msg":"Timeout"}"#;
    let now = london_ts();
    let mut snap_timeout = MarketSnapshot::empty(d("10000"));
    snap_timeout.account = account();
    snap_timeout.chart_symbol = "BTCUSDT".into();
    snap_timeout.last_error = Some(raw.into());
    snap_timeout.account_ok = true;
    let mom = MomentumParams {
        always_enter: true,
        ..MomentumParams::default()
    };
    let (stuck, _) = tick_decisions(&EngineState::new(1), &snap_timeout, now, Some(&mom), None, None);
    assert!(stuck.last_error.is_none());
    let mut leftover = EngineState::new(1);
    leftover.last_error = Some(raw.into());
    let mut clean = MarketSnapshot::empty(d("10000"));
    clean.account = account();
    clean.chart_symbol = "BTCUSDT".into();
    clean.account_ok = true;
    let (cleared, _) = tick_decisions(&leftover, &clean, now + 120.0, Some(&mom), None, None);
    assert!(cleared.last_error.is_none());
    let mut live = EngineState::new(1);
    live.last_error = Some(r#"HTTP 400 /fapi/v1/order: {"code":-2027,"msg":"cap"}"#.into());
    let (kept, _) = tick_decisions(&live, &clean, now + 240.0, Some(&mom), None, None);
    assert!(kept.last_error.as_deref().unwrap_or("").contains("-2027"));
}

#[test]
fn default_is_one_slot() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    let mom = MomentumParams {
        always_enter: true,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(1), &snap, london_ts(), Some(&mom), None, None);
    let enters = decisions.iter().filter(|d| is_enter(d)).count();
    assert_eq!(enters, 1);
}

#[test]
fn red_5m_skips_enter() {
    use tui_bot::models::Bar;
    let red = Bar {
        open_time: 0,
        open: d("51000"),
        high: d("51100"),
        low: d("50000"),
        close: d("50100"),
        volume: Decimal::ONE,
    };
    let prev = Bar {
        open_time: 1,
        open: d("50000"),
        high: d("50200"),
        low: d("49900"),
        close: d("50100"),
        volume: Decimal::ONE,
    };
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.bars = vec![prev, red.clone()];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    snap.last_bars = [
        ("BTCUSDT", red.clone()),
        ("ETHUSDT", red.clone()),
        ("SOLUSDT", red.clone()),
        ("XRPUSDT", red),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(1), &snap, london_ts(), Some(&mom), None, None);
    assert!(!decisions.iter().any(is_enter));
    assert!(decisions[0].reason().contains("5м"));
}

#[test]
fn daily_halt_blocks_enter_keeps_trail() {
    let mut pos = Position::long("BTCUSDT", d("0.01"), d("50000"), Some(d("49000")), Some(d("52000")));
    pos.unrealized_pnl = d("10");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("BTCUSDT", d("50500"), d("1"), d("8000"))];
    snap.account = account();
    snap.position = Some(pos.clone());
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    let mut state = EngineState::new(1);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    state.daily_halt = true;
    state.day_utc = "2026-08-17".into();
    state.day_start_equity = Some(d("10000"));
    let mom = MomentumParams {
        always_enter: true,
        trail_pct: d("0.006"),
        ..MomentumParams::default()
    };
    let (new_state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(new_state.daily_halt);
    assert!(!decisions.iter().any(is_enter));
    assert!(decisions.iter().any(|d| is_amend(d) || is_hold(d)));
}

#[test]
fn two_red_slots_block_third_major() {
    let mut btc = Position::long("BTCUSDT", d("0.01"), d("50000"), Some(d("49000")), Some(d("52000")));
    btc.unrealized_pnl = d("-1");
    let mut eth = Position::long("ETHUSDT", d("0.1"), d("3000"), Some(d("2940")), Some(d("3100")));
    eth.unrealized_pnl = d("-2");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.live_book = true;
    snap.account_ok = true;
    snap.open_positions = vec![btc.clone(), eth.clone()];
    snap.position = Some(btc.clone());
    let mut state = EngineState::new(1);
    state.positions = vec![btc, eth];
    state.position = state.positions.first().cloned();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn red_slot_does_not_open_another() {
    let mut btc = Position::long("BTCUSDT", d("0.01"), d("50000"), Some(d("49000")), Some(d("52000")));
    btc.unrealized_pnl = d("-1");
    let eth = Position::long("ETHUSDT", d("0.1"), d("3000"), Some(d("2940")), Some(d("3100")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.live_book = true;
    snap.account_ok = true;
    snap.open_positions = vec![btc.clone(), eth.clone()];
    snap.position = Some(btc.clone());
    let mut state = EngineState::new(1);
    state.positions = vec![btc, eth];
    state.position = state.positions.first().cloned();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn manages_three_open_longs_independently() {
    let btc = Position::long("BTCUSDT", d("0.01"), d("50000"), Some(d("48000")), Some(d("53000")));
    let eth = Position::long("ETHUSDT", d("0.1"), d("3000"), Some(d("2800")), Some(d("3200")));
    let sol = Position::long("SOLUSDT", d("0.4"), d("140"), Some(d("130")), Some(d("150")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.live_book = true;
    snap.account_ok = true;
    snap.open_positions = vec![btc.clone(), eth.clone(), sol.clone()];
    snap.position = Some(btc.clone());
    let mut state = EngineState::new(1);
    state.positions = vec![btc, eth, sol];
    state.position = state.positions.first().cloned();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        trail_pct: d("0.020"),
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
    let amends: Vec<String> = decisions
        .iter()
        .filter(|d| is_amend(d))
        .map(|d| d.symbol().to_string())
        .collect();
    assert_eq!(amends.len(), 3, "{decisions:?}");
    let mut set = amends;
    set.sort();
    assert_eq!(set, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
}

#[test]
fn leftover_short_blocks_new_entries() {
    let short = Position {
        symbol: "BTCUSDT".into(),
        side: Side::Short,
        qty: d("0.004"),
        entry_price: d("68600"),
        stop_loss: None,
        take_profit: None,
        unrealized_pnl: d("-2"),
        opened_bar_time: None,
        leverage: 0,
    };
    let mut snap = MarketSnapshot::empty(d("1"));
    snap.tickers = majors();
    snap.account = account();
    snap.chart_symbol = "ETHUSDT".into();
    snap.live_book = true;
    snap.open_positions = vec![short];
    let (_, decisions) = tick_decisions(
        &EngineState::new(2),
        &snap,
        make_ts(),
        None,
        Some(&scalp_loose()),
        None,
    );
    assert!(!decisions.iter().any(is_enter));
    assert!(decisions[0].reason().contains("хвост"));
    assert!(decisions[0].reason().contains("SHORT"));
}

fn make_ts() -> f64 {
    london_ts() + 4.0 * 60.0
}

#[test]
fn scalp_leaves_dead_btc_for_eth_setup() {
    let dead = scalp_down();
    let live = grind_then_pullback(london_ms());
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = majors();
    snap.bars = dead.clone();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.universe_bars = [
        ("BTCUSDT".into(), dead.clone()),
        ("ETHUSDT".into(), live),
        ("SOLUSDT".into(), dead),
    ]
    .into_iter()
    .collect();
    let empty = HashMap::new();
    let (decision, _) = decide(
        2,
        &snap,
        london_ts() + 4.0 * 60.0,
        0.0,
        None,
        Some(&scalp_loose()),
        None,
        None,
        &[],
        &empty,
    )
    .unwrap();
    match decision {
        Decision::EnterLong { symbol, .. } => assert_eq!(symbol, "ETHUSDT"),
        other => panic!("expected enter, got {other:?} {}", other.reason()),
    }
}

#[test]
fn trend_leaves_dead_btc_for_eth_breakout() {
    let dead = grind_down();
    let live = range_then_breakout();
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = majors();
    snap.bars = dead.clone();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.universe_bars = [
        ("BTCUSDT".into(), dead.clone()),
        ("ETHUSDT".into(), live),
        ("SOLUSDT".into(), dead),
    ]
    .into_iter()
    .collect();
    let empty = HashMap::new();
    let (decision, _) = decide(3, &snap, 1.0, 0.0, None, None, Some(&trend_loose()), None, &[], &empty).unwrap();
    match decision {
        Decision::EnterLong { symbol, .. } => assert_eq!(symbol, "ETHUSDT"),
        other => panic!("expected enter, got {other:?} {}", other.reason()),
    }
}

fn green_5m() -> Bar {
    Bar {
        open_time: 1_700_000_000_000,
        open: d("100"),
        high: d("102"),
        low: d("99"),
        close: d("101"),
        volume: d("20"),
    }
}

fn red_5m() -> Bar {
    Bar {
        open_time: 1_700_000_000_000,
        open: d("101"),
        high: d("102"),
        low: d("98"),
        close: d("99"),
        volume: d("20"),
    }
}

fn s4_liquid_ticker() -> Ticker {
    Ticker::new("BTCUSDT", d("100"), d("2.0"), d("50000000"))
}

#[test]
fn strategy4_liquid_continuation_enters() {
    let snap = strategy4_ready_snap();
    let state = EngineState::new(4);
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        decisions.iter().any(is_enter),
        "{:?}",
        decisions.iter().map(|d| d.reason().to_string()).collect::<Vec<_>>()
    );
    assert_eq!(
        decisions.iter().find(|d| is_enter(d)).unwrap().symbol(),
        "BTCUSDT"
    );
}

#[test]
fn strategy4_illiquid_weekly_leader_is_not_chased() {
    let mut dust = Ticker::new("GPSUSDT", d("0.02"), d("25"), d("100"));
    dust.week_change_percent = d("20");
    dust.high_price = d("0.0202");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![dust];
    snap.account = account();
    snap.chart_symbol = "GPSUSDT".into();
    snap.account_ok = true;
    snap.bars = vec![green_5m(), green_5m()];
    snap.last_bars = [("GPSUSDT".into(), green_5m())].into_iter().collect();
    let state = EngineState::new(4);
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn strategy4_missing_5m_bar_does_not_enter() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    let state = EngineState::new(4);
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn strategy4_missing_stop_attaches_from_entry_not_mark() {
    let pos = Position::long("BTCUSDT", d("0.01"), d("100"), None, Some(d("200")));
    let mut ticker = s4_liquid_ticker();
    ticker.last_price = d("99.5");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![ticker];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    snap.bars = vec![green_5m(), green_5m()];
    snap.last_bars = [("BTCUSDT".into(), green_5m())].into_iter().collect();
    let mut state = EngineState::new(4);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    let amend = decisions
        .iter()
        .find(|d| is_amend(d))
        .unwrap_or_else(|| panic!("{decisions:?}"));
    match amend {
        Decision::AmendStop { stop_loss, .. } => {
            let from_entry = candidate_stop(d("100"), "LONG", d("0.020")).unwrap();
            let from_mark = candidate_stop(d("99.5"), "LONG", d("0.020")).unwrap();
            assert_eq!(*stop_loss, from_entry);
            assert!(*stop_loss > from_mark);
            assert!(*stop_loss < d("100"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn strategy4_existing_stop_does_not_move_down() {
    let pos = Position::long("BTCUSDT", d("0.01"), d("100"), Some(d("99")), Some(d("200")));
    let mut ticker = s4_liquid_ticker();
    ticker.last_price = d("99.5");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![ticker];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    snap.bars = vec![green_5m(), green_5m()];
    snap.last_bars = [("BTCUSDT".into(), green_5m())].into_iter().collect();
    let mut state = EngineState::new(4);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_amend), "{decisions:?}");
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn strategy4_can_enter_three_liquid_names() {
    let tickers = vec![
        Ticker::new("BTCUSDT", d("100"), d("3.0"), d("50000000")),
        Ticker::new("ETHUSDT", d("100"), d("2.0"), d("40000000")),
        Ticker::new("SOLUSDT", d("100"), d("1.5"), d("20000000")),
        Ticker::new("GPSUSDT", d("0.02"), d("25"), d("100")),
    ];
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers;
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    attach_pullback(
        &mut snap,
        &[
            ("BTCUSDT", 100.0),
            ("ETHUSDT", 100.0),
            ("SOLUSDT", 100.0),
            ("GPSUSDT", 0.02),
        ],
    );
    let state = EngineState::new(4);
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (mut state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    let mut got: std::collections::HashSet<String> = decisions
        .iter()
        .filter(|d| is_enter(d))
        .map(|d| d.symbol().to_string())
        .collect();
    assert_eq!(got.len(), 1, "{decisions:?}");
    for step in 1..3 {
        let (ns, decs) = tick_decisions(&state, &snap, london_ts() + (step as f64) * 60.0, Some(&mom), None, None);
        state = ns;
        for d in decs.iter().filter(|d| is_enter(d)) {
            got.insert(d.symbol().to_string());
        }
    }
    assert_eq!(
        got,
        ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
        "{got:?}"
    );
    assert!(!got.contains("GPSUSDT"));
}

#[test]
fn strategy4_prefers_liquid_volume_over_hottest_percent() {
    let tickers = vec![
        Ticker::new("AAVEUSDT", d("140"), d("3.2"), d("5000000")),
        Ticker::new("LINKUSDT", d("14"), d("2.8"), d("4000000")),
        Ticker::new("NEARUSDT", d("4.1"), d("2.5"), d("3000000")),
        Ticker::new("BTCUSDT", d("50000"), d("0.8"), d("50000000")),
        Ticker::new("ETHUSDT", d("3000"), d("1.6"), d("40000000")),
        Ticker::new("SOLUSDT", d("95"), d("2.0"), d("20000000")),
    ];
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers;
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    attach_pullback(
        &mut snap,
        &[
            ("AAVEUSDT", 140.0),
            ("LINKUSDT", 14.0),
            ("NEARUSDT", 4.1),
            ("BTCUSDT", 50000.0),
            ("ETHUSDT", 3000.0),
            ("SOLUSDT", 95.0),
        ],
    );
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), Some(&mom), None, None);
    let enters: Vec<_> = decisions.iter().filter(|d| is_enter(d)).collect();
    assert_eq!(enters.len(), 1, "{decisions:?}");
    assert_eq!(enters[0].symbol(), "BTCUSDT");
}

#[test]
fn strategy4_enters_growth_alt_when_it_has_the_only_pullback() {
    let tickers = vec![
        Ticker::new("AAVEUSDT", d("140"), d("3.2"), d("5000000")),
        Ticker::new("BTCUSDT", d("50000"), d("0.8"), d("50000000")),
        Ticker::new("ETHUSDT", d("3000"), d("1.6"), d("40000000")),
        Ticker::new("SOLUSDT", d("95"), d("2.0"), d("20000000")),
    ];
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers;
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    snap.bars = vec![green_5m(), green_5m()];
    snap.last_bars = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
        .into_iter()
        .map(|s| (s.to_string(), green_5m()))
        .collect();
    attach_pullback(&mut snap, &[("AAVEUSDT", 140.0)]);
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), Some(&mom), None, None);
    let enters: Vec<_> = decisions.iter().filter(|d| is_enter(d)).collect();
    assert_eq!(enters.len(), 1, "{decisions:?}");
    assert_eq!(enters[0].symbol(), "AAVEUSDT");
}

#[test]
fn strategy4_does_not_chase_green_5m_without_pullback() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    snap.bars = vec![green_5m(), green_5m()];
    snap.last_bars = [("BTCUSDT".into(), green_5m())].into_iter().collect();
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
    assert!(
        decisions.iter().any(|d| d.reason().contains("отката")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_does_not_chase_24h_stretch() {
    let tickers = vec![
        Ticker::new("MORPHOUSDT", d("2.87"), d("9.8"), d("300000")),
        Ticker::new("SPKUSDT", d("0.0225"), d("8.4"), d("200000")),
        Ticker::new("GRASSUSDT", d("0.364"), d("7.1"), d("150000")),
        Ticker::new("SUPERUSDT", d("0.109"), d("6.2"), d("180000")),
        Ticker::new("BTCUSDT", d("50000"), d("0.8"), d("50000000")),
        Ticker::new("ETHUSDT", d("3000"), d("1.6"), d("40000000")),
        Ticker::new("SOLUSDT", d("95"), d("2.0"), d("20000000")),
    ];
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers;
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    attach_pullback(
        &mut snap,
        &[
            ("BTCUSDT", 50000.0),
            ("ETHUSDT", 3000.0),
            ("SOLUSDT", 95.0),
        ],
    );
    let mom = MomentumParams {
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), Some(&mom), None, None);
    let enters: std::collections::HashSet<_> = decisions
        .iter()
        .filter(|d| is_enter(d))
        .map(|d| d.symbol().to_string())
        .collect();
    assert!(
        !enters.contains("MORPHOUSDT")
            && !enters.contains("SPKUSDT")
            && !enters.contains("GRASSUSDT")
            && !enters.contains("SUPERUSDT"),
        "{decisions:?}"
    );
    assert!(
        decisions.iter().any(|d| d.reason().contains("улетело") || is_enter(d)),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_exits_former_growth_leader_that_reverses() {
    let pos = Position::long("SPKUSDT", d("100"), d("0.02"), Some(d("0.019")), Some(d("0.03")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        Ticker::new("MORPHOUSDT", d("2.87"), d("26.3"), d("300000")),
        Ticker::new("GRASSUSDT", d("0.364"), d("21.8"), d("150000")),
        Ticker::new("SPKUSDT", d("0.019"), d("1.0"), d("200000")),
    ];
    snap.account = account();
    snap.chart_symbol = "SPKUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    snap.bars = vec![red_5m(), red_5m()];
    snap.last_bars = [("SPKUSDT".into(), red_5m())].into_iter().collect();
    let mut state = EngineState::new(4);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    state.recent_leaders = vec!["SPKUSDT".into(), "MORPHOUSDT".into(), "GRASSUSDT".into()];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })
            && d.symbol() == "SPKUSDT"
            && d.reason().contains("разворот")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_exits_when_mark_breaks_5m_low() {
    let pos = Position::long(
        "AAVEUSDT",
        d("0.2"),
        d("140"),
        Some(d("137")),
        Some(d("144")),
    );
    let signal = pullback_last_at(140.0);
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AAVEUSDT", d("139.0"), d("3.2"), d("5000000"))];
    snap.account = account();
    snap.chart_symbol = "AAVEUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    snap.last_bars = [("AAVEUSDT".into(), signal.clone())].into_iter().collect();
    let below = signal.low - d("0.05");
    snap.tickers[0].last_price = below;
    let mut state = EngineState::new(4);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })
            && d.symbol() == "AAVEUSDT"
            && d.reason().contains("минимума")),
        "{decisions:?}"
    );
}

#[test]
fn does_not_chase_already_pumped_over_12pct() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        Ticker::new("MORPHOUSDT", d("2.87"), d("26.3"), d("300000")),
        Ticker::new("SPKUSDT", d("0.022"), d("25.4"), d("200000")),
        Ticker::new("BTCUSDT", d("50000"), d("9.5"), d("800000")),
    ];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(1), &snap, london_ts(), Some(&mom), None, None);
    let enters: Vec<_> = decisions.iter().filter(|d| is_enter(d)).map(|d| d.symbol().to_string()).collect();
    assert_eq!(enters, vec!["BTCUSDT".to_string()], "{decisions:?}");
}

#[test]
fn does_not_enter_near_24h_high() {
    let mut pumped = Ticker::new("STORJUSDT", d("0.048"), d("9.5"), d("200000"));
    pumped.high_price = d("0.0482");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        pumped,
        Ticker::new("BTCUSDT", d("50000"), d("2.0"), d("800000")),
    ];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(1), &snap, london_ts(), Some(&mom), None, None);
    assert!(
        !decisions.iter().any(|d| is_enter(d) && d.symbol() == "STORJUSDT"),
        "{decisions:?}"
    );
    assert!(decisions.iter().any(|d| is_enter(d) && d.symbol() == "BTCUSDT"), "{decisions:?}");
}

#[test]
fn exits_open_long_on_red_5m_instead_of_waiting_for_sl() {
    let pos = Position::long("BTCUSDT", d("0.01"), d("50000"), Some(d("49000")), Some(d("52000")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    snap.last_bars = [("BTCUSDT".into(), red_5m())].into_iter().collect();
    let mut state = EngineState::new(1);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(
        decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })
            && d.symbol() == "BTCUSDT"
            && d.reason().contains("5м")),
        "{decisions:?}"
    );
}

#[test]
fn exits_when_name_drops_off_the_growth_book() {
    let pos = Position::long("MORPHOUSDT", d("13.8"), d("2.9"), Some(d("2.8")), Some(d("3.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    let mut tickers = tickers();
    tickers.push(Ticker::new("MORPHOUSDT", d("2.9"), d("0.2"), d("300000")));
    snap.tickers = tickers;
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    let mut state = EngineState::new(1);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(
        decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })
            && d.symbol() == "MORPHOUSDT"
            && d.reason().contains("топа")),
        "{decisions:?}"
    );
}

fn attach_pullback(snap: &mut MarketSnapshot, rows: &[(&str, f64)]) {
    for (sym, mark) in rows {
        let seq = pullback_5m_at(*mark);
        let last = seq.last().cloned().expect("pullback");
        snap.last_bars.insert((*sym).into(), last);
        if snap.chart_symbol == *sym || snap.bars.is_empty() {
            snap.bars = seq.clone();
        }
        snap.universe_bars.insert((*sym).into(), seq);
    }
}

fn strategy4_ready_snap() -> MarketSnapshot {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    attach_pullback(&mut snap, &[("BTCUSDT", 100.0)]);
    snap
}

#[test]
fn strategy4_outside_session_does_not_enter_when_always_enter_off() {
    let snap = strategy4_ready_snap();
    let state = EngineState::new(4);
    let (_, decisions) = tick_decisions(&state, &snap, dead_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn strategy4_does_not_inherit_strategy1_always_enter() {
    let snap = strategy4_ready_snap();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, dead_ts(), Some(&mom), None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
    assert!(
        decisions.iter().any(|d| d.reason().contains("вне часов старта")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_enters_in_recommended_utc_windows() {
    let snap = strategy4_ready_snap();
    for hour in [0u32, 7, 13] {
        let ts = tui_bot::sessions::make_utc_ts(2026, 8, 17, hour, 30, 0);
        let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, ts, None, None, None);
        assert!(
            decisions.iter().any(is_enter),
            "hour {hour}: {:?}",
            decisions.iter().map(|d| d.reason().to_string()).collect::<Vec<_>>()
        );
    }
    for hour in [2u32, 4, 10, 12, 16, 22] {
        let ts = tui_bot::sessions::make_utc_ts(2026, 8, 17, hour, 0, 0);
        let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, ts, None, None, None);
        assert!(!decisions.iter().any(is_enter), "hour {hour}: {decisions:?}");
        assert!(
            decisions.iter().any(|d| d.reason().contains("вне часов старта")),
            "hour {hour}: {decisions:?}"
        );
    }
}

#[test]
fn strategy4_vanish_with_inflight_cools_and_does_not_rebuy() {
    let pos = Position::long(
        "SUPERUSDT",
        d("367"),
        d("0.109"),
        Some(d("0.107")),
        Some(d("0.112")),
    );
    let mut snap = strategy4_ready_snap();
    snap.live_book = true;
    snap.tickers = vec![
        Ticker::new("SUPERUSDT", d("0.109"), d("3.2"), d("180000")),
        s4_liquid_ticker(),
    ];
    attach_pullback(&mut snap, &[("SUPERUSDT", 0.109)]);
    snap.chart_symbol = "SUPERUSDT".into();
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    state.inflight_symbols = vec!["SUPERUSDT".into()];
    state.entry_inflight = true;
    let london = london_ts();
    let (cooled, decisions) = tick_decisions(&state, &snap, london, None, None, None);
    assert!(
        !decisions
            .iter()
            .any(|d| is_enter(d) && d.symbol().eq_ignore_ascii_case("SUPERUSDT")),
        "{decisions:?}"
    );
    assert!(
        cooled.cooldowns.get("SUPERUSDT").copied().unwrap_or(0.0) > london,
        "{:?}",
        cooled.cooldowns
    );
    assert!(
        !cooled
            .inflight_symbols
            .iter()
            .any(|s| s.eq_ignore_ascii_case("SUPERUSDT")),
        "{:?}",
        cooled.inflight_symbols
    );
    let (_, again) = tick_decisions(&cooled, &snap, london + 900.0, None, None, None);
    assert!(
        !again
            .iter()
            .any(|d| is_enter(d) && d.symbol().eq_ignore_ascii_case("SUPERUSDT")),
        "rebuy inside 30m: {again:?}"
    );
}

#[test]
fn strategy4_exits_open_long_on_red_5m_instead_of_waiting_for_sl() {
    let pos = Position::long(
        "AAVEUSDT",
        d("0.2"),
        d("140"),
        Some(d("137")),
        Some(d("144")),
    );
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AAVEUSDT", d("139"), d("3.2"), d("300000"))];
    snap.account = account();
    snap.chart_symbol = "AAVEUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    snap.last_bars = [("AAVEUSDT".into(), red_5m())].into_iter().collect();
    let mut state = EngineState::new(4);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })
            && d.symbol() == "AAVEUSDT"
            && d.reason().contains("5м")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_seeded_cooldown_blocks_super_rebuy() {
    let mut snap = strategy4_ready_snap();
    snap.tickers = vec![
        Ticker::new("SUPERUSDT", d("0.109"), d("3.2"), d("180000")),
        s4_liquid_ticker(),
    ];
    attach_pullback(&mut snap, &[("SUPERUSDT", 0.109)]);
    let mut state = EngineState::new(4);
    state
        .cooldowns
        .insert("SUPERUSDT".into(), london_ts() + 1800.0);
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        !decisions
            .iter()
            .any(|d| is_enter(d) && d.symbol().eq_ignore_ascii_case("SUPERUSDT")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_skips_thin_alts_and_pennies() {
    let mut snap = strategy4_ready_snap();
    snap.tickers = vec![
        Ticker::new("FARTCOINUSDT", d("0.178"), d("3.1"), d("80000")),
        Ticker::new("ZILUSDT", d("0.0027"), d("2.8"), d("90000")),
        Ticker::new("DOGSUSDT", d("0.00004"), d("2.5"), d("120000")),
        s4_liquid_ticker(),
    ];
    snap.last_bars = ["FARTCOINUSDT", "ZILUSDT", "DOGSUSDT", "BTCUSDT"]
        .into_iter()
        .map(|s| (s.to_string(), green_5m()))
        .collect();
    let mom = MomentumParams {
        max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), Some(&mom), None, None);
    assert!(
        !decisions.iter().any(|d| is_enter(d)
            && ["FARTCOINUSDT", "ZILUSDT", "DOGSUSDT"]
                .iter()
                .any(|s| d.symbol().eq_ignore_ascii_case(s))),
        "{decisions:?}"
    );
    assert!(
        decisions.iter().any(|d| is_enter(d) && d.symbol() == "BTCUSDT"),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_desk_pause_after_stop_does_not_fill_next_alt() {
    let pos = Position::long("LAUSDT", d("100"), d("0.06"), Some(d("0.058")), Some(d("0.062")));
    let mut snap = strategy4_ready_snap();
    snap.live_book = true;
    snap.tickers = vec![
        Ticker::new("GTCUSDT", d("0.086"), d("3.0"), d("5000000")),
        s4_liquid_ticker(),
    ];
    attach_pullback(&mut snap, &[("GTCUSDT", 0.086)]);
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let london = london_ts();
    let (cooled, decisions) = tick_decisions(&state, &snap, london, None, None, None);
    assert!(!decisions.iter().any(is_enter), "rotated into next alt: {decisions:?}");
    let window_end = tui_bot::sessions::window_end_ts(london, &tui_bot::sessions::DEFAULT_ENTRY_WINDOWS)
        .expect("london window");
    assert!(
        cooled.cooldown_until >= window_end,
        "desk pause until window end {window_end}, got {}",
        cooled.cooldown_until
    );
    let (_, again) = tick_decisions(&cooled, &snap, london + 60.0, None, None, None);
    assert!(!again.iter().any(is_enter), "desk still paused: {again:?}");
}

#[test]
fn strategy4_always_enter_knob_opens_dead_hours() {
    let snap = strategy4_ready_snap();
    let mom = MomentumParams {
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        max_positions: 1,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, dead_ts(), Some(&mom), None, None);
    assert!(
        decisions.iter().any(is_enter),
        "{:?}",
        decisions.iter().map(|d| d.reason().to_string()).collect::<Vec<_>>()
    );
}

#[test]
fn strategy4_win_vanish_allows_next_liquid() {
    let pos = Position::long(
        "BTCUSDT",
        d("0.01"),
        d("100"),
        Some(d("99.2")),
        Some(d("101.5")),
    );
    let mut snap = strategy4_ready_snap();
    snap.live_book = true;
    snap.tickers = vec![
        Ticker::new("BTCUSDT", d("102"), d("2.0"), d("50000000")),
        Ticker::new("ETHUSDT", d("100"), d("1.8"), d("40000000")),
    ];
    attach_pullback(&mut snap, &[("BTCUSDT", 102.0), ("ETHUSDT", 100.0)]);
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let london = london_ts();
    let (cooled, decisions) = tick_decisions(&state, &snap, london, None, None, None);
    assert!(
        cooled.cooldown_until <= london,
        "winning vanish must not desk-pause: {}",
        cooled.cooldown_until
    );
    assert!(
        decisions
            .iter()
            .any(|d| is_enter(d) && d.symbol() == "ETHUSDT"),
        "{decisions:?}"
    );
    assert!(
        !decisions
            .iter()
            .any(|d| is_enter(d) && d.symbol() == "BTCUSDT"),
        "rebuy winner: {decisions:?}"
    );
}



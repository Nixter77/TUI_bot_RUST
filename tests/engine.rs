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
fn is_reduce(d: &Decision) -> bool {
    matches!(d, Decision::ReduceLong { .. })
}

#[test]
fn poll_is_one_or_two_minutes() {
    assert!(STRATEGY1_POLL_SECONDS == 60 || STRATEGY1_POLL_SECONDS == 120);
    let p = MomentumParams::default();
    assert!(p.poll_seconds == 60 || p.poll_seconds == 120);
}

#[test]
fn bad_poll_seconds_is_hold_not_panic() {
    let params = MomentumParams {
        poll_seconds: 5,
        always_enter: true,
        ..MomentumParams::default()
    };
    let (decision, _) = momentum_decision(&tickers(), None, 1000.0, 0.0, Some(&params));
    match decision {
        Decision::Hold { reason } => assert!(reason.contains("poll_seconds"), "{reason}"),
        other => panic!("expected hold, got {other:?}"),
    }
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
        s4_max_positions: 1,
        ..MomentumParams::default()
    };
    let (filled, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), Some(&mom), None, None);
    assert!(decisions.iter().any(is_enter), "{decisions:?}");
    assert!(
        filled
            .inflight_symbols
            .iter()
            .any(|s| s.eq_ignore_ascii_case("AVAXUSDT")),
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
        s4_max_positions: 3,
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
        s4_max_positions: 3,
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
        s4_max_positions: 3,
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
        s4_max_positions: 3,
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
fn daily_loss_usdt_halt_still_trips_via_equity() {
    // OR layers: R budget 75 but USDT=20 still trips at −20.
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.account.wallet_balance = d("9980");
    snap.account.unrealized_pnl = d("0");
    snap.account_ok = true;
    snap.live_book = true;
    snap.chart_symbol = "BTCUSDT".into();
    let mut state = EngineState::new(1);
    state.day_utc = "2026-08-17".into();
    state.day_start_equity = Some(d("10000"));
    let mom = MomentumParams {
        always_enter: true,
        daily_loss_usdt: d("20"),
        daily_loss_r: d("3"),
        risk_pct: d("0.0025"),
        ..MomentumParams::default()
    };
    let (new_state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(new_state.daily_halt, "USDT −20 must still halt under larger R budget");
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn daily_loss_r_halt_blocks_enter_independent_of_usdt() {
    // USDT limit huge; R=3 → 75; wallet 9925 → −75 trips on R alone.
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.account.wallet_balance = d("9925");
    snap.account.unrealized_pnl = d("0");
    snap.account_ok = true;
    snap.live_book = true;
    snap.chart_symbol = "BTCUSDT".into();
    let mut state = EngineState::new(1);
    state.day_utc = "2026-08-17".into();
    state.day_start_equity = Some(d("10000"));
    let mom = MomentumParams {
        always_enter: true,
        daily_loss_usdt: d("10000"),
        daily_loss_r: d("3"),
        risk_pct: d("0.0025"),
        ..MomentumParams::default()
    };
    let (new_state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(new_state.daily_halt, "R layer must halt at −75 alone");
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}


#[test]
fn daily_usdt_halt_trips_from_equity_pnl() {
    // day start 10000; wallet+uPnL = 9979 → −21 ≤ −20 USDT. R budget huge.
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.account.wallet_balance = d("9979");
    snap.account.unrealized_pnl = Decimal::ZERO;
    snap.account_ok = true;
    snap.live_book = true;
    let mut state = EngineState::new(1);
    state.day_utc = "2026-08-17".into();
    state.day_start_equity = Some(d("10000"));
    let mom = MomentumParams {
        always_enter: true,
        daily_loss_usdt: d("20"),
        daily_loss_r: d("3"), // larger R budget; USDT still trips (OR)
        risk_pct: d("0.0025"),
        ..MomentumParams::default()
    };
    let (new_state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(new_state.daily_halt, "USDT layer must trip");
    assert!(!decisions.iter().any(is_enter));
}

#[test]
fn daily_loss_r_halt_trips_independently() {
    // USDT=20 or R floor=75; equity 9920 → −80 trips (either layer).
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.account.wallet_balance = d("9920");
    snap.account.unrealized_pnl = Decimal::ZERO;
    snap.account_ok = true;
    snap.live_book = true;
    let mut state = EngineState::new(1);
    state.day_utc = "2026-08-17".into();
    state.day_start_equity = Some(d("10000"));
    let mom = MomentumParams {
        always_enter: true,
        daily_loss_usdt: d("20"),
        daily_loss_r: d("3"),
        risk_pct: d("0.0025"),
        ..MomentumParams::default()
    };
    let (new_state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(new_state.daily_halt, "day halt must trip at −80");
    assert!(!decisions.iter().any(is_enter));
    assert!(
        decisions.iter().any(|d| d.reason().contains("стоп дня") || is_hold(d)),
        "{decisions:?}"
    );
}

#[test]
fn daily_loss_r_halt_keeps_manage_trail() {
    let mut pos = Position::long("BTCUSDT", d("0.01"), d("50000"), Some(d("49000")), Some(d("52000")));
    pos.unrealized_pnl = d("10");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("BTCUSDT", d("50500"), d("1"), d("8000"))];
    snap.account = account();
    // Deep red day (−80); halt latched, open long still managed.
    snap.account.wallet_balance = d("9920");
    snap.account.unrealized_pnl = Decimal::ZERO;
    snap.position = Some(pos.clone());
    snap.chart_symbol = "BTCUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    let mut state = EngineState::new(1);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    state.day_utc = "2026-08-17".into();
    state.day_start_equity = Some(d("10000"));
    let mom = MomentumParams {
        always_enter: true,
        trail_pct: d("0.006"),
        daily_loss_usdt: d("20"),
        daily_loss_r: d("3"),
        risk_pct: d("0.0025"),
        ..MomentumParams::default()
    };
    let (new_state, decisions) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(new_state.daily_halt);
    assert!(!decisions.iter().any(is_enter));
    assert!(decisions.iter().any(|d| is_amend(d) || is_hold(d)), "{decisions:?}");
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
        s4_max_positions: 3,
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
        s4_max_positions: 3,
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
        s4_max_positions: 3,
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
    Ticker::new("AVAXUSDT", d("100"), d("2.0"), d("50000000"))
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
        "AVAXUSDT"
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
fn strategy4_15m_interval_names_the_skip() {
    use tui_bot::config::TradeInterval;
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    let mom = MomentumParams {
        s4_interval: TradeInterval::Minute15,
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), Some(&mom), None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
    assert!(
        decisions.iter().any(|d| d.reason().contains("15м")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_missing_5m_bar_does_not_enter() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    let state = EngineState::new(4);
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn strategy4_missing_stop_attaches_from_entry_not_mark() {
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), None, Some(d("200")));
    let mut ticker = s4_liquid_ticker();
    ticker.last_price = d("99.5");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![ticker];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    snap.bars = vec![green_5m(), green_5m()];
    snap.last_bars = [("AVAXUSDT".into(), green_5m())].into_iter().collect();
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
            let from_entry = candidate_stop(d("100"), "LONG", d("0.015")).unwrap();
            let from_mark = candidate_stop(d("99.5"), "LONG", d("0.015")).unwrap();
            assert_eq!(*stop_loss, from_entry);
            assert!(*stop_loss > from_mark);
            assert!(*stop_loss < d("100"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn strategy4_existing_stop_does_not_move_down() {
    let mut pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("99")), Some(d("200")));
    pos.opened_bar_time = Some(london_ms());
    let mut ticker = s4_liquid_ticker();
    ticker.last_price = d("99.5");
    let quiet = Bar {
        open_time: london_ms(),
        open: d("99.4"),
        high: d("99.6"),
        low: d("99.2"),
        close: d("99.5"),
        volume: d("20"),
    };
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![ticker];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    snap.bars = vec![quiet.clone()];
    snap.universe_bars.insert("AVAXUSDT".into(), vec![quiet.clone()]);
    snap.last_bars = [("AVAXUSDT".into(), quiet)].into_iter().collect();
    let mut state = EngineState::new(4);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_amend), "{decisions:?}");
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

fn adopt_green_s4_fill(state: &mut EngineState, snap: &mut MarketSnapshot, symbol: &str) {
    let mark = snap
        .tickers
        .iter()
        .find(|t| t.symbol == symbol)
        .map(|t| t.last_price)
        .unwrap_or(d("100"));
    let mut pos = Position::long(symbol, d("0.01"), mark, Some(mark * d("0.985")), Some(mark * d("1.03")));
    pos.unrealized_pnl = d("0.001");
    state.positions.retain(|p| p.symbol != symbol);
    state.positions.push(pos.clone());
    state.position = state.positions.first().cloned();
    state.inflight_symbols.retain(|s| !s.eq_ignore_ascii_case(symbol));
    snap.live_book = true;
    snap.open_positions = state.positions.clone();
    snap.position = state.position.clone();
}

#[test]
fn strategy4_can_enter_three_liquid_names() {
    let tickers = vec![
        Ticker::new("AVAXUSDT", d("100"), d("3.0"), d("50000000")),
        Ticker::new("LINKUSDT", d("100"), d("2.0"), d("40000000")),
        Ticker::new("ADAUSDT", d("100"), d("1.5"), d("20000000")),
        Ticker::new("GPSUSDT", d("0.02"), d("25"), d("100")),
    ];
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers;
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    attach_pullback(
        &mut snap,
        &[
            ("AVAXUSDT", 100.0),
            ("LINKUSDT", 100.0),
            ("ADAUSDT", 100.0),
            ("GPSUSDT", 0.02),
        ],
    );
    let mut state = EngineState::new(4);
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        s4_max_positions: 3,
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        ..MomentumParams::default()
    };
    let mut got: std::collections::HashSet<String> = std::collections::HashSet::new();
    for step in 0..3 {
        let (ns, decs) = tick_decisions(&state, &snap, london_ts() + (step as f64) * 60.0, Some(&mom), None, None);
        state = ns;
        let entered: Vec<String> = decs
            .iter()
            .filter(|d| is_enter(d))
            .map(|d| d.symbol().to_string())
            .collect();
        assert_eq!(entered.len(), 1, "step {step}: {decs:?}");
        for sym in &entered {
            got.insert(sym.clone());
            adopt_green_s4_fill(&mut state, &mut snap, sym);
        }
    }
    assert_eq!(
        got,
        ["AVAXUSDT", "LINKUSDT", "ADAUSDT"]
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
        "{got:?}"
    );
    assert!(!got.contains("GPSUSDT"));
}

#[test]
fn strategy4_prefers_liquid_volume_over_hottest_percent() {
    use tui_bot::continuation::{liquid_universe, ContinuationParams};
    // Hotter % AAVE is thinner; AVAX wins volume among non-majors. Majors excluded.
    let tickers = vec![
        Ticker::new("AAVEUSDT", d("140"), d("3.2"), d("5000000")),
        Ticker::new("BTCUSDT", d("50000"), d("1.6"), d("90000000")),
        Ticker::new("ETHUSDT", d("3000"), d("1.6"), d("80000000")),
        Ticker::new("SOLUSDT", d("95"), d("2.0"), d("70000000")),
        Ticker::new("AVAXUSDT", d("100"), d("1.6"), d("50000000")),
    ];
    let uni = liquid_universe(&tickers, &[], &ContinuationParams::default());
    assert!(!uni.is_empty());
    assert_eq!(
        uni[0].symbol, "AVAXUSDT",
        "highest non-major volume first: {:?}",
        uni.iter().map(|t| t.symbol.as_str()).collect::<Vec<_>>()
    );
    assert!(!uni.iter().any(|t| t.symbol == "BTCUSDT"));
    if let Some(i) = uni.iter().position(|t| t.symbol == "AAVEUSDT") {
        assert!(i > 0, "AAVE must rank below AVAX");
    }
}
#[test]
fn strategy4_does_not_chase_green_5m_without_pullback() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    let greens = vec![green_5m(), green_5m()];
    snap.bars = greens.clone();
    snap.universe_bars.insert("AVAXUSDT".into(), greens);
    snap.last_bars = [("AVAXUSDT".into(), green_5m())].into_iter().collect();
    snap.htf_bars.insert("AVAXUSDT".into(), htf_up_4h_at(100.0));
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
        Ticker::new("AVAXUSDT", d("100"), d("2.0"), d("50000000")),
        Ticker::new("LINKUSDT", d("14"), d("1.6"), d("40000000")),
        Ticker::new("ADAUSDT", d("0.55"), d("1.8"), d("20000000")),
    ];
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers;
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    attach_pullback(
        &mut snap,
        &[
            ("AVAXUSDT", 100.0),
            ("LINKUSDT", 14.0),
            ("ADAUSDT", 0.55),
        ],
    );
    let mom = MomentumParams {
        max_positions: 3,
        s4_max_positions: 3,
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
        decisions.iter().any(is_enter),
        "liquid mid-tape pullback should still enter: {decisions:?}"
    );
}

#[test]
fn strategy4_enters_green_day_off_the_24h_high() {
    let mut avax = Ticker::new("AVAXUSDT", d("100"), d("8.0"), d("50000000"));
    avax.high_price = d("110");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![avax];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    attach_pullback(&mut snap, &[("AVAXUSDT", 100.0)]);
    let mom = MomentumParams {
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        max_positions: 1,
        s4_max_positions: 1,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, dead_ts(), Some(&mom), None, None);
    assert!(
        decisions.iter().any(|d| is_enter(d) && d.symbol() == "AVAXUSDT"),
        "pullback of a +8% liquid name off the high must enter: {decisions:?}"
    );
}

#[test]
fn strategy4_holds_former_leader_until_stop() {
    let pos = Position::long("AAVEUSDT", d("0.2"), d("140"), Some(d("137")), Some(d("146")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        Ticker::new("MORPHOUSDT", d("2.87"), d("26.3"), d("300000")),
        Ticker::new("GRASSUSDT", d("0.364"), d("21.8"), d("150000")),
        Ticker::new("AAVEUSDT", d("139"), d("1.0"), d("5000000")),
    ];
    snap.account = account();
    snap.chart_symbol = "AAVEUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    snap.last_bars = [("AAVEUSDT".into(), red_5m())].into_iter().collect();
    let mut state = EngineState::new(4);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    state.recent_leaders = vec!["AAVEUSDT".into(), "MORPHOUSDT".into(), "GRASSUSDT".into()];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        !decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })),
        "red 5m / dropped tape must not dump a long still above SL: {decisions:?}"
    );
}

#[test]
fn strategy4_exits_when_mark_hits_placed_stop() {
    let pos = Position::long(
        "AAVEUSDT",
        d("0.2"),
        d("140"),
        Some(d("137")),
        Some(d("144")),
    );
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AAVEUSDT", d("136.9"), d("3.2"), d("5000000"))];
    snap.account = account();
    snap.chart_symbol = "AAVEUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    snap.last_bars = [("AAVEUSDT".into(), pullback_last_at(140.0))].into_iter().collect();
    let mut state = EngineState::new(4);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })
            && d.symbol() == "AAVEUSDT"
            && d.reason().contains("stop")),
        "{decisions:?}"
    );
}

#[test]
fn does_not_chase_already_pumped_over_12pct() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        Ticker::new("MORPHOUSDT", d("2.87"), d("26.3"), d("300000")),
        Ticker::new("SPKUSDT", d("0.022"), d("25.4"), d("200000")),
        Ticker::new("AVAXUSDT", d("50000"), d("9.5"), d("800000")),
    ];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        s4_max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(1), &snap, london_ts(), Some(&mom), None, None);
    let enters: Vec<_> = decisions.iter().filter(|d| is_enter(d)).map(|d| d.symbol().to_string()).collect();
    assert_eq!(enters, vec!["AVAXUSDT".to_string()], "{decisions:?}");
}

#[test]
fn does_not_enter_near_24h_high() {
    let mut pumped = Ticker::new("STORJUSDT", d("0.048"), d("9.5"), d("200000"));
    pumped.high_price = d("0.0482");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![
        pumped,
        Ticker::new("AVAXUSDT", d("50000"), d("2.0"), d("800000")),
    ];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        s4_max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(1), &snap, london_ts(), Some(&mom), None, None);
    assert!(
        !decisions.iter().any(|d| is_enter(d) && d.symbol() == "STORJUSDT"),
        "{decisions:?}"
    );
    assert!(decisions.iter().any(|d| is_enter(d) && d.symbol() == "AVAXUSDT"), "{decisions:?}");
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
    // Red 5m at BTC scale so S1 red-exit path sees the symbol bar.
    snap.last_bars = [(
        "BTCUSDT".into(),
        Bar {
            open_time: london_ms(),
            open: d("51000"),
            high: d("51100"),
            low: d("49500"),
            close: d("49800"),
            volume: d("20"),
        },
    )]
    .into_iter()
    .collect();
    let mut state = EngineState::new(1);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        s4_max_positions: 3,
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
    snap.chart_symbol = "AVAXUSDT".into();
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos);
    let mut state = EngineState::new(1);
    state.position = snap.position.clone();
    state.positions = snap.open_positions.clone();
    let mom = MomentumParams {
        always_enter: true,
        max_positions: 3,
        s4_max_positions: 3,
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
        snap.htf_bars.insert((*sym).into(), htf_up_4h_at(*mark));
    }
}

fn strategy4_ready_snap() -> MarketSnapshot {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    attach_pullback(&mut snap, &[("AVAXUSDT", 100.0)]);
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
        s4_max_positions: 3,
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
    attach_pullback(&mut snap, &[("SUPERUSDT", 0.109), ("AVAXUSDT", 100.0)]);
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
fn strategy4_holds_open_long_on_red_5m_above_stop() {
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
        !decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })),
        "red 5m above SL must hold for 2R: {decisions:?}"
    );
}

#[test]
fn strategy4_seeded_cooldown_blocks_super_rebuy() {
    let mut snap = strategy4_ready_snap();
    snap.tickers = vec![
        Ticker::new("SUPERUSDT", d("0.109"), d("3.2"), d("180000")),
        s4_liquid_ticker(),
    ];
    attach_pullback(&mut snap, &[("SUPERUSDT", 0.109), ("AVAXUSDT", 100.0)]);
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
    snap.last_bars.insert("FARTCOINUSDT".into(), green_5m());
    snap.last_bars.insert("ZILUSDT".into(), green_5m());
    snap.last_bars.insert("DOGSUSDT".into(), green_5m());
    let mom = MomentumParams {
        max_positions: 3,
        s4_max_positions: 3,
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
        decisions.iter().any(|d| is_enter(d) && d.symbol() == "AVAXUSDT"),
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
        s4_max_positions: 1,
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
    let mut pos = Position::long(
        "AVAXUSDT",
        d("0.01"),
        d("100"),
        Some(d("99.2")),
        Some(d("101.5")),
    );
    pos.unrealized_pnl = d("0.02");
    let mut snap = strategy4_ready_snap();
    snap.live_book = true;
    snap.open_positions.clear();
    snap.position = None;
    snap.tickers = vec![
        Ticker::new("AVAXUSDT", d("102"), d("2.0"), d("50000000")),
        Ticker::new("LINKUSDT", d("100"), d("1.8"), d("40000000")),
    ];
    attach_pullback(&mut snap, &[("AVAXUSDT", 102.0), ("LINKUSDT", 100.0)]);
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
            .any(|d| is_enter(d) && d.symbol() == "LINKUSDT"),
        "{decisions:?}"
    );
    assert!(
        !decisions
            .iter()
            .any(|d| is_enter(d) && d.symbol() == "AVAXUSDT"),
        "rebuy winner: {decisions:?}"
    );
}

#[test]
fn strategy4_skips_penny_mbox() {
    let mut snap = strategy4_ready_snap();
    snap.tickers = vec![
        Ticker::new("MBOXUSDT", d("0.00062"), d("3.1"), d("800000")),
        Ticker::new("BEATUSDT", d("0.1279"), d("3.2"), d("900000")),
        s4_liquid_ticker(),
    ];
    attach_pullback(
        &mut snap,
        &[("MBOXUSDT", 0.00062), ("BEATUSDT", 0.1279), ("AVAXUSDT", 100.0)],
    );
    let mom = MomentumParams {
        max_positions: 3,
        s4_max_positions: 3,
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, dead_ts(), Some(&mom), None, None);
    assert!(
        !decisions.iter().any(|d| is_enter(d)
            && ["MBOXUSDT", "BEATUSDT"]
                .iter()
                .any(|s| d.symbol().eq_ignore_ascii_case(s))),
        "{decisions:?}"
    );
    assert!(
        decisions.iter().any(|d| is_enter(d) && d.symbol() == "AVAXUSDT"),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_does_not_rebuy_loser_after_desk_pause() {
    let pos = Position::long(
        "LINKUSDT",
        d("0.02"),
        d("100"),
        Some(d("98")),
        Some(d("104")),
    );
    let mut snap = strategy4_ready_snap();
    snap.live_book = true;
    snap.open_positions.clear();
    snap.position = None;
    snap.tickers = vec![
        Ticker::new("LINKUSDT", d("99"), d("2.0"), d("40000000")),
        Ticker::new("AVAXUSDT", d("100"), d("2.0"), d("50000000")),
    ];
    attach_pullback(&mut snap, &[("LINKUSDT", 99.0), ("AVAXUSDT", 100.0)]);
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let mom = MomentumParams {
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        max_positions: 1,
        s4_max_positions: 1,
        ..MomentumParams::default()
    };
    let t0 = dead_ts();
    let (cooled, first) = tick_decisions(&state, &snap, t0, Some(&mom), None, None);
    assert!(!first.iter().any(is_enter), "desk must pause after loss: {first:?}");
    let eth_until = cooled.cooldowns.get("LINKUSDT").copied().unwrap_or(0.0);
    assert!(
        eth_until >= t0 + tui_bot::errors::LOSS_SYMBOL_COOLDOWN_SEC,
        "loser cooldown {eth_until} vs t0 {t0}"
    );
    let later = t0 + 1_860.0;
    let (_, again) = tick_decisions(&cooled, &snap, later, Some(&mom), None, None);
    assert!(
        !again
            .iter()
            .any(|d| is_enter(d) && d.symbol().eq_ignore_ascii_case("LINKUSDT")),
        "rebought loser after 31m: {again:?}"
    );
    assert!(
        again
            .iter()
            .any(|d| is_enter(d) && d.symbol() == "AVAXUSDT"),
        "other liquid should enter after desk pause: {again:?}"
    );
}

#[test]
fn strategy4_stop_is_at_least_one_and_a_half_percent() {
    let snap = strategy4_ready_snap();
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), None, None, None);
    let Decision::EnterLong {
        stop_loss,
        take_profit,
        ..
    } = decisions.iter().find(|d| is_enter(d)).unwrap_or_else(|| panic!("{decisions:?}"))
    else {
        panic!("{decisions:?}");
    };
    let mark = d("100");
    let risk = (mark - *stop_loss) / mark;
    assert!(
        risk >= d("0.015"),
        "SL {stop_loss} risk {risk} must be >= 1.5%"
    );
    assert!(*take_profit > mark + d("2") * (mark - *stop_loss));
}

#[test]
fn strategy4_15m_stop_and_tp_are_wider_than_five_minute() {
    use tui_bot::config::TradeInterval;
    let snap = strategy4_ready_snap();
    let mom = MomentumParams {
        s4_interval: TradeInterval::Minute15,
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), Some(&mom), None, None);
    let Decision::EnterLong {
        stop_loss,
        take_profit,
        ..
    } = decisions.iter().find(|d| is_enter(d)).unwrap_or_else(|| panic!("{decisions:?}"))
    else {
        panic!("{decisions:?}");
    };
    let mark = d("100");
    let risk = (mark - *stop_loss) / mark;
    assert!(
        risk >= d("0.020"),
        "15m SL {stop_loss} risk {risk} must be >= 2%"
    );
    assert!(risk <= d("0.050"), "15m SL {stop_loss} risk {risk} must be <= 5%");
    let r = mark - *stop_loss;
    assert!(
        *take_profit >= mark + d("2") * r,
        "15m TP {take_profit} must be at least 2R above entry"
    );
}

#[test]
fn strategy4_skips_weak_24h_change() {
    let mut snap = strategy4_ready_snap();
    snap.tickers = vec![
        Ticker::new("DOTUSDT", d("270.50"), d("0.446"), d("40000000")),
        s4_liquid_ticker(),
    ];
    attach_pullback(&mut snap, &[("DOTUSDT", 270.50), ("AVAXUSDT", 100.0)]);
    let mom = MomentumParams {
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        max_positions: 1,
        s4_max_positions: 1,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, dead_ts(), Some(&mom), None, None);
    assert!(
        !decisions
            .iter()
            .any(|d| is_enter(d) && d.symbol() == "DOTUSDT"),
        "{decisions:?}"
    );
    assert!(
        decisions.iter().any(|d| is_enter(d) && d.symbol() == "AVAXUSDT"),
        "{decisions:?}"
    );

    let mut only = MarketSnapshot::empty(d("10000"));
    only.tickers = vec![Ticker::new("DOTUSDT", d("270.50"), d("0.446"), d("40000000"))];
    only.account = account();
    only.chart_symbol = "DOTUSDT".into();
    only.account_ok = true;
    attach_pullback(&mut only, &[("DOTUSDT", 270.50)]);
    let (_, weak_only) = tick_decisions(&EngineState::new(4), &only, dead_ts(), Some(&mom), None, None);
    assert!(
        !weak_only.iter().any(is_enter),
        "weak 24h DOT entered without AVAX in the book: {weak_only:?}"
    );
}

#[test]
fn strategy4_skips_24h_dump_with_5m_pullback() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("PROMUSDT", d("10"), d("-4.558"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "PROMUSDT".into();
    snap.account_ok = true;
    attach_pullback(&mut snap, &[("PROMUSDT", 10.0)]);
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "24h dump entered as pullback: {decisions:?}");
    assert!(
        !decisions.iter().any(|d| d.reason().contains("откат ликвид")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_second_slot_waits_until_first_is_green() {
    let mut snap = strategy4_ready_snap();
    snap.live_book = true;
    snap.tickers = vec![
        Ticker::new("AVAXUSDT", d("100"), d("2.0"), d("50000000")),
        Ticker::new("LINKUSDT", d("100"), d("1.8"), d("40000000")),
    ];
    attach_pullback(&mut snap, &[("AVAXUSDT", 100.0), ("LINKUSDT", 100.0)]);
    let mut avax = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103")));
    avax.unrealized_pnl = Decimal::ZERO;
    avax.opened_bar_time = Some(london_ms());
    snap.open_positions = vec![avax.clone()];
    snap.position = Some(avax.clone());
    let mut state = EngineState::new(4);
    state.positions = vec![avax.clone()];
    state.position = Some(avax);
    let mom = MomentumParams {
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        max_positions: 3,
        s4_max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, flat) = tick_decisions(&state, &snap, london_ts(), Some(&mom), None, None);
    assert!(
        !flat.iter().any(is_enter),
        "0-pnl slot must not scale in: {flat:?}"
    );
    state.positions[0].unrealized_pnl = d("0.001");
    snap.open_positions[0].unrealized_pnl = d("0.001");
    let (_, green) = tick_decisions(&state, &snap, london_ts() + 60.0, Some(&mom), None, None);
    assert!(
        green.iter().any(|d| is_enter(d) && d.symbol() == "LINKUSDT"),
        "{green:?}"
    );
}

#[test]
fn strategy4_skips_bounce_in_one_hour_downtrend() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    let seq = downtrend_then_bounce_5m_at(100.0);
    snap.bars = seq.clone();
    snap.last_bars.insert("AVAXUSDT".into(), seq.last().cloned().unwrap());
    snap.universe_bars.insert("AVAXUSDT".into(), seq);
    snap.htf_bars.insert("AVAXUSDT".into(), htf_up_4h_at(100.0));
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
    assert!(
        decisions.iter().any(|d| {
            let r = d.reason();
            r.contains("EMA20") || r.contains("higher low") || r.contains("часовой")
        }),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_moves_stop_to_breakeven_at_one_r() {
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    match decisions.iter().find(|d| is_reduce(d)) {
        Some(Decision::ReduceLong { qty, stop_loss, reason, .. }) => {
            assert_eq!(*qty, d("0.005"));
            assert!(*stop_loss >= d("100"), "BE {stop_loss}");
            assert!(*stop_loss < d("101.5"));
            assert!(reason.contains("частичная фиксация") && reason.contains("1R"), "{reason}");
        }
        other => panic!("{other:?} {decisions:?}"),
    }
}

#[test]
fn strategy4_locks_be_from_unrealized_pnl_even_if_last_is_shy() {
    let mut pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    pos.unrealized_pnl = d("0.015");
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.2"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    match decisions.iter().find(|d| is_reduce(d)) {
        Some(Decision::ReduceLong { reason, stop_loss, .. }) => {
            assert!(reason.contains("частичная фиксация"), "{reason}");
            assert!(*stop_loss >= d("100"), "BE {stop_loss}");
        }
        other => panic!("{other:?} {decisions:?}"),
    }
}

#[test]
fn strategy4_locks_be_if_post_entry_bar_high_hit_one_r() {
    let mut pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let now = london_ts();
    let opened_ms = (now * 1000.0) as i64;
    pos.opened_bar_time = Some(opened_ms);
    let peak = Bar {
        open_time: opened_ms + 300_000,
        open: d("101"),
        high: d("101.6"),
        low: d("100.8"),
        close: d("101.0"),
        volume: d("10"),
    };
    let last = Bar {
        open_time: opened_ms + 600_000,
        open: d("101"),
        high: d("101.1"),
        low: d("100.9"),
        close: d("101.0"),
        volume: d("10"),
    };
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.0"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.bars = vec![peak.clone(), last.clone()];
    snap.universe_bars.insert("AVAXUSDT".into(), vec![peak, last]);
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    match decisions.iter().find(|d| is_reduce(d)) {
        Some(Decision::ReduceLong { reason, .. }) => {
            assert!(reason.contains("частичная фиксация"), "{reason}");
        }
        other => panic!("{other:?} {decisions:?}"),
    }
}

#[test]
fn strategy4_does_not_move_stop_before_one_r() {
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("100.4"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_amend), "{decisions:?}");
    assert!(!decisions.iter().any(is_reduce), "{decisions:?}");
    assert!(!decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })), "{decisions:?}");
}

#[test]
fn strategy4_missing_universe_bars_does_not_enter() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![s4_liquid_ticker()];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.last_bars = [("AVAXUSDT".into(), pullback_last_at(100.0))].into_iter().collect();
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn strategy4_skips_15m_pullback_in_4h_downtrend() {
    let mut snap = strategy4_ready_snap();
    snap.htf_bars
        .insert("AVAXUSDT".into(), htf_down_4h_at(100.0));
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
    assert!(
        decisions.iter().any(|d| d.reason().contains("4ч")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_missing_4h_bars_does_not_enter() {
    let mut snap = strategy4_ready_snap();
    snap.htf_bars.clear();
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
}

#[test]
fn strategy4_trails_on_5m_low_after_breakeven() {
    let be = d("100.08");
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(be), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("102"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    snap.last_bars = [(
        "AVAXUSDT".into(),
        Bar {
            open_time: 1_700_000_000_000,
            open: d("101.8"),
            high: d("102.2"),
            low: d("101.2"),
            close: d("102.0"),
            volume: d("20"),
        },
    )]
    .into_iter()
    .collect();
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    match decisions.iter().find(|d| is_amend(d)) {
        Some(Decision::AmendStop { stop_loss, reason, .. }) => {
            assert_eq!(*stop_loss, d("101.2"));
            assert!(reason.contains("5м"), "{reason}");
        }
        other => panic!("{other:?} {decisions:?}"),
    }
}

#[test]
fn strategy4_exits_when_4h_closes_below_ema20() {
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("100.4"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    snap.htf_bars.insert("AVAXUSDT".into(), htf_down_4h_at(100.0));
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })
            && d.symbol() == "AVAXUSDT"
            && d.reason() == "4ч сломал тренд"),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_holds_through_missing_4h_while_above_stop() {
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("100.4"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    snap.htf_bars.clear();
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        !decisions.iter().any(|d| matches!(d, Decision::ExitPosition { .. })),
        "missing 4h must not dump a long still above SL: {decisions:?}"
    );
}

#[test]
fn retry_until_blocks_new_enter() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = tickers();
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    let mom = MomentumParams {
        always_enter: true,
        ..MomentumParams::default()
    };
    let now = london_ts();
    let mut blocked = EngineState::new(1);
    blocked.retry_until = now + 20.0;
    let (_, decisions) = tick_decisions(&blocked, &snap, now, Some(&mom), None, None);
    assert!(decisions.iter().all(|d| !is_enter(d)), "{decisions:?}");
    assert!(
        decisions[0].reason().contains("сеть"),
        "{}",
        decisions[0].reason()
    );
}

fn vvv_s4_pos() -> Position {
    Position::long(
        "VVVUSDT",
        d("22.60"),
        d("17.055"),
        Some(d("16.7139")),
        Some(d("17.751")),
    )
}

fn vvv_s4_snap(mark: Decimal, pos: Position, bars: Vec<Bar>) -> (EngineState, MarketSnapshot) {
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("VVVUSDT", mark, d("1.633"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "VVVUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.bars = bars.clone();
    snap.universe_bars.insert("VVVUSDT".into(), bars);
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    (state, snap)
}

fn assert_s4_1r_lock_not_wait(decisions: &[Decision]) {
    assert!(
        !decisions.iter().any(|d| d.reason().contains("жду 1R")),
        "must not hold жду 1R after 1R: {decisions:?}"
    );
    let locked = decisions.iter().any(|d| match d {
        Decision::ReduceLong { reason, .. } => reason.contains("частичная фиксация") || reason.contains("1R"),
        Decision::AmendStop { reason, .. } => reason.contains("безубыток"),
        Decision::ExitPosition { reason, .. } => reason.contains("1R был") || reason.contains("фиксирую"),
        _ => false,
    });
    assert!(locked, "expected ReduceLong/AmendStop BE or Exit after 1R: {decisions:?}");
}

#[test]
fn strategy4_vvv_peak_upnl_locks_1r() {
    let mut pos = vvv_s4_pos();
    pos.unrealized_pnl = d("8");
    pos.opened_bar_time = None;
    let (state, snap) = vvv_s4_snap(d("17.40"), pos, vec![]);
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    match decisions.iter().find(|d| is_reduce(d)) {
        Some(Decision::ReduceLong { qty, stop_loss, reason, .. }) => {
            assert_eq!(*qty, d("11.30"));
            assert!(*stop_loss >= d("17.055"), "BE {stop_loss}");
            assert!(reason.contains("частичная фиксация"), "{reason}");
        }
        other => panic!("expected ReduceLong, got {other:?} {decisions:?}"),
    }
    assert_s4_1r_lock_not_wait(&decisions);
}

#[test]
fn strategy4_vvv_bar_high_without_opened_bar_time_locks_1r() {
    let mut pos = vvv_s4_pos();
    pos.opened_bar_time = None;
    let peak = Bar {
        open_time: 1_788_191_230_000,
        open: d("17.20"),
        high: d("17.3961"),
        low: d("17.10"),
        close: d("17.22"),
        volume: d("10"),
    };
    let last = Bar {
        open_time: 1_788_191_530_000,
        open: d("17.22"),
        high: d("17.25"),
        low: d("17.15"),
        close: d("17.20"),
        volume: d("10"),
    };
    let (state, snap) = vvv_s4_snap(d("17.20"), pos, vec![peak, last]);
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert_s4_1r_lock_not_wait(&decisions);
}

#[test]
fn strategy4_vvv_dump_after_1r_exits_instead_of_waiting() {
    let mut pos = vvv_s4_pos();
    pos.opened_bar_time = None;
    pos.unrealized_pnl = d("8");
    // Mark back below BE so AmendStop would be invalid vs mark.
    let peak = Bar {
        open_time: 1_788_191_230_000,
        open: d("17.20"),
        high: d("17.3961"),
        low: d("17.10"),
        close: d("17.22"),
        volume: d("10"),
    };
    let (state, snap) = vvv_s4_snap(d("16.74"), pos, vec![peak]);
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        !decisions.iter().any(|d| d.reason().contains("жду 1R")),
        "{decisions:?}"
    );
    assert!(
        decisions.iter().any(|d| matches!(
            d,
            Decision::ExitPosition { reason, .. }
                if reason.contains("1R был") || reason.contains("фиксирую")
        )),
        "{decisions:?}"
    );
}

#[test]
fn coalesce_overlay_does_not_lower_live_stop() {
    let live = Position::long("VVVUSDT", d("22.60"), d("17.055"), Some(d("17.068")), Some(d("17.751")));
    let journal = Position::long("VVVUSDT", d("22.60"), d("17.055"), Some(d("16.7139")), Some(d("17.751")));
    let out = coalesce_position(Some(&live), Some(&journal)).unwrap();
    assert_eq!(out.stop_loss, Some(d("17.068")));
}




#[test]
fn strategy4_book_uses_liquid_n_not_max_plus_four() {
    use tui_bot::continuation::{liquid_universe, pick_strategy4_book, ContinuationParams};
    let mut tickers = Vec::new();
    for i in 0..40 {
        let sym = format!("T{i:02}USDT");
        let vol = d(&(50000000 - i * 100000).to_string());
        // Mild positive 24h — inside dump/stretch gates.
        tickers.push(Ticker::new(&sym, d("10"), d("1.2"), vol));
    }
    // A dump stays out. A green +8% off the high is a pullback candidate (not a chase).
    tickers.push(Ticker::new("DUMPUSDT", d("10"), d("-3.0"), d("60000000")));
    let mut pump = Ticker::new("PUMPUSDT", d("10"), d("8.0"), d("60000000"));
    pump.high_price = d("10.01");
    tickers.push(pump);
    let mut pulled = Ticker::new("PULLUSDT", d("9.5"), d("8.0"), d("60000000"));
    pulled.high_price = d("10.5");
    tickers.push(pulled);
    let p = ContinuationParams {
        max_positions: 3,
        liquid_n: 20,
        ..ContinuationParams::default()
    };
    let uni = liquid_universe(&tickers, &[], &p);
    assert_eq!(uni.len(), 20, "liquid_n caps universe");
    let book = pick_strategy4_book(&tickers, p.liquid_n, &[], Some(&p));
    assert!(book.len() > 7, "entry book should exceed old max_positions+4; got {}", book.len());
    assert!(book.len() <= 20);
    assert!(!book.iter().any(|t| t.symbol == "DUMPUSDT"));
    assert!(
        !book.iter().any(|t| t.symbol == "PUMPUSDT"),
        "name sitting on 24h high is a chase: {book:?}"
    );
    assert!(
        book.iter().any(|t| t.symbol == "PULLUSDT"),
        "green +8% off the high belongs in the book: {book:?}"
    );
}

#[test]
fn strategy4_htf_skips_flat_swings_even_if_close_above_ema20() {
    // Declining 4h swing lows block entry even when close > EMA20.
    let mut snap = strategy4_ready_snap();
    let mut htf = htf_up_4h_at(100.0);
    let n = htf.len();
    let i1 = n - 10;
    let i2 = n - 4;
    for &(i, lo) in &[(i1, d("96")), (i2, d("94"))] {
        htf[i].low = lo;
        htf[i - 1].low = lo + d("2");
        htf[i + 1].low = lo + d("2");
        htf[i].high = htf[i].high.max(lo + d("4"));
    }
    if let Some(last) = htf.last_mut() {
        last.close = d("100");
        last.open = d("99.7");
        last.high = d("100.2");
        last.low = d("98");
    }
    snap.htf_bars.insert("AVAXUSDT".into(), htf);
    let mom = MomentumParams {
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        s4_max_positions: 3,
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, dead_ts(), Some(&mom), None, None);
    assert!(!decisions.iter().any(is_enter), "{decisions:?}");
    assert!(
        decisions.iter().any(|d| d.reason().contains("4ч нет higher low")),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_slots_ignore_strategy1_max_positions() {
    // S1 basket size must not cap S4 concurrent slots / entry book.
    let snap = strategy4_ready_snap();
    let mom = MomentumParams {
        s4_always_enter: true,
        s4_entry_windows: Vec::new(),
        s4_max_positions: 5,
        max_positions: 1, // S1-only; must not shrink S4 to 1
        ..MomentumParams::default()
    };
    let (_, decisions) = tick_decisions(&EngineState::new(4), &snap, dead_ts(), Some(&mom), None, None);
    assert!(decisions.iter().any(is_enter), "{decisions:?}");
    assert_eq!(
        tui_bot::continuation::ContinuationParams::default().max_positions,
        5
    );
}

#[test]
fn strategy4_default_s4_max_positions_is_five() {
    assert_eq!(tui_bot::config::DEFAULT_S4_MAX_POSITIONS, 5);
    assert_eq!(MomentumParams::default().s4_max_positions, 5);
    assert_eq!(tui_bot::continuation::ContinuationParams::default().max_positions, 5);
}

#[test]
fn skip_no_htf_trend_requires_4h_higher_low_when_swings_exist() {
    let mut snap = MarketSnapshot::empty(d("10000"));
    let mut htf = htf_up_4h_at(100.0);
    let n = htf.len();
    let i1 = n - 10;
    let i2 = n - 4;
    for &(i, lo) in &[(i1, d("96")), (i2, d("94"))] {
        htf[i].low = lo;
        htf[i - 1].low = lo + d("2");
        htf[i + 1].low = lo + d("2");
        htf[i].high = htf[i].high.max(lo + d("4"));
    }
    if let Some(last) = htf.last_mut() {
        last.close = d("100");
        last.open = d("99.7");
        last.high = d("100.2");
        last.low = d("98");
    }
    snap.htf_bars.insert("AVAXUSDT".into(), htf.clone());
    let flat = tui_bot::continuation::skip_no_htf_trend(&snap, "AVAXUSDT");
    assert!(
        flat.as_deref().unwrap_or("").contains("4ч нет higher low"),
        "declining 4h swings must skip: {flat:?}"
    );
    snap.htf_bars.insert("AVAXUSDT".into(), htf_up_4h_at(100.0));
    assert!(
        tui_bot::continuation::skip_no_htf_trend(&snap, "AVAXUSDT").is_none(),
        "rising 4h swings above EMA20 must pass"
    );
    snap.htf_bars
        .insert("AVAXUSDT".into(), htf_down_4h_at(100.0));
    let reason = tui_bot::continuation::skip_no_htf_trend(&snap, "AVAXUSDT");
    assert!(
        reason.as_deref().unwrap_or("").contains("EMA20")
            || reason.as_deref().unwrap_or("").contains("4ч"),
        "{reason:?}"
    );
}




#[test]
fn strategy4_banks_at_1_5r_after_be() {
    let be = d("100.08");
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(be), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("102.40"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(
            d, Decision::ExitPosition { reason, .. } if reason.contains("1.5R")
        )),
        "expected 1.5R bank after BE, got {decisions:?}"
    );
    assert!(!decisions.iter().any(is_amend), "{decisions:?}");
}

#[test]
fn strategy4_banks_at_1_5r_while_still_pre_be() {
    // Scale-first: even at ≥1.5R on first touch, ReduceLong half + BE; remainder banks next tick.
    let mut pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    pos.opened_bar_time = Some(london_ms());
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("102.40"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    match decisions.iter().find(|d| is_reduce(d)) {
        Some(Decision::ReduceLong { qty, reason, .. }) => {
            assert_eq!(*qty, d("0.005"));
            assert!(reason.contains("частичная фиксация") && reason.contains("1R"), "{reason}");
        }
        other => panic!("expected scale-out at first 1.5R touch, got {other:?} {decisions:?}"),
    }
    // After latch, same mark banks the remainder.
    state.scaled_one_r.insert("AVAXUSDT".into());
    // Simulate remainder qty after reduce (engine latch alone; qty still full in this unit test).
    let (_, again) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        again.iter().any(|d| matches!(
            d, Decision::ExitPosition { reason, .. } if reason.contains("1.5R")
        )),
        "latched remainder at 1.5R must Exit, got {again:?}"
    );
}

#[test]
fn strategy4_post_be_1_5r_locks_half_r_after_giveback() {
    // Peak printed 1.5R; mark gave back but still >0.5R → AmendStop «замок 0.5R».
    let be = d("100.08");
    let mut pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(be), Some(d("103.1")));
    let opened_ms = london_ms();
    pos.opened_bar_time = Some(opened_ms);
    let peak = Bar {
        open_time: opened_ms + 300_000,
        open: d("102.0"),
        high: d("102.40"),
        low: d("101.5"),
        close: d("102.1"),
        volume: d("10"),
    };
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.0"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.bars = vec![peak.clone()];
    snap.universe_bars.insert("AVAXUSDT".into(), vec![peak]);
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    match decisions.iter().find(|d| is_amend(d)) {
        Some(Decision::AmendStop { stop_loss, reason, .. }) => {
            assert!(reason.contains("замок 0.5R"), "{reason}");
            assert!(*stop_loss > be && *stop_loss < d("101.0"));
            assert!(*stop_loss > d("100.5") && *stop_loss < d("101.0"), "{stop_loss}");
        }
        other => panic!("expected 0.5R lock, got {other:?} {decisions:?}"),
    }
}

#[test]
fn strategy4_post_be_1_5r_exits_when_mark_below_lock() {
    let be = d("100.08");
    let mut pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(be), Some(d("103.1")));
    let now = london_ts();
    let opened_ms = (now * 1000.0) as i64;
    pos.opened_bar_time = Some(opened_ms);
    let peak = Bar {
        open_time: opened_ms + 300_000,
        open: d("102.0"),
        high: d("102.40"),
        low: d("101.5"),
        close: d("102.1"),
        volume: d("10"),
    };
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("100.40"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.bars = vec![peak.clone()];
    snap.universe_bars.insert("AVAXUSDT".into(), vec![peak]);
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, now, None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(
            d, Decision::ExitPosition { reason, .. } if reason.contains("1.5R")
        )),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_pre_1r_peak_pullback_exits() {
    let mut pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let now = london_ts();
    let opened_ms = (now * 1000.0) as i64;
    pos.opened_bar_time = Some(opened_ms);
    let peak = Bar {
        open_time: opened_ms + 300_000,
        open: d("100.8"),
        high: d("101.30"),
        low: d("100.5"),
        close: d("101.0"),
        volume: d("10"),
    };
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("100.20"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.bars = vec![peak.clone()];
    snap.universe_bars.insert("AVAXUSDT".into(), vec![peak]);
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, now, None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(
            d, Decision::ExitPosition { reason, .. } if reason.contains("откат с пика")
        )),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_time_stop_exits_after_four_hours() {
    let mut pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let now = london_ts();
    pos.opened_bar_time = Some(((now - 14_401.0) * 1000.0) as i64);
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("100.4"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, now, None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(
            d, Decision::ExitPosition { reason, .. } if reason.contains("тайм-стоп")
        )),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_session_end_exits_open_long_outside_window() {
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("100.4"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, dead_ts(), None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(
            d, Decision::ExitPosition { reason, .. }
                if reason.contains("конец окна") || reason.contains("конец сессии")
        )),
        "{decisions:?}"
    );
}

#[test]
fn strategy4_excludes_majors_from_liquid_universe() {
    use tui_bot::continuation::{liquid_universe, ContinuationParams};
    use tui_bot::ranking::is_major_symbol;
    assert!(is_major_symbol("BTCUSDT"));
    assert!(is_major_symbol("sol"));
    assert!(!is_major_symbol("AVAXUSDT"));
    let mut tickers = vec![
        Ticker::new("BTCUSDT", d("50000"), d("2"), d("90000000")),
        Ticker::new("ETHUSDT", d("3000"), d("2"), d("80000000")),
        Ticker::new("BNBUSDT", d("600"), d("2"), d("70000000")),
        Ticker::new("XRPUSDT", d("0.6"), d("2"), d("60000000")),
        Ticker::new("SOLUSDT", d("140"), d("2"), d("50000000")),
        Ticker::new("BCHUSDT", d("400"), d("2"), d("45000000")),
    ];
    for i in 0..20 {
        tickers.push(Ticker::new(
            &format!("ALT{i}USDT"),
            d("10"),
            d("1.2"),
            d(&(40000000 - i * 100000).to_string()),
        ));
    }
    let p = ContinuationParams::default();
    let uni = liquid_universe(&tickers, &[], &p);
    assert_eq!(uni.len(), 20);
    for maj in ["BTCUSDT", "ETHUSDT", "BNBUSDT", "XRPUSDT", "SOLUSDT", "BCHUSDT"] {
        assert!(
            !uni.iter().any(|t| t.symbol == maj),
            "{maj} must be excluded from S4 liquid_universe"
        );
    }
}



#[test]
fn strategy4_mark_trail_raises_sl_tighter_than_bar_low() {
    let be = d("100.08");
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(be), Some(d("112.0")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("104"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    snap.last_bars = [(
        "AVAXUSDT".into(),
        Bar {
            open_time: london_ms(),
            open: d("103.5"),
            high: d("104.2"),
            low: d("101.0"),
            close: d("104.0"),
            volume: d("20"),
        },
    )]
    .into_iter()
    .collect();
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    match decisions.iter().find(|d| matches!(d, Decision::AmendStop { .. })) {
        Some(Decision::AmendStop { stop_loss, reason, .. }) => {
            assert!(*stop_loss > d("101.0"), "mark trail must beat bar low: {stop_loss}");
            assert!(reason.contains("trail mark"), "{reason}");
        }
        other => panic!("expected mark trail AmendStop, got {other:?} {decisions:?}"),
    }
}

#[test]
fn strategy4_reduce_at_1r_latches_once_after_be() {
    // After ReduceLong+BE applied (sl >= entry), post-BE path must not re-emit ReduceLong.
    let be = d("100.08");
    let pos = Position::long("AVAXUSDT", d("0.005"), d("100"), Some(be), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        !decisions.iter().any(is_reduce),
        "post-BE must not ReduceLong again: {decisions:?}"
    );
}


#[test]
fn strategy4_scaled_latch_blocks_second_reduce() {
    // sl still below entry (BE not on book yet) but latch set → no second ReduceLong.
    let pos = Position::long(
        "AVAXUSDT",
        d("0.01"),
        d("100"),
        Some(d("98.5")),
        Some(d("103.1")),
    );
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    state.scaled_one_r.insert("AVAXUSDT".into());
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_reduce), "latched must not Reduce again: {decisions:?}");
    let be_or_exit = decisions.iter().any(|d| match d {
        Decision::AmendStop { reason, .. } => reason.contains("безубыток"),
        Decision::ExitPosition { reason, .. } => {
            reason.contains("1R был") || reason.contains("1.5R") || reason.contains("фиксирую")
        }
        _ => false,
    });
    assert!(be_or_exit, "expected BE amend or exit after latch: {decisions:?}");
}

#[test]
fn strategy4_post_scale_15r_exits_remainder() {
    // After scale (BE stop on remainder), mark at 1.5R → Exit remainder.
    let entry = d("100");
    let risk = d("1.5"); // original risk was 1.5 → 1.5R target = 102.25 via position_risk from TP
    let be = entry + d("0.08");
    let mut pos = Position::long(
        "AVAXUSDT",
        d("0.01"),
        entry,
        Some(be),
        Some(entry + Decimal::from(2) * risk), // TP 2R ⇒ risk_from_tp = 1.5
    );
    pos.opened_bar_time = None;
    let mark = entry + Decimal::new(15, 1) * risk; // 102.25
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", mark, d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    state.scaled_one_r.insert("AVAXUSDT".into());
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(
        decisions.iter().any(|d| matches!(d, Decision::ExitPosition { reason, .. } if reason.contains("1.5R"))),
        "post-scale 1.5R must exit remainder: {decisions:?}"
    );
    assert!(!decisions.iter().any(is_reduce), "{decisions:?}");
}


#[test]
fn strategy4_scaled_one_r_latches_reduce() {
    // Explicit latch: even with SL still below entry, scaled_one_r skips ReduceLong
    // and falls through to BE AmendStop.
    let pos = Position::long("AVAXUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let mut snap = MarketSnapshot::empty(d("10000"));
    snap.tickers = vec![Ticker::new("AVAXUSDT", d("101.5"), d("2.0"), d("50000000"))];
    snap.account = account();
    snap.chart_symbol = "AVAXUSDT".into();
    snap.account_ok = true;
    snap.live_book = true;
    snap.open_positions = vec![pos.clone()];
    snap.position = Some(pos.clone());
    let mut state = EngineState::new(4);
    state.position = Some(pos.clone());
    state.positions = vec![pos];
    state.scaled_one_r.insert("AVAXUSDT".into());
    let (_, decisions) = tick_decisions(&state, &snap, london_ts(), None, None, None);
    assert!(!decisions.iter().any(is_reduce), "{decisions:?}");
    match decisions.iter().find(|d| is_amend(d)) {
        Some(Decision::AmendStop { reason, .. }) => {
            assert!(reason.contains("безубыток"), "{reason}");
        }
        other => panic!("expected BE amend after scaled latch, got {other:?} {decisions:?}"),
    }
}

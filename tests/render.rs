//! Drive shipped render_frame / render_startup_frame.

use rust_decimal::Decimal;
use tui_bot::app::render_startup_frame;
use tui_bot::engine::STRATEGY_NAMES;
use tui_bot::models::{Position, RecentAction, Side, Ticker};
use tui_bot::profit::account_profit;
use tui_bot::render::{
    account_profit_figure, fit_lines, line_tone, one_r_status, render_frame, sparkline, top_movers,
    LineTone, OneRStatus, ViewModel, TOP_MOVERS_N,
};
use tui_bot::sessions::make_utc_ts;

#[test]
fn startup_frame_has_required_surfaces() {
    let frame = render_startup_frame(None, None, 1, false, true, Some(&Default::default())).unwrap();
    assert!(frame.contains("Прибыль счета"));
    assert!(frame.contains("Сумма счета"));
    assert!(frame.contains("Позиции / сделки"));
    assert!(frame.contains("Аналитика / график"));
    assert!(frame.to_lowercase().contains("выбор"));
    for (_, title) in STRATEGY_NAMES {
        assert!(frame.contains(title), "missing {title}");
    }
    assert!(frame.contains("60 с"));
    assert!(frame.contains("x закрыть все"));
    assert!(frame.contains("0.0000"));
    assert!(!frame.trim().is_empty());
}

#[test]
fn profit_line_uses_shipped_account_profit() {
    let view = ViewModel {
        strategy_id: 3,
        wallet_balance: Decimal::from(11000),
        unrealized_pnl: Decimal::from(200),
        starting_equity: Decimal::from(10000),
        available_balance: Decimal::from(9000),
        positions: vec![Position {
            symbol: "BTCUSDT".into(),
            side: Side::Long,
            qty: "0.01".parse().unwrap(),
            entry_price: Decimal::from(50000),
            stop_loss: Some(Decimal::from(49000)),
            take_profit: Some(Decimal::from(52000)),
            unrealized_pnl: Decimal::from(200),
            opened_bar_time: None,
            leverage: 0,
        }],
        recent_actions: vec![RecentAction::new(
            make_utc_ts(2026, 9, 1, 14, 32, 5),
            "BUY BTCUSDT",
        )],
        tickers: vec![
            Ticker::new("BTCUSDT", Decimal::from(51000), "2.5".parse().unwrap(), Decimal::from(10)),
            Ticker::new("ETHUSDT", Decimal::from(3000), "-1.2".parse().unwrap(), Decimal::from(8)),
        ],
        chart_symbol: "BTCUSDT".into(),
        chart_closes: vec![Decimal::ONE, Decimal::from(2), Decimal::from(3), Decimal::from(2)],
        last_decision: "hold".into(),
        poll_seconds: 60,
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    let expected = account_profit(view.wallet_balance, view.unrealized_pnl, view.starting_equity);
    assert_eq!(account_profit_figure(&view), expected);
    assert!(frame.contains(&format!("{:.4}", expected)));
    assert!(frame.contains("Прибыль счета"));
    assert!(frame.contains("Сумма счета"));
    assert!(frame.contains("11200.0000"));
    assert!(frame.contains("BTCUSDT"));
    assert!(frame.contains("14:32:05 UTC  BUY BTCUSDT"), "{frame}");
    assert!(frame.contains("до 1R: пройден"), "{frame}");
    assert!(frame.contains(STRATEGY_NAMES[2].1));
    assert!(frame.contains('*'));
    assert!(frame.contains(&sparkline(&view.chart_closes, 48)));
    for (_, title) in STRATEGY_NAMES {
        assert!(frame.contains(title), "missing {title}");
    }
}

#[test]
fn signals_note_only_when_enabled() {
    let quiet = render_frame(&ViewModel {
        strategy_id: 1,
        wallet_balance: Decimal::ONE,
        unrealized_pnl: Decimal::ZERO,
        starting_equity: Decimal::ONE,
        available_balance: Decimal::ONE,
        ..ViewModel::default()
    });
    assert!(!quiet.contains("Звуки:"), "{quiet}");
    let loud = render_frame(&ViewModel {
        strategy_id: 1,
        wallet_balance: Decimal::ONE,
        unrealized_pnl: Decimal::ZERO,
        starting_equity: Decimal::ONE,
        available_balance: Decimal::ONE,
        signals_on: true,
        ..ViewModel::default()
    });
    assert!(loud.contains("покупка"), "{loud}");
    assert!(loud.contains("плюс"), "{loud}");
    assert!(loud.contains("минус"), "{loud}");
    assert!(loud.contains("два высоких"), "{loud}");
    assert!(loud.contains("три вверх"), "{loud}");
    assert!(loud.contains("три вниз"), "{loud}");
}

#[test]
fn pause_after_trade_is_a_vertical_list_once() {
    use std::collections::HashMap;
    let mut cooldowns = HashMap::new();
    let now = 1_700_000_000.0;
    cooldowns.insert("BTCUSDT".into(), now + 60.0 * 989.5);
    cooldowns.insert("EULUSDT".into(), now + 60.0 * 1333.8);
    cooldowns.insert("XRPUSDT".into(), now + 60.0 * 1384.9);
    let view = ViewModel {
        now_ts: Some(now),
        cooldowns,
        cooldown_until: now + 1800.0,
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    let heading = frame.matches("Пауза после сделки").count();
    assert_eq!(heading, 1, "pause must appear once:\n{frame}");
    assert!(frame.contains("  • BTCUSDT"), "{frame}");
    assert!(frame.contains("  • EULUSDT"), "{frame}");
    assert!(frame.contains("  • XRPUSDT"), "{frame}");
    assert!(!frame.contains("Пауза после сделки: BTCUSDT"), "{frame}");
    let pos = frame.find("=== Позиции / сделки ===").expect("pos");
    let foot = frame.find("Стратегия (выбор").expect("footer");
    let pause = frame.find("Пауза после сделки:").expect("pause");
    assert!(pause > pos && pause < foot, "pause belongs in the positions block:\n{frame}");
}

#[test]
fn strategy4_session_line_shows_recommended_windows() {
    use tui_bot::sessions::DEFAULT_ENTRY_WINDOWS;
    let view = ViewModel {
        strategy_id: 4,
        always_enter: false,
        entry_windows: DEFAULT_ENTRY_WINDOWS.to_vec(),
        now_ts: Some(1_700_000_000.0),
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    assert!(frame.contains("Continuation"), "{frame}");
    assert!(frame.contains("00–02"), "{frame}");
    assert!(frame.contains("07–10"), "{frame}");
    assert!(frame.contains("13–16"), "{frame}");
    assert!(!frame.contains("круглосуточно"), "{frame}");
    assert!(frame.contains("свечи 5м"), "{frame}");
}

#[test]
fn strategy4_session_line_shows_15m_interval() {
    use tui_bot::config::TradeInterval;
    use tui_bot::sessions::DEFAULT_ENTRY_WINDOWS;
    let view = ViewModel {
        strategy_id: 4,
        always_enter: false,
        entry_windows: DEFAULT_ENTRY_WINDOWS.to_vec(),
        now_ts: Some(1_700_000_000.0),
        s4_interval: TradeInterval::Minute15,
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    assert!(frame.contains("свечи 15м"), "{frame}");
    assert!(frame.contains("SL 2–5%"), "{frame}");
    assert!(frame.contains("TP 2R"), "{frame}");
    assert!(!frame.contains("свечи 5м"), "{frame}");
}

#[test]
fn strategy4_startup_ignores_strategy1_always_enter() {
    let mut env = std::collections::HashMap::new();
    env.insert("STRATEGY1_ALWAYS_ENTER".into(), "1".into());
    let s4 = render_startup_frame(None, None, 4, false, true, Some(&env)).unwrap();
    assert!(s4.contains("Continuation"), "{s4}");
    assert!(s4.contains("00–02"), "{s4}");
    assert!(!s4.contains("круглосуточно"), "{s4}");
    let s1 = render_startup_frame(None, None, 1, false, true, Some(&env)).unwrap();
    assert!(s1.contains("круглосуточно"), "{s1}");
}

#[test]
fn book_and_session_follow_always_enter_notional_leverage() {
    let view = ViewModel {
        strategy_id: 1,
        always_enter: true,
        order_notional: Decimal::from(40),
        leverage: Some(5),
        now_ts: Some(1_700_000_000.0),
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    assert!(frame.contains("плечо 5x"), "{frame}");
    assert!(frame.contains("сумма 40 USDT"), "{frame}");
    assert!(frame.contains("круглосуточно"), "{frame}");
    assert!(!frame.contains("вне часов старта"), "{frame}");
}

#[test]
fn profit_and_loss_lines_get_green_or_red_tone() {
    assert_eq!(
        line_tone("Прибыль счета:       12.0000 USDT", Decimal::from(12)),
        Some(LineTone::Profit)
    );
    assert_eq!(
        line_tone("Прибыль счета:       -3.5000 USDT", "-3.5".parse().unwrap()),
        Some(LineTone::Loss)
    );
    assert_eq!(
        line_tone("Прибыль счета:       0.0000 USDT", Decimal::ZERO),
        Some(LineTone::Profit)
    );
    assert_eq!(
        line_tone("Нереализованный PnL: -1.2500 USDT", Decimal::ZERO),
        Some(LineTone::Loss)
    );
    assert_eq!(
        line_tone("Нереализованный PnL: 2.0000 USDT", Decimal::ZERO),
        Some(LineTone::Profit)
    );
    assert_eq!(
        line_tone(
            "BTCUSDT LONG qty=0.0100 entry=50000 SL=49000 TP=52000 uPnL=-8.0000  [ведём]",
            Decimal::ZERO
        ),
        Some(LineTone::Loss)
    );
    assert_eq!(
        line_tone("  BTCUSDT      +9.500%  last=50000", Decimal::ZERO),
        Some(LineTone::Profit)
    );
    assert_eq!(
        line_tone("  TACUSDT      -34.222%  last=0.001653", Decimal::ZERO),
        Some(LineTone::Loss)
    );
    assert_eq!(line_tone("=== Счёт ===", Decimal::from(10)), None);
    assert_eq!(line_tone("Баланс кошелька:     3102.8974 USDT", Decimal::from(10)), None);
    assert_eq!(
        line_tone("  до 1R: ещё 0.0110 USDT (осталось 73.3%)", Decimal::ZERO),
        Some(LineTone::Loss)
    );
    assert_eq!(
        line_tone("  до 1R: ещё 0.0300 USDT (осталось 20%)", Decimal::ZERO),
        Some(LineTone::Warn)
    );
    assert_eq!(
        line_tone("  до 1R: пройден", Decimal::ZERO),
        Some(LineTone::Profit)
    );
    assert_eq!(
        line_tone("  до 1R: нет стопа", Decimal::ZERO),
        Some(LineTone::Warn)
    );
}

#[test]
fn fit_lines_starts_each_logical_line_at_column_zero() {
    let frame = "header\nНереализованный PnL: 0.0000 USDT\nСумма счета: 3102.8974 USDT\n";
    let rows = fit_lines(frame, 16, 20);
    assert_eq!(rows[0], "header");
    assert!(rows[1].starts_with("Нереализованный"));
    assert!(rows.iter().any(|r| r.starts_with("Сумма счета")));
    assert!(rows.iter().all(|r| r.chars().count() <= 16));
}

#[test]
fn top_growth_is_a_live_slice_of_the_full_tape() {
    assert_eq!(TOP_MOVERS_N, 5);
    let tickers = vec![
        Ticker::new("SKRUSDT", Decimal::new(21907, 6), "82.194".parse().unwrap(), Decimal::from(1)),
        Ticker::new("XYZUSDT", Decimal::from(125), "56.238".parse().unwrap(), Decimal::from(2)),
        Ticker::new("HEMIUSDT", Decimal::new(14090, 6), "23.272".parse().unwrap(), Decimal::from(1)),
        Ticker::new("ANIMEUSDT", Decimal::new(3104, 6), "18.113".parse().unwrap(), Decimal::from(1)),
        Ticker::new("AUCTIONUSDT", Decimal::new(3648, 3), "15.589".parse().unwrap(), Decimal::from(3)),
        Ticker::new("BTCUSDT", Decimal::from(50000), "2.000".parse().unwrap(), Decimal::from(9)),
        Ticker::new("ETHUSDT", Decimal::from(3000), "-1.200".parse().unwrap(), Decimal::from(8)),
        Ticker::new("SOLUSDT", Decimal::from(140), "-4.000".parse().unwrap(), Decimal::from(7)),
    ];
    let (rising, falling) = top_movers(&tickers, TOP_MOVERS_N);
    assert_eq!(rising.len(), 5);
    assert_eq!(rising[0].symbol, "SKRUSDT");
    assert_eq!(falling[0].symbol, "SOLUSDT");

    let frame = render_frame(&ViewModel {
        tickers: tickers.clone(),
        ..ViewModel::default()
    });
    assert!(frame.contains("Топ роста (5 из 8):"), "{frame}");
    assert!(frame.contains("Топ падения (5 из 8):"), "{frame}");
    let growth: Vec<_> = frame
        .lines()
        .skip_while(|l| !l.starts_with("Топ роста"))
        .skip(1)
        .take(5)
        .collect();
    assert!(growth[0].contains("SKRUSDT"), "{frame}");
    assert!(!growth.iter().any(|l| l.contains("BTCUSDT")), "{frame}");

    let mut later = tickers;
    later[5].price_change_percent = "90.000".parse().unwrap();
    let (rising2, _) = top_movers(&later, TOP_MOVERS_N);
    assert_eq!(rising2[0].symbol, "BTCUSDT");
    let frame2 = render_frame(&ViewModel {
        tickers: later,
        ..ViewModel::default()
    });
    assert!(frame2.contains("BTCUSDT"), "{frame2}");
    let growth: Vec<_> = frame2
        .lines()
        .skip_while(|l| !l.starts_with("Топ роста"))
        .skip(1)
        .take(5)
        .collect();
    assert!(growth[0].contains("BTCUSDT"), "{frame2}");
    assert!(!growth.iter().any(|l| l.contains("SOLUSDT")), "{frame2}");
}


#[test]
fn strategy4_book_shows_risk_pct_not_order_notional() {
    let view = ViewModel {
        strategy_id: 4,
        risk_pct: Decimal::new(25, 4),
        order_notional: Decimal::from(40),
        wallet_balance: Decimal::from(3100),
        unrealized_pnl: Decimal::ZERO,
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    assert!(!frame.contains("сумма 40 USDT"), "{frame}");
    assert!(!frame.contains("сумма 20 USDT"), "{frame}");
    assert!(!frame.contains("ORDER_NOTIONAL"), "{frame}");
    assert!(frame.contains("риск 0.25% счета / стоп"), "{frame}");
    // 3100 * 0.0025 / 0.015 (5m SL floor) = 516.6… via risk_position_notional
    assert!(frame.contains("до 516.7 USDT при SL 1.5%"), "{frame}");
}

#[test]
fn strategy4_book_falls_back_to_notional_when_risk_off() {
    let view = ViewModel {
        strategy_id: 4,
        risk_pct: Decimal::ZERO,
        order_notional: Decimal::from(40),
        wallet_balance: Decimal::from(3100),
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    assert!(frame.contains("сумма 40 USDT"), "{frame}");
    assert!(!frame.contains("риск 0.25%"), "{frame}");
    assert!(!frame.contains("риск 0%"), "{frame}");
}

#[test]
fn strategy4_book_without_equity_still_hides_stale_notional() {
    let view = ViewModel {
        strategy_id: 4,
        risk_pct: Decimal::new(25, 4),
        order_notional: Decimal::from(40),
        wallet_balance: Decimal::ZERO,
        unrealized_pnl: Decimal::ZERO,
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    assert!(!frame.contains("сумма 40 USDT"), "{frame}");
    assert!(frame.contains("риск 0.25% счета / стоп"), "{frame}");
    assert!(!frame.contains("USDT при SL"), "{frame}");
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

#[test]
fn recent_decisions_show_utc_time() {
    let view = ViewModel {
        recent_actions: vec![
            RecentAction::new(make_utc_ts(2026, 9, 1, 7, 4, 9), "BUY ETHUSDT TP=1 SL=1 (pullback)"),
            RecentAction::new(make_utc_ts(2026, 9, 1, 7, 5, 11), "SL ETHUSDT -> 1.0008 (безубыток на 1R)"),
        ],
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    assert!(frame.contains("Последние решения:"), "{frame}");
    assert!(frame.contains("07:04:09 UTC  BUY ETHUSDT"), "{frame}");
    assert!(frame.contains("07:05:11 UTC  SL ETHUSDT"), "{frame}");
}

#[test]
fn position_block_shows_remaining_to_one_r() {
    let far = Position::long("BTCUSDT", d("0.01"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let close = Position::long("ETHUSDT", d("0.1"), d("100"), Some(d("98.5")), Some(d("103.1")));
    let locked = Position::long("SOLUSDT", d("1"), d("100"), Some(d("100.08")), Some(d("103.1")));
    let naked = Position::long("XRPUSDT", d("10"), d("100"), None, Some(d("103.1")));
    let short = Position {
        symbol: "DOGEUSDT".into(),
        side: Side::Short,
        qty: d("100"),
        entry_price: d("0.2"),
        stop_loss: Some(d("0.21")),
        take_profit: Some(d("0.18")),
        unrealized_pnl: Decimal::ZERO,
        opened_bar_time: None,
        leverage: 0,
    };
    match one_r_status(&close, d("101.2")) {
        OneRStatus::Remaining { usdt, pct } => {
            assert_eq!(usdt, d("0.03"));
            assert_eq!(pct.round_dp(1), d("20"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(one_r_status(&locked, d("101.5")), OneRStatus::Reached);
    assert_eq!(one_r_status(&naked, d("100.1")), OneRStatus::NoStop);

    let view = ViewModel {
        positions: vec![far, close, locked, naked, short],
        tickers: vec![
            Ticker::new("BTCUSDT", d("100.4"), d("1"), d("10")),
            Ticker::new("ETHUSDT", d("101.2"), d("1"), d("10")),
            Ticker::new("SOLUSDT", d("101.5"), d("1"), d("10")),
            Ticker::new("XRPUSDT", d("100.1"), d("1"), d("10")),
            Ticker::new("DOGEUSDT", d("0.19"), d("1"), d("10")),
        ],
        ..ViewModel::default()
    };
    let frame = render_frame(&view);
    assert_eq!(frame.matches("до 1R:").count(), 4, "{frame}");
    assert!(
        frame.contains("до 1R: ещё 0.0110 USDT (осталось 73.3%)"),
        "{frame}"
    );
    assert!(
        frame.contains("до 1R: ещё 0.0300 USDT (осталось 20%)"),
        "{frame}"
    );
    assert!(frame.contains("до 1R: пройден"), "{frame}");
    assert!(frame.contains("до 1R: нет стопа"), "{frame}");
    assert!(frame.contains("DOGEUSDT SHORT"), "{frame}");
}

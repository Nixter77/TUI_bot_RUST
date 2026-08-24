//! Drive shipped render_frame / render_startup_frame.

use rust_decimal::Decimal;
use tui_bot::app::render_startup_frame;
use tui_bot::engine::STRATEGY_NAMES;
use tui_bot::models::{Position, Side, Ticker};
use tui_bot::profit::account_profit;
use tui_bot::render::{
    account_profit_figure, fit_lines, line_tone, render_frame, sparkline, LineTone, ViewModel,
};

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
        recent_actions: vec!["BUY BTCUSDT".into()],
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
    assert!(loud.contains("продажа"), "{loud}");
    assert!(loud.contains("два высоких"), "{loud}");
    assert!(loud.contains("два низких"), "{loud}");
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

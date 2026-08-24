//! Drive the real CLI entry (dump-frame) without a TTY.

use tui_bot::app::{
    dump_frame_offline_isolated, dump_frame_offline_strategy, help_text, live_without_keys_isolated, parse_args,
};
use tui_bot::engine::STRATEGY_NAMES;

#[test]
fn dump_frame_exit_zero_and_surfaces() {
    let (code, text, _) = dump_frame_offline_isolated();
    assert_eq!(code, 0);
    assert!(text.contains("Прибыль счета"), "{text}");
    assert!(text.contains("Сумма счета"), "{text}");
    assert!(text.contains("Позиции / сделки"), "{text}");
    assert!(text.contains("Аналитика / график"), "{text}");
    assert!(text.contains("x закрыть все"), "{text}");
    for (_, title) in STRATEGY_NAMES {
        assert!(text.contains(title), "missing {title} in:\n{text}");
    }
}

#[test]
fn live_without_keys_is_refused() {
    let (code, text) = live_without_keys_isolated();
    assert_eq!(code, 2);
    let low = text.to_ascii_lowercase();
    assert!(
        low.contains("config") || low.contains("key") || text.contains("BINANCE"),
        "{text}"
    );
    let args = parse_args(["--dump-frame", "--strategy", "3", "--offline"]).unwrap();
    assert!(args.dump_frame);
    assert_eq!(args.strategy, "3");
    let args4 = parse_args(["--dump-frame", "--strategy", "4", "--offline"]).unwrap();
    assert_eq!(args4.strategy, "4");
    assert!(args.offline);
    assert!(parse_args(["--backtest"]).unwrap().backtest);
    assert!(parse_args(["--report"]).unwrap().report);
}

#[test]
fn dump_frame_strategy_4_shows_title() {
    let (code, text, _) = dump_frame_offline_strategy("4");
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("Continuation: откат ликвидных (не догон 24h %)"), "{text}");
    assert!(text.contains("Текущая: 4"), "{text}");
    assert!(text.contains("Momentum rider (растущий + TP + SL вверх)"), "{text}");
    assert!(text.contains("Скальп: откат к VWAP/EMA9"), "{text}");
    assert!(text.contains("Тренд: пробой Donchian 20/10 (день)"), "{text}");
    assert!(text.contains("00–02"), "{text}");
    assert!(text.contains("07–10"), "{text}");
    assert!(text.contains("13–16"), "{text}");
    assert!(!text.contains("круглосуточно"), "{text}");
}

#[test]
fn help_lists_flags() {
    let h = help_text();
    for flag in ["--dump-frame", "--offline", "--strategy", "--live", "--backtest", "--report"] {
        assert!(h.contains(flag), "help missing {flag}:\n{h}");
    }
}

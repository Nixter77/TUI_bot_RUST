use tui_bot::app::report_on_paths;

#[test]
fn report_on_empty_state_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, text) = report_on_paths(tmp.path());
    assert_eq!(code, 0);
    assert!(text.contains("ОТЧЁТ"), "{text}");
    assert!(text.contains("Сводка"), "{text}");
}

#[test]
fn report_shows_research_section() {
    use std::fs;
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let trades = tmp.path().join("trades.jsonl");
    let mut f = fs::File::create(&trades).unwrap();
    writeln!(
        f,
        r#"{{"ts":"2026-09-06T01:00:00Z","event":"close","strategy_id":4,"symbol":"AAAUSDT","qty":"1","price":"102","reason":"tp","pnl":"1.0","fee":"0.1","live":true,"final_r":"0.5","mfe_r":"1.0","mae_r":"0.2","hold_sec":60}}"#
    )
    .unwrap();
    let (code, text) = tui_bot::app::report_on_paths(tmp.path());
    assert_eq!(code, 0);
    assert!(text.contains("По стратегиям"), "{text}");
    assert!(
        text.contains("Exp R") || text.contains("0.50") || text.contains("+1.0000"),
        "{text}"
    );
}

#[test]
fn report_filter_by_strategy() {
    use std::fs;
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let trades = tmp.path().join("trades.jsonl");
    let mut f = fs::File::create(&trades).unwrap();
    writeln!(
        f,
        r#"{{"ts":"2026-09-06T01:00:00Z","event":"close","strategy_id":4,"symbol":"AAAUSDT","qty":"1","price":"102","reason":"tp","pnl":"1.0","fee":"0.1","live":true,"final_r":"0.5"}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"ts":"2026-09-06T02:00:00Z","event":"close","strategy_id":2,"symbol":"BBBUSDT","qty":"1","price":"50","reason":"sl","pnl":"-0.5","fee":"0.1","live":true,"final_r":"-1.0"}}"#
    )
    .unwrap();
    let text_all = tui_bot::report::format_report(Some(&trades), None);
    let text_s4 = tui_bot::report::format_report_filtered(Some(&trades), None, Some(4));
    assert!(text_all.contains("AAAUSDT") && text_all.contains("BBBUSDT"), "{text_all}");
    assert!(text_s4.contains("AAAUSDT"), "{text_s4}");
    assert!(!text_s4.contains("BBBUSDT"), "{text_s4}");
    assert!(text_s4.contains("strategy 4"), "{text_s4}");
}

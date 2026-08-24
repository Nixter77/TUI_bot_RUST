use tui_bot::app::report_on_paths;

#[test]
fn report_on_empty_state_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, text) = report_on_paths(tmp.path());
    assert_eq!(code, 0);
    assert!(text.contains("home-economic report"));
    assert!(text.contains("сделки:"));
}

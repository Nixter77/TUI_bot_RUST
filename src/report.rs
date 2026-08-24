//! Operator report from trades.jsonl + errors.jsonl. No network.

use crate::errorlog::{read_error_events, DEFAULT_ERROR_LOG_PATH};
use crate::journal::{parse_pnl, TradeJournal, DEFAULT_JOURNAL_PATH};
use crate::money::dec;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::path::Path;

pub fn format_report(trades_path: Option<&Path>, errors_path: Option<&Path>) -> String {
    let journal = TradeJournal::new(Some(trades_path.unwrap_or(Path::new(DEFAULT_JOURNAL_PATH))));
    let events = journal.read_events();
    let closes: Vec<_> = events.iter().filter(|e| e.event == "close").collect();
    let opens: Vec<_> = events.iter().filter(|e| e.event == "open").collect();
    let skips: Vec<_> = events.iter().filter(|e| e.event == "skip").collect();
    let flats: Vec<_> = events.iter().filter(|e| e.event == "flatten").collect();
    let pnl = closes
        .iter()
        .filter_map(|e| parse_pnl(e.pnl.as_deref()))
        .fold(Decimal::ZERO, |a, b| a + b);
    let fee = closes
        .iter()
        .filter_map(|e| e.fee.as_deref().and_then(|s| dec(s).ok()))
        .fold(Decimal::ZERO, |a, b| a + b);
    let wins = closes
        .iter()
        .filter(|e| parse_pnl(e.pnl.as_deref()).map(|p| p > Decimal::ZERO).unwrap_or(false))
        .count();
    let wr = if closes.is_empty() {
        "—".to_string()
    } else {
        format!("{:.1}%", wins as f64 / closes.len() as f64 * 100.0)
    };

    let mut lines = vec![
        "home-economic report".into(),
        format!(
            "сделки: open={} close={} flatten={} skip={}",
            opens.len(),
            closes.len(),
            flats.len(),
            skips.len()
        ),
        format!("закрытия: wr={wr}  нетто={pnl:+.4}  комиссия={fee:.4}"),
    ];
    if !closes.is_empty() {
        lines.push("последние закрытия:".into());
        let start = closes.len().saturating_sub(8);
        for event in &closes[start..] {
            let clock = if event.ts.len() >= 19 {
                &event.ts[11..19]
            } else {
                &event.ts
            };
            lines.push(format!(
                "  {clock} {} нетто={} комиссия={} ({})",
                event.symbol,
                event.pnl.as_deref().unwrap_or("—"),
                event.fee.as_deref().unwrap_or("—"),
                event.reason
            ));
        }
    }
    if !skips.is_empty() {
        let mut skip_codes: HashMap<String, usize> = HashMap::new();
        for e in &skips {
            let key = e
                .code
                .clone()
                .unwrap_or_else(|| e.reason.chars().take(40).collect());
            *skip_codes.entry(key).or_insert(0) += 1;
        }
        lines.push("отказы входа:".into());
        let mut items: Vec<_> = skip_codes.into_iter().collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (key, n) in items.into_iter().take(8) {
            lines.push(format!("  {n:3}  {key}"));
        }
    }

    let err_events = read_error_events(Some(errors_path.unwrap_or(Path::new(DEFAULT_ERROR_LOG_PATH))));
    let shown: Vec<_> = err_events.iter().filter(|e| e.event == "shown").collect();
    if !shown.is_empty() {
        let mut codes: HashMap<String, usize> = HashMap::new();
        for e in &shown {
            let key = if e.code.is_empty() {
                e.shown.chars().take(40).collect()
            } else {
                e.code.clone()
            };
            *codes.entry(key).or_insert(0) += 1;
        }
        lines.push("ошибки TUI (shown):".into());
        let mut items: Vec<_> = codes.into_iter().collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (key, n) in items.into_iter().take(8) {
            lines.push(format!("  {n:3}  {key}"));
        }
    }
    if lines.len() == 3 && closes.is_empty() && skips.is_empty() && shown.is_empty() {
        lines.push("(журналы пусты)".into());
    }
    lines.push(String::new());
    lines.join("\n")
}

pub fn run_cli() -> i32 {
    print!("{}", format_report(None, None));
    0
}

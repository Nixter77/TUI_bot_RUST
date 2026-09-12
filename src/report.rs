//! Operator report from trades.jsonl + errors.jsonl. No network.
//! Includes Phase-1 research table (R expectancy, MFE/MAE) per strategy_id.

use crate::errorlog::{read_error_events, DEFAULT_ERROR_LOG_PATH};
use crate::journal::{parse_pnl, TradeEvent, TradeJournal, DEFAULT_JOURNAL_PATH};
use crate::money::dec;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::path::Path;

fn parse_opt_dec(raw: Option<&str>) -> Option<Decimal> {
    raw.and_then(|s| dec(s).ok())
}

fn mean(xs: &[Decimal]) -> Option<Decimal> {
    if xs.is_empty() {
        None
    } else {
        let sum: Decimal = xs.iter().copied().sum();
        Some(sum / Decimal::from(xs.len() as u64))
    }
}

fn fmt_opt_dec(v: Option<Decimal>, places: u32) -> String {
    match v {
        Some(d) => format!("{:.prec$}", d, prec = places as usize),
        None => "—".into(),
    }
}

/// Short money for display (4 dp). Never dump raw Decimal strings.
fn fmt_money(v: Decimal) -> String {
    format!("{v:.4}")
}

fn fmt_money_signed(v: Decimal) -> String {
    format!("{v:+.4}")
}

fn fmt_opt_money(raw: Option<&str>) -> String {
    match raw.and_then(|s| dec(s).ok()) {
        Some(d) => fmt_money_signed(d),
        None => "—".into(),
    }
}

fn fmt_opt_fee(raw: Option<&str>) -> String {
    match raw.and_then(|s| dec(s).ok()) {
        Some(d) => fmt_money(d),
        None => "—".into(),
    }
}

fn fmt_opt_r(raw: Option<&str>) -> String {
    match raw.and_then(|s| dec(s).ok()) {
        Some(d) => format!("{d:.2}"),
        None => "—".into(),
    }
}

fn rule(title: &str) -> String {
    format!("── {title} ──")
}

fn is_partial_scale(ev: &TradeEvent) -> bool {
    ev.reason
        .to_ascii_lowercase()
        .contains("частичная фиксация")
}

#[derive(Default)]
struct StratAgg {
    closes: usize,
    /// Full (non-scale-out) closes used for trade count / WR / PnL.
    trades: usize,
    wins: usize,
    pnl: Decimal,
    final_rs: Vec<Decimal>,
    win_rs: Vec<Decimal>,
    loss_rs: Vec<Decimal>,
    mfe_rs: Vec<Decimal>,
    mae_rs: Vec<Decimal>,
    holds: Vec<Decimal>,
    gross_win: Decimal,
    gross_loss_abs: Decimal,
}

fn aggregate(closes: &[&TradeEvent]) -> BTreeMap<i32, StratAgg> {
    let mut by: BTreeMap<i32, StratAgg> = BTreeMap::new();
    for ev in closes {
        let a = by.entry(ev.strategy_id).or_default();
        a.closes += 1;
        let partial = is_partial_scale(ev);
        let pnl = parse_pnl(ev.pnl.as_deref()).unwrap_or(Decimal::ZERO);
        if !partial {
            a.trades += 1;
            a.pnl += pnl;
            if pnl > Decimal::ZERO {
                a.wins += 1;
                a.gross_win += pnl;
            } else if pnl < Decimal::ZERO {
                a.gross_loss_abs += -pnl;
            }
            if let Some(r) = parse_opt_dec(ev.final_r.as_deref()) {
                a.final_rs.push(r);
                if r > Decimal::ZERO {
                    a.win_rs.push(r);
                } else if r < Decimal::ZERO {
                    a.loss_rs.push(r);
                }
            }
            if let Some(r) = parse_opt_dec(ev.mfe_r.as_deref()) {
                a.mfe_rs.push(r);
            }
            if let Some(r) = parse_opt_dec(ev.mae_r.as_deref()) {
                a.mae_rs.push(r);
            }
            if let Some(h) = ev.hold_sec {
                a.holds.push(Decimal::from(h));
            }
        } else {
            let _ = pnl;
        }
    }
    by
}

/// Below this full-trade count, WR is noise — soak on N + fee/funding cost instead.
const SOAK_MIN_TRADES: usize = 30;

fn format_research(closes: &[&TradeEvent]) -> Vec<String> {
    let by = aggregate(closes);
    let mut lines = vec![
        String::new(),
        rule("По стратегиям"),
        format!(
            "  {:<4} {:>6} {:>7} {:>10} {:>8} {:>7} {:>7} {:>5} {:>6} {:>6} {:>6}",
            "sid", "N", "WR*", "PnL USDT", "Exp R", "AvgW", "AvgL", "PF", "MFE", "MAE", "Hold"
        ),
    ];
    if by.is_empty() {
        lines.push("  (нет закрытий)".into());
        return lines;
    }

    let mut footnotes: Vec<String> = Vec::new();
    let mut small_n = false;
    for (sid, a) in &by {
        let wr = if a.trades == 0 {
            "—".into()
        } else if a.trades < SOAK_MIN_TRADES {
            small_n = true;
            "n/a".into()
        } else {
            format!("{:.1}%", a.wins as f64 / a.trades as f64 * 100.0)
        };
        let n_r = a.final_rs.len();
        let exp = mean(&a.final_rs);
        let avg_w = mean(&a.win_rs);
        let avg_l = mean(&a.loss_rs);
        let pf = if a.gross_loss_abs > Decimal::ZERO {
            Some(a.gross_win / a.gross_loss_abs)
        } else if a.gross_win > Decimal::ZERO {
            None
        } else {
            None
        };
        let exp_s = if n_r == 0 {
            "—".into()
        } else {
            fmt_opt_dec(exp, 2)
        };
        lines.push(format!(
            "  {sid:<4} {:>6} {:>7} {:>10} {:>8} {:>7} {:>7} {:>5} {:>6} {:>6} {:>6}",
            a.trades,
            wr,
            fmt_money_signed(a.pnl),
            exp_s,
            fmt_opt_dec(avg_w, 2),
            fmt_opt_dec(avg_l, 2),
            fmt_opt_dec(pf, 2),
            fmt_opt_dec(mean(&a.mfe_rs), 2),
            fmt_opt_dec(mean(&a.mae_rs), 2),
            fmt_opt_dec(mean(&a.holds), 0),
        ));
        if n_r < a.trades && a.trades > 0 {
            footnotes.push(format!("sid {sid}: final_r {n_r}/{}", a.trades));
        }
    }
    if !footnotes.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "  прим.: {} — старые строки без R",
            footnotes.join("; ")
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "  soak: смотри N + комиссия/funding (cost), не WR при N<{SOAK_MIN_TRADES}; метрика победы = Exp R / SQN / PF"
    ));
    if small_n {
        lines.push(format!(
            "  * WR=n/a пока N<{SOAK_MIN_TRADES} полных closes (мало для winrate)"
        ));
    }

    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for ev in closes {
        if is_partial_scale(ev) {
            continue;
        }
        let key = if ev.reason.is_empty() {
            "(empty)".into()
        } else {
            ev.reason.chars().take(48).collect()
        };
        *reasons.entry(key).or_insert(0) += 1;
    }
    if !reasons.is_empty() {
        lines.push(String::new());
        lines.push(rule("Причины выхода"));
        let mut items: Vec<_> = reasons.into_iter().collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (k, n) in items.into_iter().take(12) {
            lines.push(format!("  {n:>4}  {k}"));
        }
    }
    lines
}

fn filter_events<'a>(events: &'a [TradeEvent], strategy: Option<i32>) -> Vec<&'a TradeEvent> {
    match strategy {
        Some(sid) => events.iter().filter(|e| e.strategy_id == sid).collect(),
        None => events.iter().collect(),
    }
}

pub fn format_report(trades_path: Option<&Path>, errors_path: Option<&Path>) -> String {
    format_report_filtered(trades_path, errors_path, None)
}

/// `strategy`: when `Some(N)`, only events with that strategy_id (used by `--research --strategy N`).
pub fn format_report_filtered(
    trades_path: Option<&Path>,
    errors_path: Option<&Path>,
    strategy: Option<i32>,
) -> String {
    let journal = TradeJournal::new(Some(trades_path.unwrap_or(Path::new(DEFAULT_JOURNAL_PATH))));
    let events = journal.read_events();
    let scoped = filter_events(&events, strategy);
    let closes: Vec<_> = scoped
        .iter()
        .copied()
        .filter(|e| e.event == "close")
        .collect();
    let opens: Vec<_> = scoped
        .iter()
        .copied()
        .filter(|e| e.event == "open")
        .collect();
    let skips: Vec<_> = scoped
        .iter()
        .copied()
        .filter(|e| e.event == "skip")
        .collect();
    let flats: Vec<_> = scoped
        .iter()
        .copied()
        .filter(|e| e.event == "flatten")
        .collect();
    let pnl = closes
        .iter()
        .filter_map(|e| parse_pnl(e.pnl.as_deref()))
        .fold(Decimal::ZERO, |a, b| a + b);
    let fee = closes
        .iter()
        .filter_map(|e| e.fee.as_deref().and_then(|s| dec(s).ok()))
        .fold(Decimal::ZERO, |a, b| a + b);
    let funding = closes
        .iter()
        .filter_map(|e| e.funding.as_deref().and_then(|s| dec(s).ok()))
        .fold(Decimal::ZERO, |a, b| a + b);
    let full_n = closes.iter().filter(|e| !is_partial_scale(e)).count();
    let wins = closes
        .iter()
        .filter(|e| {
            !is_partial_scale(e)
                && parse_pnl(e.pnl.as_deref())
                    .map(|p| p > Decimal::ZERO)
                    .unwrap_or(false)
        })
        .count();
    let wr = if full_n == 0 {
        "—".to_string()
    } else if full_n < SOAK_MIN_TRADES {
        format!("n/a(N<{SOAK_MIN_TRADES})")
    } else {
        format!("{:.1}%", wins as f64 / full_n as f64 * 100.0)
    };

    let scope = match strategy {
        Some(s) => format!("strategy {s}"),
        None => "все стратегии".into(),
    };

    let mut lines = vec![
        "══════════════════════════════════════".into(),
        format!("  ОТЧЁТ  ·  {scope}"),
        "══════════════════════════════════════".into(),
        String::new(),
        rule("Сводка"),
        format!(
            "  open={:<4} close={:<4} flatten={:<3} skip={}",
            opens.len(),
            closes.len(),
            flats.len(),
            skips.len()
        ),
        format!(
            "  N={full_n:<4} WR={wr:<14} нетто={}   комиссия={}   funding={}",
            fmt_money_signed(pnl),
            fmt_money(fee),
            fmt_money(funding)
        ),
    ];

    lines.extend(format_research(&closes));
    {
        let mut by_reg: BTreeMap<String, usize> = BTreeMap::new();
        let mut tagged = 0usize;
        for ev in &opens {
            if let Some(r) = ev.btc_regime.as_deref() {
                if !r.is_empty() {
                    tagged += 1;
                    *by_reg.entry(r.to_string()).or_insert(0) += 1;
                }
            }
        }
        if tagged > 0 {
            lines.push(String::new());
            lines.push(rule(&format!(
                "BTC regime на входах ({tagged}/{})",
                opens.len()
            )));
            for (k, n) in by_reg {
                lines.push(format!("  {n:>4}  {k}"));
            }
        }
    }
    if !closes.is_empty() {
        lines.push(String::new());
        lines.push(rule("Последние закрытия"));
        let start = closes.len().saturating_sub(8);
        for event in &closes[start..] {
            let clock = if event.ts.len() >= 19 {
                &event.ts[11..19]
            } else {
                &event.ts
            };
            let reason: String = event.reason.chars().take(36).collect();
            lines.push(format!(
                "  {clock}  {:<12}  {:>9}  fee {:>7}  R {:>5}  {}",
                event.symbol,
                fmt_opt_money(event.pnl.as_deref()),
                fmt_opt_fee(event.fee.as_deref()),
                fmt_opt_r(event.final_r.as_deref()),
                reason
            ));
        }
    }
    if !skips.is_empty() {
        let mut skip_codes: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for e in &skips {
            let key = e
                .code
                .clone()
                .unwrap_or_else(|| e.reason.chars().take(40).collect());
            *skip_codes.entry(key).or_insert(0) += 1;
        }
        lines.push(String::new());
        lines.push(rule("Отказы входа"));
        let mut items: Vec<_> = skip_codes.into_iter().collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (key, n) in items.into_iter().take(8) {
            lines.push(format!("  {n:>4}  {key}"));
        }
    }

    let err_events = read_error_events(Some(
        errors_path.unwrap_or(Path::new(DEFAULT_ERROR_LOG_PATH)),
    ));
    let shown: Vec<_> = err_events.iter().filter(|e| e.event == "shown").collect();
    if !shown.is_empty() {
        let mut codes: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for e in &shown {
            let key = if e.code.is_empty() {
                e.shown.chars().take(40).collect()
            } else {
                e.code.clone()
            };
            *codes.entry(key).or_insert(0) += 1;
        }
        lines.push(String::new());
        lines.push(rule("Ошибки TUI"));
        let mut items: Vec<_> = codes.into_iter().collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (key, n) in items.into_iter().take(8) {
            lines.push(format!("  {n:>4}  {key}"));
        }
    }
    if lines.len() <= 8 && closes.is_empty() && skips.is_empty() && shown.is_empty() {
        lines.push("(журналы пусты)".into());
    }
    lines.push(String::new());
    lines.join("\n")
}

pub fn run_cli() -> i32 {
    run_cli_filtered(None)
}

pub fn run_cli_filtered(strategy: Option<i32>) -> i32 {
    print!("{}", format_report_filtered(None, None, strategy));
    0
}

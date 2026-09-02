//! Text frame for the TUI (also the --dump-frame / no-TTY path).

use crate::config::TradeInterval;
use crate::engine::strategy_title;
use crate::errorlog::format_ui_error;
use crate::models::{Position, RecentAction, Side, Ticker};
use crate::profit::{account_profit as calc_account_profit, current_equity};
use crate::sessions::{session_status, unix_now, HourWindow, SessionStatus, DEFAULT_ENTRY_WINDOWS};
use rust_decimal::Decimal;
use std::collections::HashMap;

const SPARK_BLOCKS: &str = "▁▂▃▄▅▆▇█";

#[derive(Debug, Clone)]
pub struct ViewModel {
    pub strategy_id: i32,
    pub wallet_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub starting_equity: Decimal,
    pub available_balance: Decimal,
    pub positions: Vec<Position>,
    pub recent_actions: Vec<RecentAction>,
    pub tickers: Vec<Ticker>,
    pub chart_symbol: String,
    pub chart_closes: Vec<Decimal>,
    pub last_error: Option<String>,
    pub logged_error: Option<String>,
    pub live: bool,
    pub has_credentials: bool,
    pub poll_seconds: i32,
    pub last_decision: String,
    pub mode_note: String,
    pub flatten_armed: bool,
    pub entries_paused: bool,
    pub now_ts: Option<f64>,
    pub entry_windows: Vec<HourWindow>,
    pub always_enter: bool,
    pub signals_on: bool,
    pub journal_lines: Vec<String>,
    pub leverage: Option<i32>,
    pub order_notional: Decimal,
    /// S4 risk fraction of equity. `0` = off (ORDER_NOTIONAL fallback).
    pub risk_pct: Decimal,
    pub notional_from_exchange: bool,
    pub max_positions: i32,
    pub basket_symbols: Vec<String>,
    pub cooldown_until: f64,
    pub cooldowns: HashMap<String, f64>,
    pub error_source: String,
    pub unmanaged_symbols: Vec<String>,
    pub flatten_leftovers: bool,
    pub daily_halt: bool,
    pub daily_loss_usdt: Decimal,
    pub daily_loss_r: Decimal,
    pub day_pnl: Option<Decimal>,
    pub s4_interval: TradeInterval,
}

impl Default for ViewModel {
    fn default() -> Self {
        Self {
            strategy_id: 1,
            wallet_balance: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            starting_equity: Decimal::ZERO,
            available_balance: Decimal::ZERO,
            positions: Vec::new(),
            recent_actions: Vec::new(),
            tickers: Vec::new(),
            chart_symbol: String::new(),
            chart_closes: Vec::new(),
            last_error: None,
            logged_error: None,
            live: false,
            has_credentials: false,
            poll_seconds: 60,
            last_decision: "—".into(),
            mode_note: String::new(),
            flatten_armed: false,
            entries_paused: false,
            now_ts: None,
            entry_windows: DEFAULT_ENTRY_WINDOWS.to_vec(),
            always_enter: false,
            signals_on: false,
            journal_lines: Vec::new(),
            leverage: None,
            order_notional: Decimal::from(20),
            risk_pct: crate::config::default_risk_pct(),
            notional_from_exchange: false,
            max_positions: 1,
            basket_symbols: Vec::new(),
            cooldown_until: 0.0,
            cooldowns: HashMap::new(),
            error_source: String::new(),
            unmanaged_symbols: Vec::new(),
            flatten_leftovers: false,
            daily_halt: false,
            daily_loss_usdt: Decimal::from(20),
            daily_loss_r: Decimal::from(3),
            day_pnl: None,
            s4_interval: TradeInterval::Minute5,
        }
    }
}

impl ViewModel {
    pub fn session(&self) -> SessionStatus {
        let ts = self.now_ts.unwrap_or_else(now_secs);
        session_status(ts, Some(&self.entry_windows), self.always_enter)
    }

    pub fn banner(&self) -> &'static str {
        if self.flatten_armed {
            "confirm"
        } else if self.entries_paused {
            "paused"
        } else if self.daily_halt {
            "daily"
        } else {
            "idle"
        }
    }
}

fn now_secs() -> f64 {
    unix_now()
}

fn pct_label(frac: Decimal) -> String {
    (frac * Decimal::from(100)).normalize().to_string()
}

/// S4 book size: risk-% of equity / stop. Stop is per-setup, not in the snapshot.
fn s4_risk_size(view: &ViewModel) -> String {
    let pct = pct_label(view.risk_pct);
    let equity = current_equity(view.wallet_balance, view.unrealized_pnl);
    let min_stop = view.s4_interval.min_stop_pct();
    // Same helper as live: notional = (equity * risk_pct) * entry / (entry - sl).
    // Illustrate with the TF SL floor; any entry yields the same ratio.
    let entry = Decimal::from(100);
    let sl = entry * (Decimal::ONE - min_stop);
    match crate::exchange::risk_position_notional(equity, view.risk_pct, entry, sl) {
        Some(plan) => format!(
            "риск {pct}% счета / стоп (до {} USDT при SL {}%)",
            plan.notional.round_dp(1).normalize(),
            pct_label(min_stop)
        ),
        None => format!("риск {pct}% счета / стоп"),
    }
}

fn book_line(view: &ViewModel) -> String {
    let lev = if let Some(l) = view.leverage {
        format!("плечо {l}x")
    } else {
        "плечо как на Binance".into()
    };
    let size = if view.strategy_id == 4 && view.risk_pct > Decimal::ZERO {
        s4_risk_size(view)
    } else if view.notional_from_exchange {
        "сумма = minNotional биржи".into()
    } else {
        format!("сумма {} USDT", view.order_notional)
    };
    let basket = if view.basket_symbols.is_empty() {
        "—".into()
    } else {
        view.basket_symbols.join(", ")
    };
    if view.strategy_id == 1 || view.strategy_id == 4 {
        format!("{lev}  |  {size}  |  корзина до {}: {basket}", view.max_positions)
    } else {
        format!("{lev}  |  {size}  |  скан: {basket}")
    }
}

fn fmt_remain(seconds: f64) -> String {
    let sec = seconds.max(0.0) as i64;
    if sec < 60 {
        return format!("{sec} с");
    }
    let mins = sec / 60;
    let rem = sec % 60;
    if rem == 0 {
        format!("{mins} мин")
    } else {
        format!("{mins} мин {rem} с")
    }
}

fn fmt_utc_clock(ts: f64) -> String {
    crate::sessions::utc_datetime(ts).format("%H:%M").to_string()
}

fn fmt_utc_hms(ts: f64) -> String {
    crate::sessions::utc_datetime(ts).format("%H:%M:%S").to_string()
}

/// One heading plus a row per cooling symbol. Empty when nothing is paused.
pub fn cooldown_lines(now: f64, cooldown_until: f64, cooldowns: &HashMap<String, f64>) -> Vec<String> {
    let mut active: Vec<(String, f64)> = cooldowns
        .iter()
        .filter(|(_, until)| **until > now)
        .map(|(s, u)| (s.clone(), *u))
        .collect();
    active.sort_by(|a, b| a.0.cmp(&b.0));
    if active.is_empty() {
        if cooldown_until > now {
            return vec![
                "Пауза после сделки:".into(),
                format!(
                    "  • стол  ещё {}  → {} UTC",
                    fmt_remain(cooldown_until - now),
                    fmt_utc_clock(cooldown_until)
                ),
            ];
        }
        return Vec::new();
    }
    let mut out = vec!["Пауза после сделки:".into()];
    for (symbol, until) in active {
        out.push(format!(
            "  • {symbol}  ещё {}  → {} UTC",
            fmt_remain(until - now),
            fmt_utc_clock(until)
        ));
    }
    out
}

fn session_line(view: &ViewModel) -> Option<String> {
    if view.strategy_id != 1 && view.strategy_id != 4 {
        return None;
    }
    let sess = view.session();
    let tag = if view.strategy_id == 4 {
        "Continuation"
    } else {
        "Momentum"
    };
    let tf = if view.strategy_id == 4 {
        format!(
            "  |  свечи {}  |  {}",
            view.s4_interval.as_ru(),
            view.s4_interval.geometry_ru()
        )
    } else {
        String::new()
    };
    if sess.open {
        Some(format!(
            "{tag}: {}  |  сейчас {} UTC  |  входы {}{tf}",
            sess.label, sess.utc_clock, sess.windows_text
        ))
    } else {
        let nxt = sess
            .next_open_clock
            .as_ref()
            .map(|c| format!("  |  следующий старт {c} UTC"))
            .unwrap_or_default();
        Some(format!(
            "{tag}: {}  |  сейчас {} UTC  |  входы {}{tf}{nxt}",
            sess.label, sess.utc_clock, sess.windows_text
        ))
    }
}

pub fn account_profit_figure(view: &ViewModel) -> Decimal {
    calc_account_profit(view.wallet_balance, view.unrealized_pnl, view.starting_equity)
}

/// Green/red for profit vs loss. Yellow = 1R still approaching.
/// Zero profit counts as profit (same as the Python TUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineTone {
    Profit,
    Loss,
    Warn,
}

/// How far a long is from locking +1R as money and as percent of 1R.
/// 1R USDT = qty × (entry − SL); remaining % is leftover / 1R, not leftover to the stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneRStatus {
    NoStop,
    Reached,
    Remaining { usdt: Decimal, pct: Decimal },
}

pub fn one_r_status(pos: &Position, mark: Decimal) -> OneRStatus {
    if pos.side != Side::Long {
        return OneRStatus::NoStop;
    }
    let Some(sl) = pos.stop_loss else {
        return OneRStatus::NoStop;
    };
    if pos.entry_price <= Decimal::ZERO || mark <= Decimal::ZERO {
        return OneRStatus::NoStop;
    }
    if sl >= pos.entry_price {
        return OneRStatus::Reached;
    }
    let risk = pos.entry_price - sl;
    if risk <= Decimal::ZERO {
        return OneRStatus::Reached;
    }
    let remain_price = pos.entry_price + risk - mark;
    if remain_price <= Decimal::ZERO {
        return OneRStatus::Reached;
    }
    let one_r_usdt = pos.qty.max(Decimal::ZERO) * risk;
    let usdt = pos.qty.max(Decimal::ZERO) * remain_price;
    let pct = if one_r_usdt > Decimal::ZERO {
        usdt / one_r_usdt * Decimal::from(100)
    } else {
        remain_price / risk * Decimal::from(100)
    };
    OneRStatus::Remaining { usdt, pct }
}

fn position_mark(pos: &Position, tickers: &[Ticker]) -> Decimal {
    if let Some(t) = tickers
        .iter()
        .find(|t| t.symbol.eq_ignore_ascii_case(&pos.symbol))
    {
        if t.last_price > Decimal::ZERO {
            return t.last_price;
        }
    }
    if pos.qty > Decimal::ZERO && pos.entry_price > Decimal::ZERO {
        let delta = pos.unrealized_pnl / pos.qty;
        return match pos.side {
            Side::Long => pos.entry_price + delta,
            Side::Short => pos.entry_price - delta,
        };
    }
    pos.entry_price
}

fn one_r_line(pos: &Position, mark: Decimal) -> Option<String> {
    if pos.side != Side::Long {
        return None;
    }
    let text = match one_r_status(pos, mark) {
        OneRStatus::NoStop => "до 1R: нет стопа".to_string(),
        OneRStatus::Reached => "до 1R: пройден".to_string(),
        OneRStatus::Remaining { usdt, pct } => format!(
            "до 1R: ещё {} USDT (осталось {}%)",
            fmt_money(usdt),
            pct.round_dp(1).normalize()
        ),
    };
    Some(format!("  {text}"))
}

fn one_r_tone(line: &str) -> Option<LineTone> {
    if !line.contains("до 1R:") {
        return None;
    }
    if line.contains("пройден") {
        return Some(LineTone::Profit);
    }
    if line.contains("нет стопа") {
        return Some(LineTone::Warn);
    }
    let left = number_after(line, "осталось ")?;
    if left <= Decimal::ZERO {
        Some(LineTone::Profit)
    } else if left < Decimal::from(50) {
        Some(LineTone::Warn)
    } else {
        Some(LineTone::Loss)
    }
}

fn parse_leading_decimal(raw: &str) -> Option<Decimal> {
    let s = raw.trim_start();
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut end = 0usize;
    if bytes[0] == b'+' || bytes[0] == b'-' {
        end = 1;
    }
    let digits_at = end;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    if end == digits_at {
        return None;
    }
    let token = &s[..end];
    let token = token.strip_prefix('+').unwrap_or(token);
    token.parse().ok()
}

fn tone_of(value: Decimal) -> LineTone {
    if value < Decimal::ZERO {
        LineTone::Loss
    } else {
        LineTone::Profit
    }
}

fn number_after(line: &str, marker: &str) -> Option<Decimal> {
    let i = line.find(marker)?;
    parse_leading_decimal(&line[i + marker.len()..])
}

/// Color hint for a painted TUI line. `dump-frame` stays plain text.
pub fn line_tone(line: &str, account_profit: Decimal) -> Option<LineTone> {
    if line.contains("Прибыль счета") {
        return Some(tone_of(account_profit));
    }
    if line.contains("Нереализованный PnL") || line.contains("ованный PnL") {
        return number_after(line, ":").or_else(|| parse_leading_decimal(line)).map(tone_of);
    }
    if let Some(v) = number_after(line, "uPnL=") {
        return Some(tone_of(v));
    }
    if let Some(v) = number_after(line, "нетто=") {
        return Some(tone_of(v));
    }
    if line.contains("день:") {
        if let Some(v) = number_after(line, "день:") {
            return Some(tone_of(v));
        }
    }
    if let Some(tone) = one_r_tone(line) {
        return Some(tone);
    }
    if line.contains("last=") && line.contains('%') {
        if let Some(v) = number_after(line, " ").or_else(|| {
            line.split_whitespace()
                .find(|w| w.contains('%'))
                .and_then(|w| parse_leading_decimal(w))
        }) {
            return Some(tone_of(v));
        }
    }
    None
}

pub fn sparkline(values: &[Decimal], width: usize) -> String {
    if values.is_empty() {
        return "(нет данных графика)".into();
    }
    let pts: Vec<Decimal> = if values.len() <= width {
        values.to_vec()
    } else {
        let step = values.len() as f64 / width as f64;
        (0..width)
            .map(|i| values[(i as f64 * step) as usize])
            .collect()
    };
    let lo = *pts.iter().min().unwrap();
    let hi = *pts.iter().max().unwrap();
    let span = hi - lo;
    if span == Decimal::ZERO {
        return "▄".repeat(pts.len());
    }
    let last = SPARK_BLOCKS.chars().count() - 1;
    let blocks: Vec<char> = SPARK_BLOCKS.chars().collect();
    pts.iter()
        .map(|v| {
            let mut idx = ((*v - lo) / span * Decimal::from(last as i32))
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0) as i32;
            if idx < 0 {
                idx = 0;
            }
            if idx as usize > last {
                idx = last as i32;
            }
            blocks[idx as usize]
        })
        .collect()
}

fn fmt_money(value: Decimal) -> String {
    format!("{:.4}", value.round_dp(4))
}

fn fmt_price(value: Decimal) -> String {
    let abs = value.abs();
    if abs < Decimal::ONE {
        format!("{:.6}", value.round_dp(6))
    } else if abs < Decimal::from(100) {
        format!("{:.5}", value.round_dp(5))
    } else {
        format!("{:.4}", value.round_dp(4))
    }
}

fn ru_positions(n: usize) -> String {
    let mod10 = n % 10;
    let mod100 = n % 100;
    if mod10 == 1 && mod100 != 11 {
        format!("{n} позицию")
    } else if (2..=4).contains(&mod10) && ![12, 13, 14].contains(&mod100) {
        format!("{n} позиции")
    } else {
        format!("{n} позиций")
    }
}

/// Rows shown in «Топ роста» / «Топ падения». The rest of the 24h tape stays off-screen.
pub const TOP_MOVERS_N: usize = 5;

/// Re-sort the full 24h tape each frame. Names rotate as percents change.
pub fn top_movers(tickers: &[Ticker], n: usize) -> (Vec<Ticker>, Vec<Ticker>) {
    let mut rising = tickers.to_vec();
    rising.sort_by(|a, b| {
        b.price_change_percent
            .cmp(&a.price_change_percent)
            .then(b.quote_volume.cmp(&a.quote_volume))
            .then(a.symbol.cmp(&b.symbol))
    });
    rising.truncate(n);
    let mut falling = tickers.to_vec();
    falling.sort_by(|a, b| {
        a.price_change_percent
            .cmp(&b.price_change_percent)
            .then(b.quote_volume.cmp(&a.quote_volume))
            .then(a.symbol.cmp(&b.symbol))
    });
    falling.truncate(n);
    (rising, falling)
}

fn top_heading(label: &str, shown: usize, total: usize) -> String {
    if total == 0 {
        format!("{label}:")
    } else {
        format!("{label} ({shown} из {total}):")
    }
}

pub fn render_frame(view: &ViewModel) -> String {
    let profit = account_profit_figure(view);
    let names = crate::engine::STRATEGY_NAMES;
    let mut choice_parts = Vec::new();
    for (sid, title) in names {
        let mark = if sid == view.strategy_id { "*" } else { " " };
        choice_parts.push(format!("[{sid}{mark}] {title}"));
    }
    let live_flag = if view.live { "LIVE" } else { "WATCH" };
    let cred = if view.has_credentials {
        "keys=env"
    } else {
        "keys=missing"
    };
    let header = format!("home-economic  |  Binance USDT-M Futures TestNet  |  {live_flag}  |  {cred}");

    let equity = current_equity(view.wallet_balance, view.unrealized_pnl);
    let acc_lines = [
        "=== Счёт ===".to_string(),
        format!("Баланс кошелька:     {} USDT", fmt_money(view.wallet_balance)),
        format!("Нереализованный PnL: {} USDT", fmt_money(view.unrealized_pnl)),
        format!("Сумма счета:         {} USDT", fmt_money(equity)),
        format!("Доступно:            {} USDT", fmt_money(view.available_balance)),
        format!("Прибыль счета:       {} USDT", fmt_money(profit)),
    ];

    let mut pos_lines = vec!["=== Позиции / сделки ===".to_string()];
    let unmanaged: std::collections::HashSet<String> = view
        .unmanaged_symbols
        .iter()
        .map(|s| s.to_ascii_uppercase())
        .collect();
    if view.positions.is_empty() {
        pos_lines.push("(нет открытых позиций)".into());
    } else {
        for pos in &view.positions {
            let sl = pos.stop_loss.map(fmt_price).unwrap_or_else(|| "—".into());
            let tp = pos.take_profit.map(fmt_price).unwrap_or_else(|| "—".into());
            let tag = if pos.side != Side::Long {
                "шорт, не ведём"
            } else if unmanaged.contains(&pos.symbol.to_ascii_uppercase()) {
                "не ведём"
            } else {
                "ведём"
            };
            pos_lines.push(format!(
                "{} {} qty={} entry={} SL={sl} TP={tp} uPnL={}  [{tag}]",
                pos.symbol,
                pos.side,
                fmt_money(pos.qty),
                fmt_price(pos.entry_price),
                fmt_money(pos.unrealized_pnl)
            ));
            if let Some(line) = one_r_line(pos, position_mark(pos, &view.tickers)) {
                pos_lines.push(line);
            }
        }
    }
    if !view.recent_actions.is_empty() {
        pos_lines.push("Последние решения:".into());
        let start = view.recent_actions.len().saturating_sub(5);
        for act in &view.recent_actions[start..] {
            pos_lines.push(format!("  • {} UTC  {}", fmt_utc_hms(act.at), act.text));
        }
    } else {
        pos_lines.push(format!("Последнее решение: {}", view.last_decision));
    }
    if !view.journal_lines.is_empty() {
        pos_lines.push("Журнал сделок:".into());
        for line in &view.journal_lines {
            pos_lines.push(format!("  • {line}"));
        }
    }
    let now = view.now_ts.unwrap_or_else(now_secs);
    pos_lines.extend(cooldown_lines(now, view.cooldown_until, &view.cooldowns));
    if !view.unmanaged_symbols.is_empty() && view.banner() != "confirm" {
        pos_lines.push(format!(
            "На бирже есть то, чем стратегия не управляет: {}. x x закроет только этот хвост.",
            view.unmanaged_symbols.join(", ")
        ));
    }

    let tape_n = view.tickers.len();
    let (rising, falling) = top_movers(&view.tickers, TOP_MOVERS_N);
    let chart_sym = if view.chart_symbol.is_empty() {
        rising
            .first()
            .map(|t| t.symbol.clone())
            .unwrap_or_else(|| "—".into())
    } else {
        view.chart_symbol.clone()
    };
    let mut analytics = vec![
        "=== Аналитика / график ===".to_string(),
        format!("Символ графика: {chart_sym}"),
        sparkline(&view.chart_closes, 48),
        top_heading("Топ роста", rising.len(), tape_n),
    ];
    if rising.is_empty() {
        analytics.push("  (нет тикеров)".into());
    } else {
        for t in &rising {
            analytics.push(format!(
                "  {:12} {:+.3}%  last={}",
                t.symbol,
                t.price_change_percent,
                fmt_price(t.last_price)
            ));
        }
    }
    analytics.push(top_heading("Топ падения", falling.len(), tape_n));
    if falling.is_empty() {
        analytics.push("  (нет тикеров)".into());
    } else {
        for t in &falling {
            analytics.push(format!(
                "  {:12} {:+.3}%  last={}",
                t.symbol,
                t.price_change_percent,
                fmt_price(t.last_price)
            ));
        }
    }

    let banner = view.banner();
    let (keys, status): (String, Option<String>) = if banner == "confirm" {
        let n = view.positions.len();
        let keys = "Клавиши: x — да, закрыть   |   любая другая — отмена".into();
        let status = if view.flatten_leftovers && !view.unmanaged_symbols.is_empty() {
            format!(
                "Подтверждение: ещё раз x закроет хвосты, которыми стратегия не управляет ({}). Стратегические лонги не трогает.",
                view.unmanaged_symbols.join(", ")
            )
        } else if n > 0 {
            format!(
                "Подтверждение: ещё раз x закроет {} рыночным ордером (лонги и шорты).",
                ru_positions(n)
            )
        } else {
            "Подтверждение: открытых позиций нет, закрывать нечего. Другая клавиша — назад.".into()
        };
        (keys, Some(status))
    } else if banner == "paused" {
        (
            "Клавиши: 1/2/3/4 выбор стратегии  |  x закрыть все (дважды)  |  q выход  |  r разрешить входы".into(),
            Some("Автопокупки выключены: только что закрывали все. r — снова разрешить стратегии покупать.".into()),
        )
    } else if banner == "daily" {
        let lost = view.day_pnl.map(fmt_money).unwrap_or_else(|| "—".into());
        (
            "Клавиши: 1/2/3/4 выбор стратегии  |  x закрыть все (дважды)  |  q выход".into(),
            Some(format!(
                "Стоп дня: прибыль дня {lost} USDT (лимиты −{} USDT / −{}R). Новых входов нет до 00:00 UTC. r это не снимает.",
                view.daily_loss_usdt,
                view.daily_loss_r
            )),
        )
    } else {
        let keys = "Клавиши: 1/2/3/4 выбор стратегии  |  x закрыть все (дважды)  |  q выход  |  r обновить".into();
        let status = if view.signals_on {
            Some("Звуки: покупка — два высоких; плюс — три вверх; минус — три вниз.".into())
        } else {
            None
        };
        (keys, status)
    };

    let mut footer = vec!["Стратегия (выбор 1/2/3/4):".to_string()];
    for part in choice_parts {
        footer.push(format!("  {part}"));
    }
    footer.extend([
        format!("Текущая: {} — {}", view.strategy_id, strategy_title(view.strategy_id)),
        format!("Каденс стратегии 1: {} с (1 или 2 минуты)", view.poll_seconds),
        book_line(view),
    ]);
    if let Some(session) = session_line(view) {
        footer.push(session);
    }
    footer.push(keys);
    if let Some(status) = status {
        footer.push(status);
    }
    footer.push("Логи сделок: .state/trades.jsonl".into());
    footer.push("Логи ошибок: .state/errors.jsonl".into());
    if !view.mode_note.is_empty() {
        footer.push(view.mode_note.clone());
    }
    if let Some(err) = &view.last_error {
        footer.push(format_ui_error(err));
    }

    let mut blocks = vec![header, String::new()];
    blocks.extend(acc_lines);
    blocks.push(String::new());
    blocks.extend(pos_lines);
    blocks.push(String::new());
    blocks.extend(analytics);
    blocks.push(String::new());
    blocks.extend(footer);
    format!("{}\n", blocks.join("\n"))
}

/// Pack a frame into at most `rows` display lines, each ≤ `width` cells.
/// Every logical line starts on a new row (column 0) so raw-mode terminals
/// cannot staircase `\n` without `\r`.
pub fn fit_lines(frame: &str, width: usize, rows: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in frame.lines() {
        if out.len() >= rows {
            break;
        }
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut buf = String::new();
        let mut n = 0usize;
        for ch in line.chars() {
            if n >= width {
                out.push(std::mem::take(&mut buf));
                n = 0;
                if out.len() >= rows {
                    return out;
                }
            }
            buf.push(ch);
            n += 1;
        }
        out.push(buf);
    }
    out
}

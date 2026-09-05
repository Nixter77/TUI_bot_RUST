//! Watch-only radar: waiting names, 24h tape, open/closed P&L. Never sends orders.

use crate::config::{Config, TradeInterval};
use crate::continuation::{liquid_universe, s4_setup_skip, ContinuationParams};
use crate::dayrisk::utc_day_key;
use crate::engine::strategy_title;
use crate::indicators::{last_ema, vwap};
use crate::journal::{event_unix, parse_pnl, TradeEvent};
use crate::models::{near_24h_high, EngineState, MarketSnapshot, Position, Side, Ticker};
use crate::momentum::{s1_setup_skip, MomentumParams};
use crate::profit::{account_profit, current_equity};
use crate::ranking::{iter_liquid_majors, pick_strategy1_book};
use crate::render::{cooldown_lines, one_r_status, top_movers, OneRStatus};
use crate::scalp::{scalp_decision, ScalpParams};
use crate::sessions::{
    format_windows, in_entry_window, next_window_start, outside_entry_reason, session_status,
    utc_datetime, HourWindow,
};
use crate::trend::trend_decision;
use crate::view::view_positions_with;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

pub const MONITOR_RISING_N: usize = 12;
pub const MONITOR_FALLING_N: usize = 5;
pub const MONITOR_WAIT_N: usize = 20;
pub const MONITOR_CLOSED_N: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    /// Setup is good; strategy would buy if gates/slots allow.
    Ready,
    /// In the universe, missing a setup (pullback, VWAP, HTF, …).
    Setup,
    /// Symbol cooldown after a close.
    Pause,
    /// Hours / halt / desk / full book / retry.
    Gate,
}

impl WaitKind {
    fn rank(self) -> u8 {
        match self {
            WaitKind::Ready => 0,
            WaitKind::Gate => 1,
            WaitKind::Pause => 2,
            WaitKind::Setup => 3,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            WaitKind::Ready => "готов",
            WaitKind::Setup => "сетап",
            WaitKind::Pause => "пауза",
            WaitKind::Gate => "блок",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WaitRow {
    pub symbol: String,
    pub change_pct: Decimal,
    pub last: Decimal,
    pub volume: Decimal,
    pub kind: WaitKind,
    pub reason: String,
    /// Body after «до входа:» — time, 24h %, or price gap.
    pub until: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClosedRow {
    pub clock: String,
    pub symbol: String,
    pub pnl: Decimal,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct MonitorView {
    pub strategy_id: i32,
    pub wallet_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub starting_equity: Decimal,
    pub available_balance: Decimal,
    pub account_profit: Decimal,
    pub day_pnl: Option<Decimal>,
    pub daily_halt: bool,
    pub positions: Vec<Position>,
    pub tickers: Vec<Ticker>,
    pub waiting: Vec<WaitRow>,
    pub rising: Vec<Ticker>,
    pub falling: Vec<Ticker>,
    pub tape_n: usize,
    pub closed_today: Vec<ClosedRow>,
    pub closed_net: Decimal,
    pub closed_wins: usize,
    pub closed_losses: usize,
    pub cooldown_until: f64,
    pub cooldowns: HashMap<String, f64>,
    pub now_ts: f64,
    pub last_error: Option<String>,
    pub has_credentials: bool,
    pub s4_interval: crate::config::TradeInterval,
    pub max_positions: i32,
    pub always_enter: bool,
    pub entry_windows: Vec<HourWindow>,
    pub session_open: bool,
    pub session_label: String,
    pub next_open_clock: Option<String>,
}

pub fn build_monitor(
    cfg: &Config,
    state: &EngineState,
    snapshot: &MarketSnapshot,
    events: &[TradeEvent],
    now: f64,
) -> MonitorView {
    let acc = &snapshot.account;
    let positions = view_positions_with(snapshot, &state.positions);
    let waiting = classify_waiting(cfg, state, snapshot, &positions, now);
    let (rising, falling) = top_movers(&snapshot.tickers, MONITOR_RISING_N.max(MONITOR_FALLING_N));
    let rising: Vec<Ticker> = rising.into_iter().take(MONITOR_RISING_N).collect();
    let falling: Vec<Ticker> = falling.into_iter().take(MONITOR_FALLING_N).collect();
    let (closed_today, closed_net, closed_wins, closed_losses) = closed_today_rows(events, now);
    let (windows, always) = session_knobs(cfg, state.strategy_id);
    let sess = session_status(now, Some(&windows), always);
    let day_pnl = state
        .day_start_equity
        .map(|start| acc.wallet_balance + acc.unrealized_pnl - start);
    MonitorView {
        strategy_id: state.strategy_id,
        wallet_balance: acc.wallet_balance,
        unrealized_pnl: acc.unrealized_pnl,
        starting_equity: acc.starting_equity,
        available_balance: acc.available_balance,
        account_profit: account_profit(acc.wallet_balance, acc.unrealized_pnl, acc.starting_equity),
        day_pnl,
        daily_halt: state.daily_halt,
        positions,
        tickers: snapshot.tickers.clone(),
        waiting,
        rising,
        falling,
        tape_n: snapshot.tickers.len(),
        closed_today,
        closed_net,
        closed_wins,
        closed_losses,
        cooldown_until: state.cooldown_until,
        cooldowns: state.cooldowns.clone(),
        now_ts: now,
        last_error: snapshot.last_error.clone().or_else(|| state.last_error.clone()),
        has_credentials: cfg.credentials.is_some(),
        s4_interval: cfg.s4_interval,
        max_positions: if state.strategy_id == 4 {
            cfg.s4_max_positions
        } else {
            cfg.max_positions
        },
        always_enter: always,
        entry_windows: windows,
        session_open: sess.open,
        session_label: sess.label,
        next_open_clock: sess.next_open_clock,
    }
}

fn session_knobs(cfg: &Config, strategy_id: i32) -> (Vec<HourWindow>, bool) {
    if strategy_id == 4 {
        (cfg.s4_entry_windows.clone(), cfg.s4_always_enter)
    } else if strategy_id == 1 {
        (cfg.entry_windows.clone(), cfg.always_enter)
    } else if strategy_id == 2 {
        (cfg.s2_entry_windows.clone(), cfg.s2_always_enter)
    } else {
        (Vec::new(), true)
    }
}

fn held_set(positions: &[Position]) -> HashSet<String> {
    positions
        .iter()
        .filter(|p| p.qty > Decimal::ZERO)
        .map(|p| p.symbol.to_ascii_uppercase())
        .collect()
}

fn global_gate(
    cfg: &Config,
    state: &EngineState,
    positions: &[Position],
    now: f64,
) -> Option<String> {
    if state.daily_halt {
        return Some("стоп дня — новых входов нет до 00:00 UTC".into());
    }
    if state.entries_paused {
        return Some("входы выключены (r в торговом TUI)".into());
    }
    if now < state.retry_until {
        return Some("пауза сети — входы закрыты".into());
    }
    if now < state.cooldown_until {
        return Some("пауза после стопа — слот не заполняю".into());
    }
    let (windows, always) = session_knobs(cfg, state.strategy_id);
    if !in_entry_window(now, Some(&windows), always) {
        let status = session_status(now, Some(&windows), always);
        return Some(outside_entry_reason(&status));
    }
    let open: Vec<&Position> = positions.iter().filter(|p| p.qty > Decimal::ZERO).collect();
    let max = if state.strategy_id == 4 {
        cfg.s4_max_positions
    } else if state.strategy_id == 1 {
        cfg.max_positions
    } else {
        1
    };
    if open.len() as i32 >= max.max(1) {
        return Some(format!("корзина полная ({}/{})", open.len(), max.max(1)));
    }
    // S4: allow next liquid up to max_positions regardless of open PnL (desk restore).
    // S1: still wait for green before scaling the basket.
    let not_green = open.iter().any(|p| p.unrealized_pnl <= Decimal::ZERO);
    if !open.is_empty() && not_green && state.strategy_id == 1 {
        return Some("слот не в плюсе — новый не открываю".into());
    }
    None
}

fn s4_params(cfg: &Config) -> ContinuationParams {
    let mut p = ContinuationParams::default().with_interval(cfg.s4_interval);
    p.max_positions = cfg.s4_max_positions;
    p.always_enter = cfg.s4_always_enter;
    p.entry_windows = cfg.s4_entry_windows.clone();
    p
}

fn s1_params(cfg: &Config) -> MomentumParams {
    MomentumParams {
        poll_seconds: cfg.poll_seconds,
        tp_pct: cfg.tp_pct,
        trail_pct: cfg.trail_pct,
        entry_windows: cfg.entry_windows.clone(),
        always_enter: cfg.always_enter,
        max_positions: cfg.max_positions,
        ..MomentumParams::default()
    }
}

fn push_unique(out: &mut Vec<Ticker>, seen: &mut HashSet<String>, t: Ticker) {
    let key = t.symbol.to_ascii_uppercase();
    if seen.insert(key) {
        out.push(t);
    }
}

fn candidate_tickers(cfg: &Config, state: &EngineState, snapshot: &MarketSnapshot) -> Vec<Ticker> {
    let skip = &state.skip_symbols;
    let mut out: Vec<Ticker> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    match state.strategy_id {
        4 => {
            // S4 desk = liquid volume book. Not the 24h % tape (that is «Топ роста»).
            let p = s4_params(cfg);
            for t in liquid_universe(&snapshot.tickers, skip, &p) {
                push_unique(&mut out, &mut seen, t.clone());
            }
        }
        1 => {
            for t in pick_strategy1_book(&snapshot.tickers, cfg.max_positions.max(8) as usize, skip) {
                push_unique(&mut out, &mut seen, t);
            }
        }
        _ => {
            for t in iter_liquid_majors(&snapshot.tickers, skip) {
                push_unique(&mut out, &mut seen, t);
            }
        }
    }
    out
}

fn setup_skip(
    cfg: &Config,
    state: &EngineState,
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    now: f64,
) -> Option<String> {
    match state.strategy_id {
        4 => {
            let p = s4_params(cfg);
            s4_setup_skip(snapshot, ticker, &p, &state.skip_symbols)
        }
        1 => {
            let p = s1_params(cfg);
            let book = pick_strategy1_book(
                &snapshot.tickers,
                p.max_positions.max(8) as usize,
                &state.skip_symbols,
            );
            let in_book = book
                .iter()
                .any(|t| t.symbol.eq_ignore_ascii_case(&ticker.symbol));
            s1_setup_skip(ticker, &snapshot.last_bars, in_book)
        }
        2 => {
            let p = ScalpParams::from_config(cfg);
            match scalp_decision(
                snapshot.bars_for(&ticker.symbol),
                None,
                &ticker.symbol,
                Some(&p),
                Some(now),
            ) {
                crate::models::Decision::Hold { reason } => Some(reason),
                crate::models::Decision::EnterLong { .. } => None,
                _ => None,
            }
        }
        3 => match trend_decision(
            snapshot.bars_for(&ticker.symbol),
            None,
            &ticker.symbol,
            None,
        ) {
            crate::models::Decision::Hold { reason } => Some(reason),
            crate::models::Decision::EnterLong { .. } => None,
            _ => None,
        },
        _ => Some("неизвестная стратегия".into()),
    }
}

fn pause_reason(state: &EngineState, symbol: &str, now: f64) -> Option<String> {
    let key = symbol.to_ascii_uppercase();
    let until = state.cooldowns.get(&key).copied().unwrap_or(0.0);
    if until > now {
        Some(format!("пауза после сделки ещё {}", fmt_remain(until - now)))
    } else {
        None
    }
}

fn until_clock(until: f64, now: f64) -> String {
    format!(
        "ещё {} → {} UTC",
        fmt_remain(until - now),
        utc_datetime(until).format("%H:%M")
    )
}

fn next_utc_midnight(now: f64) -> f64 {
    let t = utc_datetime(now);
    (t.date_naive() + chrono::Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .map(|n| n.and_utc().timestamp() as f64)
        .unwrap_or(now + 86_400.0)
}

fn next_bar_until(snapshot: &MarketSnapshot, symbol: &str, interval: TradeInterval, now: f64) -> String {
    let bars = snapshot.bars_for(symbol);
    let Some(last) = bars.last() else {
        if let Some(bar) = snapshot.last_bars.get(symbol) {
            return bar_close_until(bar.open_time, interval, now);
        }
        return format!("ждёт свечу {}", interval.as_ru());
    };
    bar_close_until(last.open_time, interval, now)
}

fn bar_close_until(open_time_ms: i64, interval: TradeInterval, now: f64) -> String {
    let close = (open_time_ms + interval.duration_ms()) as f64 / 1000.0;
    if close > now {
        format!(
            "ещё {} до закрытия {}",
            fmt_remain(close - now),
            interval.as_ru()
        )
    } else {
        format!("ждёт свечу {}", interval.as_ru())
    }
}

fn scan_until(state: &EngineState, poll_sec: f64, now: f64) -> String {
    if state.last_scan_ts <= 0.0 || poll_sec <= 0.0 {
        return "сейчас".into();
    }
    let due = state.last_scan_ts + poll_sec;
    if now >= due {
        "сейчас".into()
    } else {
        format!("ещё {} до скана", fmt_remain(due - now))
    }
}

fn pct_gap(value: Decimal) -> String {
    format!("{}", value.abs().round_dp(1).normalize())
}

/// How far 24h % is from the S4 buy band [min_change, max_change].
/// `stretch_pct` only blocks dumps (negative 24h) — same as `skip_24h_tape`.
fn tape_until(change: Decimal, p: &ContinuationParams) -> Option<String> {
    let lo = if p.min_change_percent > Decimal::ZERO {
        p.min_change_percent
    } else {
        Decimal::ZERO
    };
    // Mega-pump above max_change — ask for cool-off. Green stretch under max is OK.
    if let Some(max_c) = p.max_change_percent {
        if change > max_c {
            let gap = (change - max_c).max(Decimal::new(1, 1));
            return Some(format!(
                "ещё {}% 24h вниз (надо ≤ {}%)",
                pct_gap(gap),
                pct_gap(max_c)
            ));
        }
    }
    // Dump (≤ -stretch) or weak/flat day — need green lift into the band.
    if change <= -p.stretch_pct || change < lo {
        let need = (lo - change).max(Decimal::new(1, 1));
        return Some(format!(
            "ещё {}% 24h вверх (надо ≥ {}%)",
            pct_gap(need),
            pct_gap(lo)
        ));
    }
    None
}

fn price_until(last: Decimal, target: Decimal, label: &str) -> String {
    if last <= Decimal::ZERO || target <= Decimal::ZERO {
        return format!("ждёт {label}");
    }
    if last >= target {
        return format!("сейчас ({label})");
    }
    let usdt = target - last;
    let pct = usdt / last * Decimal::from(100);
    format!(
        "ещё {} USDT ({}%) до {label}",
        fmt_price(usdt),
        pct_gap(pct)
    )
}

fn s4_setup_until(
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    p: &ContinuationParams,
    now: f64,
) -> String {
    if let Some(text) = tape_until(ticker.price_change_percent, p) {
        return text;
    }
    if near_24h_high(ticker, p.near_high_frac) && ticker.high_price > Decimal::ZERO {
        let cap = ticker.high_price * (Decimal::ONE - p.near_high_frac);
        if ticker.last_price > cap && ticker.last_price > Decimal::ZERO {
            let pct = (ticker.last_price - cap) / ticker.last_price * Decimal::from(100);
            return format!("ещё {}% вниз от 24h high", pct_gap(pct));
        }
    }
    if snapshot.bars_for(&ticker.symbol).is_empty() && !snapshot.last_bars.contains_key(&ticker.symbol)
    {
        return next_bar_until(snapshot, &ticker.symbol, p.interval, now);
    }
    let htf = snapshot.htf_bars_for(&ticker.symbol);
    if htf.len() >= 21 {
        let closes: Vec<Decimal> = htf.iter().map(|b| b.close).collect();
        if let (Some(ema), Some(last)) = (last_ema(&closes, 20), htf.last()) {
            if last.close <= ema {
                return price_until(last.close, ema, "4ч EMA20");
            }
        }
    } else {
        return "ждёт 4ч историю".into();
    }
    let bars = snapshot.bars_for(&ticker.symbol);
    if bars.len() >= 21 {
        let closes: Vec<Decimal> = bars.iter().map(|b| b.close).collect();
        if let (Some(ema), Some(last)) = (last_ema(&closes, 20), bars.last()) {
            if last.close <= ema {
                return price_until(last.close, ema, "EMA20");
            }
        }
    }
    if let Some(vwap_price) = vwap(bars) {
        if ticker.last_price < vwap_price {
            return price_until(ticker.last_price, vwap_price, "VWAP");
        }
    }
    next_bar_until(snapshot, &ticker.symbol, p.interval, now)
}

fn until_entry(
    cfg: &Config,
    state: &EngineState,
    snapshot: &MarketSnapshot,
    ticker: &Ticker,
    kind: WaitKind,
    now: f64,
) -> String {
    match kind {
        WaitKind::Pause => {
            let until = state
                .cooldowns
                .get(&ticker.symbol.to_ascii_uppercase())
                .copied()
                .unwrap_or(0.0);
            if until > now {
                until_clock(until, now)
            } else {
                "сейчас".into()
            }
        }
        WaitKind::Gate => {
            if state.daily_halt {
                return until_clock(next_utc_midnight(now), now);
            }
            if now < state.retry_until {
                return until_clock(state.retry_until, now);
            }
            if now < state.cooldown_until {
                return until_clock(state.cooldown_until, now);
            }
            let (windows, always) = session_knobs(cfg, state.strategy_id);
            if !in_entry_window(now, Some(&windows), always) {
                if let Some(nxt) = next_window_start(now, &windows) {
                    return until_clock(nxt.timestamp() as f64, now);
                }
            }
            if state.entries_paused {
                return "пока r в торговом TUI".into();
            }
            "ждёт свободный слот".into()
        }
        WaitKind::Setup => {
            if state.strategy_id == 4 {
                s4_setup_until(snapshot, ticker, &s4_params(cfg), now)
            } else if state.strategy_id == 1 {
                if near_24h_high(ticker, Decimal::new(2, 2)) && ticker.high_price > Decimal::ZERO {
                    let cap = ticker.high_price * Decimal::new(98, 2);
                    if ticker.last_price > cap {
                        return format!(
                            "ещё {}% вниз от 24h high",
                            pct_gap((ticker.last_price - cap) / ticker.last_price * Decimal::from(100))
                        );
                    }
                }
                next_bar_until(snapshot, &ticker.symbol, TradeInterval::Minute5, now)
            } else {
                next_bar_until(snapshot, &ticker.symbol, TradeInterval::Minute5, now)
            }
        }
        WaitKind::Ready => {
            let poll = if state.strategy_id == 4 {
                crate::continuation::SCAN_SEC
            } else {
                cfg.poll_seconds.max(1) as f64
            };
            scan_until(state, poll, now)
        }
    }
}

pub fn classify_waiting(
    cfg: &Config,
    state: &EngineState,
    snapshot: &MarketSnapshot,
    positions: &[Position],
    now: f64,
) -> Vec<WaitRow> {
    let held = held_set(positions);
    let gate = global_gate(cfg, state, positions, now);
    let mut rows: Vec<WaitRow> = Vec::new();
    for ticker in candidate_tickers(cfg, state, snapshot) {
        if held.contains(&ticker.symbol.to_ascii_uppercase()) {
            continue;
        }
        if ticker.last_price <= Decimal::ZERO {
            continue;
        }
        let setup = setup_skip(cfg, state, snapshot, &ticker, now);
        let pause = pause_reason(state, &ticker.symbol, now);
        let (kind, reason) = if let Some(why) = pause {
            (WaitKind::Pause, why)
        } else if let Some(why) = setup {
            (WaitKind::Setup, why)
        } else if let Some(why) = gate.clone() {
            (WaitKind::Gate, why)
        } else {
            (WaitKind::Ready, "готов к входу".into())
        };
        let until = until_entry(
            cfg,
            state,
            snapshot,
            &ticker,
            kind,
            now,
        );
        rows.push(WaitRow {
            symbol: ticker.symbol.clone(),
            change_pct: ticker.price_change_percent,
            last: ticker.last_price,
            volume: ticker.quote_volume,
            kind,
            reason,
            until,
        });
    }
    rows.sort_by(|a, b| {
        let by_book = if state.strategy_id == 4 {
            b.volume.cmp(&a.volume)
        } else {
            b.change_pct.cmp(&a.change_pct)
        };
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then(by_book)
            .then(a.symbol.cmp(&b.symbol))
    });
    rows.truncate(MONITOR_WAIT_N);
    rows
}

fn closed_today_rows(events: &[TradeEvent], now: f64) -> (Vec<ClosedRow>, Decimal, usize, usize) {
    let today = utc_day_key(now);
    let mut rows = Vec::new();
    let mut net = Decimal::ZERO;
    let mut wins = 0usize;
    let mut losses = 0usize;
    for ev in events {
        if ev.event != "close" {
            continue;
        }
        let Some(ts) = event_unix(&ev.ts) else {
            continue;
        };
        if utc_day_key(ts) != today {
            continue;
        }
        let pnl = parse_pnl(ev.pnl.as_deref()).unwrap_or(Decimal::ZERO);
        net += pnl;
        if pnl > Decimal::ZERO {
            wins += 1;
        } else if pnl < Decimal::ZERO {
            losses += 1;
        }
        let clock = if ev.ts.len() >= 19 {
            ev.ts[11..19].to_string()
        } else {
            crate::sessions::utc_datetime(ts).format("%H:%M:%S").to_string()
        };
        rows.push(ClosedRow {
            clock,
            symbol: crate::journal::journal_symbol(&ev.symbol),
            pnl,
            reason: ev.reason.clone(),
        });
    }
    let start = rows.len().saturating_sub(MONITOR_CLOSED_N);
    let shown = rows[start..].to_vec();
    (shown, net, wins, losses)
}

fn fmt_remain(seconds: f64) -> String {
    let sec = seconds.max(0.0) as i64;
    if sec < 60 {
        return format!("{sec} с");
    }
    let hours = sec / 3600;
    let mins = (sec % 3600) / 60;
    let rem = sec % 60;
    if hours > 0 {
        if mins == 0 {
            format!("{hours} ч")
        } else {
            format!("{hours} ч {mins} мин")
        }
    } else if rem == 0 {
        format!("{mins} мин")
    } else {
        format!("{mins} мин {rem} с")
    }
}

fn fmt_money(value: Decimal) -> String {
    format!("{:.4}", value.round_dp(4))
}

fn fmt_signed(value: Decimal) -> String {
    let n = value.round_dp(4);
    if n > Decimal::ZERO {
        format!("+{}", fmt_money(n))
    } else {
        fmt_money(n)
    }
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

fn fmt_pct(value: Decimal) -> String {
    let n = value.round_dp(3);
    if n > Decimal::ZERO {
        format!("+{n}%")
    } else {
        format!("{n}%")
    }
}

fn fmt_vol(value: Decimal) -> String {
    if value >= Decimal::from(1_000_000) {
        format!("{}M", (value / Decimal::from(1_000_000)).round_dp(1).normalize())
    } else if value >= Decimal::from(1000) {
        format!("{}k", (value / Decimal::from(1000)).round_dp(0).normalize())
    } else {
        format!("{}", value.round_dp(0).normalize())
    }
}

fn pnl_tag(pnl: Decimal) -> &'static str {
    if pnl > Decimal::ZERO {
        "в плюсе"
    } else if pnl < Decimal::ZERO {
        "в минусе"
    } else {
        "в нуле"
    }
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

fn one_r_text(pos: &Position, mark: Decimal) -> Option<String> {
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

fn growth_tag(
    symbol: &str,
    positions: &[Position],
    waiting: &[WaitRow],
) -> String {
    if let Some(pos) = positions
        .iter()
        .find(|p| p.symbol.eq_ignore_ascii_case(symbol) && p.qty > Decimal::ZERO)
    {
        return format!("[{}]", pnl_tag(pos.unrealized_pnl));
    }
    if let Some(row) = waiting
        .iter()
        .find(|w| w.symbol.eq_ignore_ascii_case(symbol))
    {
        return format!("[{}]", row.kind.tag());
    }
    String::new()
}

fn top_heading(label: &str, shown: usize, total: usize) -> String {
    if total == 0 {
        format!("=== {label} ===")
    } else {
        format!("=== {label} ({shown} из {total}) ===")
    }
}

fn wait_heading(view: &MonitorView) -> String {
    match view.strategy_id {
        4 => "=== В ожидании входа (книга ликвид, не топ 24h) ===".into(),
        1 => "=== В ожидании входа (книга momentum) ===".into(),
        _ => "=== В ожидании входа ===".into(),
    }
}

fn wait_hint(view: &MonitorView) -> &'static str {
    match view.strategy_id {
        4 => "  кого стратегия 4 реально берёт: ликвидный откат, не догон 24h %",
        1 => "  кого momentum берёт из растущих (не вся лента)",
        2 => "  BTC/ETH/SOL — скальп VWAP/EMA9",
        3 => "  BTC/ETH/SOL — тренд Donchian",
        _ => "  кандидаты текущей стратегии",
    }
}

pub fn render_monitor(view: &MonitorView) -> String {
    let cred = if view.has_credentials {
        "keys=env"
    } else {
        "keys=missing"
    };
    let header = format!(
        "home-economic  |  MONITOR  |  Binance USDT-M Futures TestNet  |  WATCH  |  {cred}"
    );
    let equity = current_equity(view.wallet_balance, view.unrealized_pnl);
    let open_n = view
        .positions
        .iter()
        .filter(|p| p.qty > Decimal::ZERO)
        .count();
    let profit_n = view
        .positions
        .iter()
        .filter(|p| p.qty > Decimal::ZERO && p.unrealized_pnl > Decimal::ZERO)
        .count();
    let loss_n = view
        .positions
        .iter()
        .filter(|p| p.qty > Decimal::ZERO && p.unrealized_pnl < Decimal::ZERO)
        .count();
    let ready_n = view
        .waiting
        .iter()
        .filter(|w| w.kind == WaitKind::Ready)
        .count();
    let wait_n = view.waiting.len();
    let day = view
        .day_pnl
        .map(|v| format!("{} USDT", fmt_signed(v)))
        .unwrap_or_else(|| "—".into());
    let title = strategy_title(view.strategy_id);
    let mut session = format!(
        "Текущая: {} — {}  |  сейчас {} UTC  |  входы {}",
        view.strategy_id,
        title,
        crate::sessions::utc_datetime(view.now_ts).format("%H:%M"),
        if view.always_enter {
            "круглосуточно".into()
        } else {
            format_windows(&view.entry_windows)
        }
    );
    if view.strategy_id == 4 {
        session.push_str(&format!(
            "  |  свечи {}  |  {}",
            view.s4_interval.as_ru(),
            view.s4_interval.geometry_ru()
        ));
    }
    if !view.session_open {
        if let Some(nxt) = &view.next_open_clock {
            session.push_str(&format!("  |  следующий старт {nxt} UTC"));
        }
    }
    if !view.always_enter {
        session.push_str(&format!("  |  {}", view.session_label));
    }

    let mut summary = vec![
        "=== Сводка ===".to_string(),
        session,
        format!(
            "Счёт: {} USDT  |  Нереализованный PnL: {} USDT  |  Прибыль счета: {} USDT",
            fmt_money(equity),
            fmt_signed(view.unrealized_pnl),
            fmt_signed(view.account_profit)
        ),
        format!(
            "день: {day}  |  открыто {open_n}/{} ({} в плюсе / {} в минусе)  |  готовы {ready_n}  |  ждут {wait_n}",
            view.max_positions.max(1),
            profit_n,
            loss_n
        ),
    ];
    if view.daily_halt {
        summary.push("Стоп дня: новых входов нет до 00:00 UTC.".into());
    }

    let mut pos_lines = vec!["=== Открытые позиции (прибыль / убыток) ===".to_string()];
    if view.positions.is_empty() {
        pos_lines.push("(нет открытых позиций)".into());
    } else {
        for pos in &view.positions {
            let mark = position_mark(pos, &view.tickers);
            let notional = pos.qty * pos.entry_price;
            let pct = if notional > Decimal::ZERO {
                pos.unrealized_pnl / notional * Decimal::from(100)
            } else {
                Decimal::ZERO
            };
            let sl = pos.stop_loss.map(fmt_price).unwrap_or_else(|| "—".into());
            let tp = pos.take_profit.map(fmt_price).unwrap_or_else(|| "—".into());
            pos_lines.push(format!(
                "{} {} qty={} entry={} last={} uPnL={} ({})  SL={sl} TP={tp}  [{}]",
                pos.symbol,
                pos.side,
                fmt_money(pos.qty),
                fmt_price(pos.entry_price),
                fmt_price(mark),
                fmt_signed(pos.unrealized_pnl),
                fmt_pct(pct),
                pnl_tag(pos.unrealized_pnl)
            ));
            if let Some(line) = one_r_text(pos, mark) {
                pos_lines.push(line);
            }
        }
    }

    let mut wait_lines = vec![wait_heading(view)];
    wait_lines.push(wait_hint(view).into());
    if view.waiting.is_empty() {
        wait_lines.push("(нет кандидатов на вход)".into());
    } else {
        for row in &view.waiting {
            wait_lines.push(format!(
                "  {:12} {:>9}  last={}  vol={:<6}  [{}]  {}",
                row.symbol,
                fmt_pct(row.change_pct),
                fmt_price(row.last),
                fmt_vol(row.volume),
                row.kind.tag(),
                row.reason
            ));
            wait_lines.push(format!("    до входа: {}", row.until));
        }
    }

    let mut tape = vec![top_heading("Топ роста 24h", view.rising.len(), view.tape_n)];
    tape.push("  лента рынка — не список покупок".into());
    if view.rising.is_empty() {
        tape.push("  (нет тикеров)".into());
    } else {
        for t in &view.rising {
            let tag = growth_tag(&t.symbol, &view.positions, &view.waiting);
            tape.push(format!(
                "  {:12} {:>9}  last={}  vol={:<6}  {}",
                t.symbol,
                fmt_pct(t.price_change_percent),
                fmt_price(t.last_price),
                fmt_vol(t.quote_volume),
                tag
            ));
        }
    }
    tape.push(String::new());
    tape.push(top_heading("Топ падения", view.falling.len(), view.tape_n));
    if view.falling.is_empty() {
        tape.push("  (нет тикеров)".into());
    } else {
        for t in &view.falling {
            let tag = growth_tag(&t.symbol, &view.positions, &view.waiting);
            tape.push(format!(
                "  {:12} {:>9}  last={}  vol={:<6}  {}",
                t.symbol,
                fmt_pct(t.price_change_percent),
                fmt_price(t.last_price),
                fmt_vol(t.quote_volume),
                tag
            ));
        }
    }

    let mut closed = vec![format!(
        "=== Закрытые сегодня ({}W/{}L  нетто={}) ===",
        view.closed_wins,
        view.closed_losses,
        fmt_signed(view.closed_net)
    )];
    if view.closed_today.is_empty() {
        closed.push("  (сегодня закрытий нет)".into());
    } else {
        for row in &view.closed_today {
            closed.push(format!(
                "  {}  {:12}  нетто={}  ({})",
                row.clock,
                row.symbol,
                fmt_signed(row.pnl),
                row.reason
            ));
        }
    }

    let mut blocks = vec!["=== Блоки ===".to_string()];
    let cool = cooldown_lines(view.now_ts, view.cooldown_until, &view.cooldowns);
    if cool.is_empty() && !view.daily_halt && view.session_open {
        blocks.push("  (нет пауз)".into());
    } else {
        blocks.extend(cool);
        if view.daily_halt {
            blocks.push("  • стоп дня".into());
        }
        if !view.session_open {
            blocks.push(format!("  • {}", view.session_label));
        }
    }

    let footer = [
        "MONITOR: ордера не отправляются (можно держать рядом с --live).".to_string(),
        "Клавиши: 1/2/3/4 линза стратегии  |  r обновить  |  q выход".to_string(),
        "Логи сделок: .state/trades.jsonl".to_string(),
    ];

    let mut out = vec![header, String::new()];
    out.extend(summary);
    out.push(String::new());
    out.extend(pos_lines);
    out.push(String::new());
    out.extend(wait_lines);
    out.push(String::new());
    out.extend(tape);
    out.push(String::new());
    out.extend(closed);
    out.push(String::new());
    out.extend(blocks);
    out.push(String::new());
    out.extend(footer);
    if let Some(err) = &view.last_error {
        out.push(crate::errorlog::format_ui_error(err));
    }
    format!("{}\n", out.join("\n"))
}



//! Interactive TUI loop. Keys: 1/2/3/4 strategy, r refresh, q quit, x then x flatten.
//!
//! Snapshot HTTP runs on a side thread (Python `SnapshotPoller`). Tick / live
//! apply stay on this thread and serialize on the same client mutex as the pull.

use crate::config::Config;
use crate::dayrisk::apply_day_risk;
use crate::engine::{tick_decisions, MomentumParams};
use crate::errorlog::{note_frame as note_error_frame, set_active as set_error_log, ErrorLog};
use crate::errors::COOLDOWN_SEC;
use crate::exchange::{BinanceFutures, LiveClient, SnapshotClient};
use crate::journal::{seed_cooldowns, set_active as set_journal, TradeJournal, DEFAULT_JOURNAL_PATH};
use crate::keys::{handle_key, KeyAction};
use crate::live::{apply_decision, apply_flatten, apply_paper_decision, reconcile_live, LiveApplyResult};
use crate::models::{unmanaged_positions, Decision, EngineState, MarketSnapshot};
use crate::monitor::{build_monitor, render_monitor};
use crate::pidlock::acquire_live_lock;
use crate::poll::{Pulled, SnapshotPoller};
use crate::profit::{current_equity, EquityPin};
use crate::render::{account_profit_figure, fit_lines, line_tone, render_frame, LineTone, ViewModel};
use crate::signals::{emit_decision, reason_suggests_win, set_enabled, shutdown as shutdown_signals};
use crate::snapshot::{apply_tradfi_skip, fetch_snapshot, make_client, pull_snapshot};
use crate::view::{build_view, view_positions};
use rust_decimal::Decimal;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, DisableLineWrap, EnableLineWrap,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute, queue};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Python `SnapshotPoller` interval. Do not tick/apply on the 200ms key poll.
const SNAPSHOT_INTERVAL_SECS: f64 = 5.0;
/// No successful pull for this long → footer warning (poller hung / panicking).
pub const SNAPSHOT_STALE_SECS: f64 = 30.0;
const SNAPSHOT_STALE_MSG: &str = "сеть: снимок рынка завис";

fn now() -> f64 {
    crate::sessions::unix_now()
}

fn lock_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
struct PollInput {
    state: EngineState,
    prior: MarketSnapshot,
    pin_value: Option<rust_decimal::Decimal>,
}

fn publish_poll(
    slot: &Mutex<PollInput>,
    state: &EngineState,
    snapshot: &MarketSnapshot,
    pin: &EquityPin,
) {
    let mut g = lock_poison(slot);
    g.state = state.clone();
    g.prior = snapshot.clone();
    g.pin_value = pin.value;
}

fn pull_locked(
    cfg: &Config,
    client: &Mutex<Option<BinanceFutures>>,
    state: &mut EngineState,
    pin: &mut EquityPin,
    offline: bool,
    prior: Option<&MarketSnapshot>,
) -> MarketSnapshot {
    let mut g = lock_poison(client);
    pull_snapshot(
        cfg,
        g.as_mut().map(|c| c as &mut dyn SnapshotClient),
        state,
        pin,
        offline,
        prior,
    )
}

fn with_live<R>(
    client: &Mutex<Option<BinanceFutures>>,
    f: impl FnOnce(&mut dyn LiveClient) -> R,
) -> Option<R> {
    let mut g = lock_poison(client);
    g.as_mut().map(|c| f(c as &mut dyn LiveClient))
}

/// True when the tape (tickers / «Топ роста») must be re-fetched.
/// Independent of the 200ms key poll so resize/mouse/keys cannot freeze the board.
pub fn snapshot_due(now: f64, last_at: f64) -> bool {
    now - last_at >= SNAPSHOT_INTERVAL_SECS
}

pub fn snapshot_stale(now: f64, last_at: f64) -> bool {
    now - last_at >= SNAPSHOT_STALE_SECS
}

fn scan_once(
    cfg: &Config,
    client: &Mutex<Option<BinanceFutures>>,
    state: &mut EngineState,
    snapshot: &MarketSnapshot,
    last_text: &mut String,
    momentum: &MomentumParams,
) {
    if cfg.live {
        if let Some(rec) = with_live(client, |c| reconcile_live(cfg, c, state, snapshot, Some(now()))) {
            if rec.skip_tick {
                if !rec.last_text.is_empty() {
                    *last_text = rec.last_text;
                }
                return;
            }
            if !rec.last_text.is_empty() {
                *last_text = rec.last_text;
            }
        }
    }
    let (new_state, decisions) =
        tick_decisions(state, snapshot, now(), Some(momentum), None, None);
    *state = new_state;
    *last_text = decisions
        .first()
        .map(|d| d.reason().to_string())
        .unwrap_or_else(|| "—".into());
    if cfg.live {
        if let Some(()) = with_live(client, |c| {
            for d in &decisions {
                apply_decision(cfg, c, state, snapshot, d);
            }
        }) {
            return;
        }
    } else {
        for d in &decisions {
            apply_paper_decision(state, snapshot, d);
        }
    }
    let has_pos = snapshot.position.is_some()
        || !state.positions.is_empty()
        || snapshot
            .open_positions
            .iter()
            .any(|p| p.qty > rust_decimal::Decimal::ZERO);
    if state.strategy_id == 4 {
        crate::s4stats::flush_s4_skip_stats();
    }
    for d in &decisions {
        let won = if let Decision::ExitPosition { reason, symbol } = d {
            if reason_suggests_win(reason) {
                Some(true)
            } else {
                snapshot
                    .open_positions
                    .iter()
                    .chain(snapshot.position.iter())
                    .chain(state.positions.iter())
                    .find(|p| {
                        p.symbol.eq_ignore_ascii_case(symbol)
                            && p.qty > rust_decimal::Decimal::ZERO
                    })
                    .map(|p| p.unrealized_pnl > rust_decimal::Decimal::ZERO)
            }
        } else {
            None
        };
        emit_decision(d, &LiveApplyResult::default(), false, has_pos, won);
    }
}

/// Always leave the user's terminal usable, even on panic after raw mode.
struct TerminalGuard {
    restored: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(e) = execute!(
            stdout,
            EnterAlternateScreen,
            DisableLineWrap,
            cursor::Hide
        ) {
            let _ = disable_raw_mode();
            return Err(e);
        }
        Ok(Self { restored: false })
    }

    fn restore(&mut self) {
        if self.restored {
            return;
        }
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            EnableLineWrap,
            cursor::Show,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        self.restored = true;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

fn paint_frame(stdout: &mut impl Write, frame: &str, profit: Decimal) -> io::Result<()> {
    let (cols, rows) = crossterm::terminal::size()?;
    let width = (cols.saturating_sub(1) as usize).max(1);
    let lines = fit_lines(frame, width, rows as usize);
    execute!(stdout, Clear(ClearType::All))?;
    for (i, line) in lines.iter().enumerate() {
        queue!(stdout, cursor::MoveTo(0, i as u16))?;
        match line_tone(line, profit) {
            Some(LineTone::Profit) => {
                queue!(stdout, SetForegroundColor(Color::Green))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            }
            Some(LineTone::Loss) => {
                queue!(stdout, SetForegroundColor(Color::Red))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            }
            Some(LineTone::Warn) => {
                queue!(stdout, SetForegroundColor(Color::Yellow))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            }
            None => write!(stdout, "{line}")?,
        }
    }
    stdout.flush()
}

fn paint(stdout: &mut impl Write, view: &ViewModel, frame: &str) -> io::Result<()> {
    paint_frame(stdout, frame, account_profit_figure(view))
}

fn spawn_poller(
    cfg: &Config,
    client: Arc<Mutex<Option<BinanceFutures>>>,
    poll_in: Arc<Mutex<PollInput>>,
    offline: bool,
) -> Option<SnapshotPoller<MarketSnapshot>> {
    let cfg_p = cfg.clone();
    SnapshotPoller::start(
        Duration::from_secs_f64(SNAPSHOT_INTERVAL_SECS),
        move || {
            let input = lock_poison(&poll_in).clone();
            let mut st = input.state;
            let mut g = lock_poison(&client);
            let tradfi = if offline {
                Vec::new()
            } else {
                g.as_mut()
                    .and_then(|c| c.tradfi_symbols().ok())
                    .unwrap_or_default()
            };
            apply_tradfi_skip(&mut st, &tradfi);
            let overlay = st.positions.clone();
            let snap = fetch_snapshot(
                &cfg_p,
                g.as_mut().map(|c| c as &mut dyn SnapshotClient),
                &st,
                offline,
                input.pin_value,
                Some(&input.prior),
                None,
                &[],
                &overlay,
            );
            Pulled {
                snapshot: snap,
                tradfi,
            }
        },
    )
    .ok()
}

pub fn run_tui(cfg: &Config, state: &mut EngineState, offline: bool) -> io::Result<()> {
    let _live_lock = if cfg.live && !offline {
        Some(acquire_live_lock(None)?)
    } else {
        None
    };
    set_enabled(true);
    set_journal(Some(std::path::PathBuf::from(DEFAULT_JOURNAL_PATH)));
    set_error_log(Some(ErrorLog::new(None)));
    seed_cooldowns(state, now(), COOLDOWN_SEC);
    let mut last_text = "—".to_string();
    let mut pin = EquityPin::from_config(cfg.starting_equity);
    let client = Arc::new(Mutex::new(if offline {
        None
    } else {
        Some(make_client(cfg))
    }));
    let momentum = MomentumParams {
        poll_seconds: cfg.poll_seconds,
        tp_pct: cfg.tp_pct,
        trail_pct: cfg.trail_pct,
        entry_windows: cfg.entry_windows.clone(),
        always_enter: cfg.always_enter,
        s4_entry_windows: cfg.s4_entry_windows.clone(),
        s4_always_enter: cfg.s4_always_enter,
        s4_interval: cfg.s4_interval,
        s4_max_positions: cfg.s4_max_positions,
        max_positions: cfg.max_positions,
        daily_loss_usdt: cfg.daily_loss_usdt,
        daily_loss_r: cfg.daily_loss_r,
        risk_pct: cfg.risk_pct,
        ..MomentumParams::default()
    };
    // First REST happens before raw mode so Ctrl+C is still SIGINT.
    let mut snapshot = pull_locked(cfg, &client, state, &mut pin, offline, None);
    if cfg.live {
        let skip = with_live(&client, |c| {
            let rec = reconcile_live(cfg, c, state, &snapshot, Some(now()));
            if rec.skip_tick || !rec.last_text.is_empty() {
                last_text = rec.last_text.clone();
            }
            rec.skip_tick
        })
        .unwrap_or(false);
        if skip {
            snapshot = pull_locked(cfg, &client, state, &mut pin, offline, Some(&snapshot));
        }
    }
    let mut term = TerminalGuard::enter()?;
    let mut stdout = io::stdout();
    let mut flatten_armed = false;

    let poll_in = Arc::new(Mutex::new(PollInput {
        state: state.clone(),
        prior: snapshot.clone(),
        pin_value: pin.value,
    }));
    let poller = spawn_poller(cfg, client.clone(), poll_in.clone(), offline);
    if let Some(p) = poller.as_ref() {
        p.bump();
    }
    scan_once(cfg, &client, state, &snapshot, &mut last_text, &momentum);
    publish_poll(&poll_in, state, &snapshot, &pin);
    let mut last_snap_at = now();

    let result = (|| -> io::Result<()> {
        loop {
            if let Some(pulled) = poller.as_ref().and_then(|p| p.take()) {
                apply_tradfi_skip(state, &pulled.tradfi);
                snapshot = pulled.snapshot;
                if snapshot.live_book && snapshot.account_fresh {
                    pin.capture(snapshot.account.starting_equity);
                }
                if state.last_error.as_deref() == Some(SNAPSHOT_STALE_MSG) {
                    state.last_error = None;
                }
                scan_once(cfg, &client, state, &snapshot, &mut last_text, &momentum);
                publish_poll(&poll_in, state, &snapshot, &pin);
                last_snap_at = now();
            } else if poller.is_none() && snapshot_due(now(), last_snap_at) {
                last_snap_at = now();
                snapshot = pull_locked(cfg, &client, state, &mut pin, offline, Some(&snapshot));
                scan_once(cfg, &client, state, &snapshot, &mut last_text, &momentum);
                publish_poll(&poll_in, state, &snapshot, &pin);
            } else if poller.is_some() && snapshot_stale(now(), last_snap_at) && state.last_error.is_none() {
                state.last_error = Some(SNAPSHOT_STALE_MSG.into());
            }

            let view = build_view(cfg, state, &snapshot, &last_text, flatten_armed);
            note_error_frame(
                view.logged_error.as_deref(),
                view.strategy_id,
                view.live,
                &view.chart_symbol,
                &view.error_source,
            );
            let frame = render_frame(&view);
            paint(&mut stdout, &view, &frame)?;

            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                break;
            }
            let ch = match key.code {
                KeyCode::Char(c) => c,
                KeyCode::Esc => 'q',
                _ => '\0',
            };
            match handle_key(ch, flatten_armed) {
                KeyAction::Quit => break,
                KeyAction::Strategy(id) => {
                    state.strategy_id = id;
                    flatten_armed = false;
                    publish_poll(&poll_in, state, &snapshot, &pin);
                }
                KeyAction::Refresh => {
                    state.entries_paused = false;
                    state.last_error = None;
                    flatten_armed = false;
                    publish_poll(&poll_in, state, &snapshot, &pin);
                    if let Some(p) = poller.as_ref() {
                        p.bump();
                    } else {
                        last_snap_at = 0.0;
                    }
                }
                KeyAction::FlattenArm => flatten_armed = true,
                KeyAction::FlattenConfirm => {
                    flatten_armed = false;
                    let tail = unmanaged_positions(&view_positions(&snapshot), &state.positions);
                    let _ = with_live(&client, |c| {
                        let targets = if tail.is_empty() { None } else { Some(tail) };
                        apply_flatten(cfg, c, state, Some(&snapshot), targets.as_deref());
                    });
                    last_text = state
                        .recent_actions
                        .last()
                        .map(|a| a.text.clone())
                        .unwrap_or_else(|| "FLAT".into());
                    if let Some(p) = poller.as_ref() {
                        let _ = p.take();
                        publish_poll(&poll_in, state, &snapshot, &pin);
                        p.bump();
                    } else {
                        publish_poll(&poll_in, state, &snapshot, &pin);
                        last_snap_at = 0.0;
                    }
                }
                KeyAction::FlattenCancel => flatten_armed = false,
                KeyAction::Ignore => {}
            }
        }
        Ok(())
    })();

    term.restore();
    if let Some(mut p) = poller {
        p.stop();
    }
    shutdown_signals();
    set_journal(None);
    set_error_log(None);
    result
}

fn refresh_day_risk(cfg: &Config, state: &mut EngineState, snapshot: &MarketSnapshot, now_ts: f64) {
    let equity = current_equity(
        snapshot.account.wallet_balance,
        snapshot.account.unrealized_pnl,
    );
    apply_day_risk(
        state,
        now_ts,
        equity,
        cfg.daily_loss_usdt,
        cfg.daily_loss_r,
        cfg.risk_pct,
    );
}

/// Watch-only radar. No live.lock, no orders, no flatten.
pub fn run_monitor(cfg: &Config, state: &mut EngineState, offline: bool) -> io::Result<()> {
    set_journal(Some(std::path::PathBuf::from(DEFAULT_JOURNAL_PATH)));
    seed_cooldowns(state, now(), COOLDOWN_SEC);
    let mut pin = EquityPin::from_config(cfg.starting_equity);
    let client = Arc::new(Mutex::new(if offline {
        None
    } else {
        Some(make_client(cfg))
    }));
    let mut snapshot = pull_locked(cfg, &client, state, &mut pin, offline, None);
    refresh_day_risk(cfg, state, &snapshot, now());
    let mut term = TerminalGuard::enter()?;
    let mut stdout = io::stdout();

    let poll_in = Arc::new(Mutex::new(PollInput {
        state: state.clone(),
        prior: snapshot.clone(),
        pin_value: pin.value,
    }));
    let poller = spawn_poller(cfg, client.clone(), poll_in.clone(), offline);
    if let Some(p) = poller.as_ref() {
        p.bump();
    }
    publish_poll(&poll_in, state, &snapshot, &pin);
    let mut last_snap_at = now();

    let result = (|| -> io::Result<()> {
        loop {
            if let Some(pulled) = poller.as_ref().and_then(|p| p.take()) {
                apply_tradfi_skip(state, &pulled.tradfi);
                snapshot = pulled.snapshot;
                if snapshot.live_book && snapshot.account_fresh {
                    pin.capture(snapshot.account.starting_equity);
                }
                if state.last_error.as_deref() == Some(SNAPSHOT_STALE_MSG) {
                    state.last_error = None;
                }
                refresh_day_risk(cfg, state, &snapshot, now());
                publish_poll(&poll_in, state, &snapshot, &pin);
                last_snap_at = now();
            } else if poller.is_none() && snapshot_due(now(), last_snap_at) {
                last_snap_at = now();
                snapshot = pull_locked(cfg, &client, state, &mut pin, offline, Some(&snapshot));
                refresh_day_risk(cfg, state, &snapshot, now());
                publish_poll(&poll_in, state, &snapshot, &pin);
            } else if poller.is_some()
                && snapshot_stale(now(), last_snap_at)
                && state.last_error.is_none()
            {
                state.last_error = Some(SNAPSHOT_STALE_MSG.into());
            }

            let events = TradeJournal::new(Some(std::path::Path::new(DEFAULT_JOURNAL_PATH))).read_events();
            let view = build_monitor(cfg, state, &snapshot, &events, now());
            let frame = render_monitor(&view);
            paint_frame(&mut stdout, &frame, view.account_profit)?;

            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                break;
            }
            let ch = match key.code {
                KeyCode::Char(c) => c,
                KeyCode::Esc => 'q',
                _ => '\0',
            };
            match handle_key(ch, false) {
                KeyAction::Quit => break,
                KeyAction::Strategy(id) => {
                    state.strategy_id = id;
                    publish_poll(&poll_in, state, &snapshot, &pin);
                    if let Some(p) = poller.as_ref() {
                        p.bump();
                    } else {
                        last_snap_at = 0.0;
                    }
                }
                KeyAction::Refresh => {
                    state.last_error = None;
                    publish_poll(&poll_in, state, &snapshot, &pin);
                    if let Some(p) = poller.as_ref() {
                        p.bump();
                    } else {
                        last_snap_at = 0.0;
                    }
                }
                KeyAction::FlattenArm
                | KeyAction::FlattenConfirm
                | KeyAction::FlattenCancel
                | KeyAction::Ignore => {}
            }
        }
        Ok(())
    })();

    term.restore();
    if let Some(mut p) = poller {
        p.stop();
    }
    set_journal(None);
    result
}

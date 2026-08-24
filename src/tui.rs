//! Interactive TUI loop. Keys: 1/2/3/4 strategy, r refresh, q quit, x then x flatten.

use crate::config::Config;
use crate::engine::{tick_decisions, MomentumParams};
use crate::keys::{handle_key, KeyAction};
use crate::errors::COOLDOWN_SEC;
use crate::journal::{seed_cooldowns, set_active as set_journal, DEFAULT_JOURNAL_PATH};
use crate::live::{apply_decision, apply_flatten, reconcile_live, LiveApplyResult};
use crate::models::{unmanaged_positions, EngineState};
use crate::signals::{emit_decision, set_enabled};
use crate::profit::EquityPin;
use crate::render::{account_profit_figure, fit_lines, line_tone, render_frame, LineTone, ViewModel};
use crate::snapshot::{make_client, pull_snapshot};
use crate::view::{build_view, view_positions};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, DisableLineWrap, EnableLineWrap,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute, queue};
use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Python `SnapshotPoller` interval. Do not tick/apply on the 200ms key poll.
const SNAPSHOT_INTERVAL_SECS: f64 = 5.0;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn paint(stdout: &mut impl Write, view: &ViewModel, frame: &str) -> io::Result<()> {
    let (cols, rows) = crossterm::terminal::size()?;
    let width = (cols.saturating_sub(1) as usize).max(1);
    let lines = fit_lines(frame, width, rows as usize);
    let profit = account_profit_figure(view);
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
            None => write!(stdout, "{line}")?,
        }
    }
    stdout.flush()
}

pub fn curses_loop(cfg: &Config, state: &mut EngineState, offline: bool) -> io::Result<()> {
    set_enabled(true);
    set_journal(Some(std::path::PathBuf::from(DEFAULT_JOURNAL_PATH)));
    seed_cooldowns(state, now(), COOLDOWN_SEC);
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        DisableLineWrap,
        cursor::Hide
    )?;
    let mut flatten_armed = false;
    let mut last_text = "—".to_string();
    let mut pin = EquityPin::from_config(cfg.starting_equity);
    let mut client = if offline { None } else { Some(make_client(cfg)) };
    let momentum = MomentumParams {
        poll_seconds: cfg.poll_seconds,
        tp_pct: cfg.tp_pct,
        trail_pct: cfg.trail_pct,
        entry_windows: cfg.entry_windows.clone(),
        always_enter: cfg.always_enter,
        s4_entry_windows: cfg.s4_entry_windows.clone(),
        s4_always_enter: cfg.s4_always_enter,
        max_positions: cfg.max_positions,
        daily_loss_usdt: cfg.daily_loss_usdt,
        ..MomentumParams::default()
    };
    let mut snapshot = pull_snapshot(
        cfg,
        client.as_mut().map(|c| c as &mut dyn crate::exchange::SnapshotClient),
        state,
        &mut pin,
        offline,
        None,
    );
    if cfg.live {
        let skip = if let Some(c) = client.as_mut() {
            let rec = reconcile_live(cfg, c, state, &snapshot, Some(now()));
            if rec.skip_tick || !rec.last_text.is_empty() {
                last_text = rec.last_text;
            }
            rec.skip_tick
        } else {
            false
        };
        if skip {
            snapshot = pull_snapshot(
                cfg,
                client.as_mut().map(|c| c as &mut dyn crate::exchange::SnapshotClient),
                state,
                &mut pin,
                offline,
                Some(&snapshot),
            );
        }
    }
    let mut last_snap_at = now();

    let result = (|| -> io::Result<()> {
        loop {
            let view = build_view(cfg, state, &snapshot, &last_text, flatten_armed);
            let frame = render_frame(&view);
            paint(&mut stdout, &view, &frame)?;

            if event::poll(Duration::from_millis(200))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
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
                        }
                        KeyAction::Refresh => {
                            state.entries_paused = false;
                            state.last_error = None;
                            flatten_armed = false;
                            snapshot = pull_snapshot(
                                cfg,
                                client.as_mut().map(|c| c as &mut dyn crate::exchange::SnapshotClient),
                                state,
                                &mut pin,
                                offline,
                                Some(&snapshot),
                            );
                            last_snap_at = now();
                        }
                        KeyAction::FlattenArm => flatten_armed = true,
                        KeyAction::FlattenConfirm => {
                            flatten_armed = false;
                            let tail = unmanaged_positions(&view_positions(&snapshot), &state.positions);
                            if let Some(c) = client.as_mut() {
                                let targets = if tail.is_empty() { None } else { Some(tail) };
                                apply_flatten(cfg, c, state, Some(&snapshot), targets.as_deref());
                            }
                            last_text = state
                                .recent_actions
                                .last()
                                .cloned()
                                .unwrap_or_else(|| "FLAT".into());
                            snapshot = pull_snapshot(
                                cfg,
                                client.as_mut().map(|c| c as &mut dyn crate::exchange::SnapshotClient),
                                state,
                                &mut pin,
                                offline,
                                Some(&snapshot),
                            );
                            last_snap_at = now();
                        }
                        KeyAction::FlattenCancel => flatten_armed = false,
                        KeyAction::Ignore => {}
                    }
                }
            } else {
                let t = now();
                if t - last_snap_at < SNAPSHOT_INTERVAL_SECS {
                    continue;
                }
                last_snap_at = t;
                snapshot = pull_snapshot(
                    cfg,
                    client.as_mut().map(|c| c as &mut dyn crate::exchange::SnapshotClient),
                    state,
                    &mut pin,
                    offline,
                    Some(&snapshot),
                );
                if cfg.live {
                    if let Some(c) = client.as_mut() {
                        let rec = reconcile_live(cfg, c, state, &snapshot, Some(now()));
                        if rec.skip_tick {
                            last_text = rec.last_text;
                            continue;
                        }
                        if !rec.last_text.is_empty() {
                            last_text = rec.last_text;
                        }
                    }
                }
                let (new_state, decisions) =
                    tick_decisions(state, &snapshot, now(), Some(&momentum), None, None);
                *state = new_state;
                last_text = decisions
                    .first()
                    .map(|d| d.reason().to_string())
                    .unwrap_or_else(|| "—".into());
                if cfg.live {
                    if let Some(c) = client.as_mut() {
                        for d in &decisions {
                            apply_decision(cfg, c, state, &snapshot, d);
                        }
                    }
                } else {
                    let has_pos = snapshot.position.is_some()
                        || !state.positions.is_empty()
                        || snapshot.open_positions.iter().any(|p| p.qty > rust_decimal::Decimal::ZERO);
                    for d in &decisions {
                        emit_decision(d, &LiveApplyResult::default(), false, has_pos);
                    }
                }
            }
        }
        Ok(())
    })();

    let _ = execute!(
        stdout,
        EnableLineWrap,
        cursor::Show,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
    set_journal(None);
    result
}

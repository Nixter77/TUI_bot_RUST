//! Distinct buy / win-close / loss-close chimes. Silent unless the TUI enables them.

use crate::flatten::FlattenResult;
use crate::live::LiveApplyResult;
use crate::models::Decision;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeSignal {
    Buy,
    SellWin,
    SellLoss,
}

/// Rising pair (buy) vs three rising (win exit) vs three falling (loss exit).
pub const BUY_HZ: (f64, f64) = (880.0, 1175.0);
pub const SELL_WIN_HZ: (f64, f64, f64) = (784.0, 988.0, 1318.5);
pub const SELL_LOSS_HZ: (f64, f64, f64) = (392.0, 311.1, 246.9);

static ENABLED: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<Arc<dyn Fn(TradeSignal) + Send + Sync>>> = Mutex::new(None);
static WAVS: Mutex<Option<(PathBuf, PathBuf, PathBuf)>> = Mutex::new(None);
static PLAYER: Mutex<Option<Child>> = Mutex::new(None);

fn lock_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn set_sink(sink: Option<Arc<dyn Fn(TradeSignal) + Send + Sync>>) {
    *lock_poison(&SINK) = sink;
}

pub fn signals_enabled() -> bool {
    let raw = std::env::var("TRADER_SIGNALS").unwrap_or_default();
    let raw = raw.trim().to_ascii_lowercase();
    if matches!(raw.as_str(), "0" | "false" | "off" | "no") {
        return false;
    }
    if matches!(raw.as_str(), "1" | "true" | "on" | "yes") {
        return true;
    }
    ENABLED.load(Ordering::Relaxed)
}

/// Take-profit / break-even wording — used when the caller did not pass `won`.
pub fn reason_suggests_win(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    let normalized = lower.replace(['-', '_'], " ");
    if normalized.contains("take profit")
        || lower.contains("безубыток")
        || lower.contains("частичная фиксация")
    {
        return true;
    }
    normalized
        .split(|c: char| !c.is_alphanumeric())
        .any(|tok| tok == "tp")
}

fn close_kind(won: Option<bool>, reason: &str) -> TradeSignal {
    match won {
        Some(true) => TradeSignal::SellWin,
        Some(false) => TradeSignal::SellLoss,
        None => {
            if reason_suggests_win(reason) {
                TradeSignal::SellWin
            } else {
                TradeSignal::SellLoss
            }
        }
    }
}

pub fn kind_for_decision(
    decision: &Decision,
    result: &LiveApplyResult,
    live: bool,
    has_position: bool,
    won: Option<bool>,
) -> Option<TradeSignal> {
    match decision {
        Decision::Hold { .. } => None,
        Decision::EnterLong { .. } => {
            if result.filled {
                Some(TradeSignal::Buy)
            } else if !live && result.error.is_none() {
                Some(TradeSignal::Buy)
            } else {
                None
            }
        }
        Decision::ExitPosition { reason, .. } => {
            if result.error.is_some() {
                None
            } else if live && !has_position {
                None
            } else {
                Some(close_kind(won, reason))
            }
        }
        Decision::AmendStop { .. } => None,
        Decision::ReduceLong { reason, .. } => {
            if result.error.is_some() {
                None
            } else if live && !result.filled {
                None
            } else {
                Some(close_kind(won.or(Some(true)), reason))
            }
        }
    }
}

pub fn kind_for_flatten(result: &FlattenResult, won: Option<bool>) -> Option<TradeSignal> {
    if result.closed.is_empty() {
        None
    } else if won == Some(true) {
        Some(TradeSignal::SellWin)
    } else {
        Some(TradeSignal::SellLoss)
    }
}

pub fn write_chime(path: &Path, freqs: &[f64], sample_rate: u32) -> std::io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut pcm: Vec<i16> = Vec::new();
    let gap_n = (sample_rate as f64 * 0.04) as usize;
    for (i, freq) in freqs.iter().enumerate() {
        let ms = if i == 0 { 80 } else { 120 };
        let n = ((sample_rate as u64 * ms) / 1000).max(1) as usize;
        let fade = 90.min(n / 4);
        for k in 0..n {
            let env = if k < fade {
                k as f64 / fade as f64
            } else if k > n - fade {
                (n - k) as f64 / fade as f64
            } else {
                1.0
            };
            let sample = (32767.0
                * 0.38
                * env
                * (2.0 * std::f64::consts::PI * freq * k as f64 / sample_rate as f64).sin())
                as i16;
            pcm.push(sample);
        }
        pcm.extend(std::iter::repeat(0i16).take(gap_n));
    }
    write_wav(path, &pcm, sample_rate)?;
    Ok(path.to_path_buf())
}

fn write_wav(path: &Path, pcm: &[i16], sample_rate: u32) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    let data_bytes = (pcm.len() * 2) as u32;
    let file_size = 36 + data_bytes;
    f.write_all(b"RIFF")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&(sample_rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for s in pcm {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

pub fn chime_paths() -> std::io::Result<(PathBuf, PathBuf, PathBuf)> {
    let mut guard = lock_poison(&WAVS);
    if let Some(triple) = guard.as_ref() {
        return Ok(triple.clone());
    }
    let root = std::env::temp_dir().join("home-economic-signals");
    let buy = write_chime(&root.join("buy.wav"), &[BUY_HZ.0, BUY_HZ.1], 22_050)?;
    let sell_win = write_chime(
        &root.join("sell_win.wav"),
        &[SELL_WIN_HZ.0, SELL_WIN_HZ.1, SELL_WIN_HZ.2],
        22_050,
    )?;
    let sell_loss = write_chime(
        &root.join("sell_loss.wav"),
        &[SELL_LOSS_HZ.0, SELL_LOSS_HZ.1, SELL_LOSS_HZ.2],
        22_050,
    )?;
    *guard = Some((buy.clone(), sell_win.clone(), sell_loss.clone()));
    Ok((buy, sell_win, sell_loss))
}

fn player_cmd(path: &Path) -> Option<Command> {
    if cfg!(target_os = "macos") {
        let mut cmd = Command::new("afplay");
        cmd.arg(path);
        return Some(cmd);
    }
    for name in ["paplay", "pw-play", "aplay"] {
        if which(name) {
            let mut cmd = Command::new(name);
            if name == "aplay" {
                cmd.arg("-q");
            }
            cmd.arg(path);
            return Some(cmd);
        }
    }
    None
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|p| {
                let cand = p.join(name);
                cand.is_file()
            })
        })
        .unwrap_or(false)
}

/// Non-blocking. Never raises into the TUI.
pub fn play(kind: TradeSignal, enabled: Option<bool>) -> bool {
    let on = enabled.unwrap_or_else(signals_enabled);
    if !on {
        return false;
    }
    if let Some(sink) = lock_poison(&SINK).clone() {
        sink(kind);
        return true;
    }
    let path = match chime_paths() {
        Ok((buy, sell_win, sell_loss)) => match kind {
            TradeSignal::Buy => buy,
            TradeSignal::SellWin => sell_win,
            TradeSignal::SellLoss => sell_loss,
        },
        Err(_) => {
            let _ = std::io::Write::write_all(&mut std::io::stderr(), b"\x07");
            return true;
        }
    };
    match player_cmd(&path) {
        None => {
            let _ = std::io::Write::write_all(&mut std::io::stderr(), b"\x07");
            true
        }
        Some(mut cmd) => spawn_player(&mut cmd),
    }
}

fn spawn_player(cmd: &mut Command) -> bool {
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut slot = lock_poison(&PLAYER);
    if let Some(child) = slot.as_mut() {
        match child.try_wait() {
            Ok(Some(_)) => {
                *slot = None;
            }
            Ok(None) => {
                // Previous chime still running — do not stack players or leak zombies.
                return true;
            }
            Err(_) => {
                *slot = None;
            }
        }
    }
    match cmd.spawn() {
        Ok(child) => {
            *slot = Some(child);
            true
        }
        Err(_) => false,
    }
}

/// Reap or stop the player. Call when the TUI exits.
pub fn shutdown() {
    let mut slot = lock_poison(&PLAYER);
    if let Some(mut child) = slot.take() {
        match child.try_wait() {
            Ok(Some(_)) => {}
            _ => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

pub fn emit_decision(
    decision: &Decision,
    result: &LiveApplyResult,
    live: bool,
    has_position: bool,
    won: Option<bool>,
) -> Option<TradeSignal> {
    let kind = kind_for_decision(decision, result, live, has_position, won);
    if let Some(k) = kind {
        play(k, None);
    }
    kind
}

pub fn emit_flatten(result: &FlattenResult, won: Option<bool>) -> Option<TradeSignal> {
    let kind = kind_for_flatten(result, won);
    if let Some(k) = kind {
        play(k, None);
    }
    kind
}

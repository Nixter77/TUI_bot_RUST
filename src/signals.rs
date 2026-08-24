//! Distinct buy / sell chimes. Silent unless the TUI enables them.

use crate::flatten::FlattenResult;
use crate::live::LiveApplyResult;
use crate::models::Decision;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeSignal {
    Buy,
    Sell,
}

/// Rising pair (buy) vs falling pair (sell) — must stay audibly different.
pub const BUY_HZ: (f64, f64) = (880.0, 1175.0);
pub const SELL_HZ: (f64, f64) = (523.0, 349.0);

static ENABLED: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<Arc<dyn Fn(TradeSignal) + Send + Sync>>> = Mutex::new(None);
static WAVS: Mutex<Option<(PathBuf, PathBuf)>> = Mutex::new(None);

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

pub fn kind_for_decision(
    decision: &Decision,
    result: &LiveApplyResult,
    live: bool,
    has_position: bool,
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
        Decision::ExitPosition { .. } => {
            if result.error.is_some() {
                None
            } else if live && !has_position {
                None
            } else {
                Some(TradeSignal::Sell)
            }
        }
        Decision::AmendStop { .. } => None,
    }
}

pub fn kind_for_flatten(result: &FlattenResult) -> Option<TradeSignal> {
    if result.closed.is_empty() {
        None
    } else {
        Some(TradeSignal::Sell)
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

pub fn chime_paths() -> std::io::Result<(PathBuf, PathBuf)> {
    {
        let guard = lock_poison(&WAVS);
        if let Some(pair) = guard.as_ref() {
            return Ok(pair.clone());
        }
    }
    let root = std::env::temp_dir().join("home-economic-signals");
    let buy = write_chime(&root.join("buy.wav"), &[BUY_HZ.0, BUY_HZ.1], 22_050)?;
    let sell = write_chime(&root.join("sell.wav"), &[SELL_HZ.0, SELL_HZ.1], 22_050)?;
    *lock_poison(&WAVS) = Some((buy.clone(), sell.clone()));
    Ok((buy, sell))
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
        Ok((buy, sell)) => match kind {
            TradeSignal::Buy => buy,
            TradeSignal::Sell => sell,
        },
        Err(_) => {
            let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x07");
            return true;
        }
    };
    match player_cmd(&path) {
        None => {
            let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x07");
            true
        }
        Some(mut cmd) => {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.process_group(0);
            }
            match cmd.spawn() {
                Ok(_) => true,
                Err(_) => false,
            }
        }
    }
}

pub fn emit_decision(
    decision: &Decision,
    result: &LiveApplyResult,
    live: bool,
    has_position: bool,
) -> Option<TradeSignal> {
    let kind = kind_for_decision(decision, result, live, has_position);
    if let Some(k) = kind {
        play(k, None);
    }
    kind
}

pub fn emit_flatten(result: &FlattenResult) -> Option<TradeSignal> {
    let kind = kind_for_flatten(result);
    if let Some(k) = kind {
        play(k, None);
    }
    kind
}

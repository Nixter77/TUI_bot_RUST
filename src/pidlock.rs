//! Exclusive live-session lock. Two `--live` processes would double-buy.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const DEFAULT_LIVE_LOCK: &str = ".state/live.lock";

#[derive(Debug)]
pub struct LiveLock {
    path: PathBuf,
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

/// Create-new PID file. Stale locks (dead pid) are replaced.
pub fn acquire_live_lock(path: Option<&Path>) -> io::Result<LiveLock> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LIVE_LOCK));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    for _ in 0..3 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                writeln!(f, "{}", std::process::id())?;
                let _ = f.sync_all();
                return Ok(LiveLock { path });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let text = fs::read_to_string(&path).unwrap_or_default();
                let pid: u32 = text.trim().parse().unwrap_or(0);
                if pid != 0 && pid_alive(pid) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("уже запущен другой --live (pid {pid}). Сначала q в том TUI."),
                    ));
                }
                let _ = fs::remove_file(&path);
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "не удалось взять live.lock",
    ))
}

impl Drop for LiveLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

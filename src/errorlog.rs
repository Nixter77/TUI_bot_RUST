//! Footer errors. Append-only `.state/errors.jsonl`. Never raises into the TUI.

use crate::errors::{describe_exchange_error, extract_int_code, redact_secrets};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

pub const DEFAULT_ERROR_LOG_PATH: &str = ".state/errors.jsonl";
/// Same-error frames before a `still` line (~1 min at 5s poll).
const STILL_EVERY: i32 = 12;

pub fn format_ui_error(raw: &str) -> String {
    format!("Ошибка: {}", describe_exchange_error(raw))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ErrorEvent {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub shown: String,
    #[serde(default)]
    pub raw: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub strategy_id: i32,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub count: i32,
    #[serde(default)]
    pub duration_sec: i32,
    #[serde(default)]
    pub code: String,
}

pub fn extract_code(text: &str) -> String {
    if let Some(code) = extract_int_code(text) {
        return code.to_string();
    }
    if let Some(idx) = text.to_ascii_uppercase().find("HTTP ") {
        let rest = text.get(idx + 5..).unwrap_or("");
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
        if digits.len() == 3 {
            return format!("HTTP {digits}");
        }
    }
    String::new()
}

pub fn read_error_events(path: Option<&Path>) -> Vec<ErrorEvent> {
    let target = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ERROR_LOG_PATH));
    let _io = lock_poison(&ERROR_IO);
    let Ok(text) = fs::read_to_string(target) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<ErrorEvent>(line) {
            out.push(ev);
        }
    }
    out
}

pub fn guess_source(raw: &str, default: &str) -> String {
    let low = raw.to_ascii_lowercase();
    if low.contains("flatten")
        || raw.contains("нечего закрывать")
        || raw.contains("ещё открыты")
        || raw.contains("не удалось прочитать позиции")
    {
        return "flatten".into();
    }
    if low.starts_with("skip ")
        || low.contains("live refused")
        || low.contains("filled but")
        || low.contains("/order")
        || low.contains("/algoorder")
        || low.contains("/leverage")
    {
        return "live".into();
    }
    default.to_string()
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn lock_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

static ERROR_IO: Mutex<()> = Mutex::new(());
static ACTIVE: Mutex<Option<ErrorLog>> = Mutex::new(None);

/// In-process writer. TUI attaches one; `--report` only reads the file.
pub struct ErrorLog {
    pub path: PathBuf,
    shown: Option<String>,
    raw: String,
    source: String,
    strategy_id: i32,
    live: bool,
    symbol: String,
    code: String,
    count: i32,
    started: Option<Instant>,
    started_mono: f64,
    emitted_count: i32,
}

impl ErrorLog {
    pub fn new(path: Option<&Path>) -> Self {
        Self {
            path: path
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_ERROR_LOG_PATH)),
            shown: None,
            raw: String::new(),
            source: String::new(),
            strategy_id: 0,
            live: false,
            symbol: String::new(),
            code: String::new(),
            count: 0,
            started: None,
            started_mono: 0.0,
            emitted_count: 0,
        }
    }

    fn append_line(&self, event: &ErrorEvent) {
        let Ok(json) = serde_json::to_string(event) else {
            return;
        };
        let _io = lock_poison(&ERROR_IO);
        if let Some(parent) = self.path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let line = format!("{json}\n");
            let _ = f.write_all(line.as_bytes()).and_then(|_| f.flush());
        }
    }

    fn emit(&mut self, event: &str, clock: &str, mono: f64) {
        let Some(shown) = self.shown.clone() else {
            return;
        };
        let started = if self.started_mono != 0.0 {
            self.started_mono
        } else {
            mono
        };
        let duration = if let Some(at) = self.started {
            at.elapsed().as_secs() as i32
        } else {
            (mono - started).max(0.0) as i32
        };
        self.append_line(&ErrorEvent {
            ts: clock.to_string(),
            event: event.into(),
            shown,
            raw: self.raw.clone(),
            source: self.source.clone(),
            strategy_id: self.strategy_id,
            live: self.live,
            symbol: self.symbol.clone(),
            count: self.count,
            duration_sec: duration.max(0),
            code: self.code.clone(),
        });
        self.emitted_count = self.count;
    }

    fn close(&mut self, clock: &str, mono: f64) {
        if self.shown.is_none() {
            return;
        }
        self.emit("cleared", clock, mono);
        self.shown = None;
        self.raw.clear();
        self.source.clear();
        self.code.clear();
        self.count = 0;
        self.started = None;
        self.started_mono = 0.0;
        self.emitted_count = 0;
    }

    pub fn observe(
        &mut self,
        shown: Option<&str>,
        raw: &str,
        source: &str,
        strategy_id: i32,
        live: bool,
        symbol: &str,
        ts: Option<&str>,
        now: Option<f64>,
    ) {
        let clock = ts.map(str::to_string).unwrap_or_else(iso_now);
        let mono = now.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        });
        let Some(shown) = shown else {
            self.close(&clock, mono);
            return;
        };
        if self.shown.as_deref() == Some(shown) {
            self.count += 1;
            if self.count - self.emitted_count >= STILL_EVERY {
                self.emit("still", &clock, mono);
            }
            return;
        }
        self.close(&clock, mono);
        self.shown = Some(shown.to_string());
        self.raw = redact_secrets(raw);
        self.source = if source.is_empty() {
            guess_source(raw, "poll")
        } else {
            source.to_string()
        };
        self.strategy_id = strategy_id;
        self.live = live;
        self.symbol = symbol.to_string();
        self.code = extract_code(&format!("{raw} {shown}"));
        self.count = 1;
        self.started = Some(Instant::now());
        self.started_mono = mono;
        self.emitted_count = 0;
        self.emit("shown", &clock, mono);
    }

    pub fn note_frame(
        &mut self,
        last_error: Option<&str>,
        strategy_id: i32,
        live: bool,
        symbol: &str,
        source: &str,
    ) {
        if let Some(raw) = last_error {
            let shown = format_ui_error(raw);
            self.observe(
                Some(&shown),
                raw,
                source,
                strategy_id,
                live,
                symbol,
                None,
                None,
            );
        } else {
            self.observe(None, "", source, strategy_id, live, symbol, None, None);
        }
    }
}

pub fn set_active(log: Option<ErrorLog>) {
    let mut slot = lock_poison(&ACTIVE);
    if let Some(mut old) = slot.take() {
        old.observe(None, "", "", 0, false, "", None, None);
    }
    *slot = log;
}

pub fn note_frame(
    last_error: Option<&str>,
    strategy_id: i32,
    live: bool,
    symbol: &str,
    source: &str,
) {
    if let Some(log) = lock_poison(&ACTIVE).as_mut() {
        log.note_frame(last_error, strategy_id, live, symbol, source);
    }
}

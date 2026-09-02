//! UTC hours when crypto typically ignites. Pure: no I/O.

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};

pub type HourWindow = (u8, u8);

pub const DEFAULT_ENTRY_HOURS: &str = "0-2,7-10,13-16";
pub const DEFAULT_ENTRY_WINDOWS: [HourWindow; 3] = [(0, 2), (7, 10), (13, 16)];

fn window_label_for(window: HourWindow) -> String {
    match window {
        (0, 2) => "старт Азия / дневная свеча".into(),
        (7, 10) => "старт Лондон".into(),
        (13, 16) => "старт Нью-Йорк / пересечение".into(),
        (start, end) => format!("старт {start:02}–{:02} UTC", end % 24),
    }
}

pub fn parse_entry_windows(raw: &str) -> Result<Vec<HourWindow>, String> {
    let text = raw.trim().to_ascii_lowercase();
    if text.is_empty() || matches!(text.as_str(), "*" | "24" | "all" | "always") {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = text.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Ok(Vec::new());
    }
    let mut windows = Vec::new();
    for part in parts {
        let (left, right) = part
            .split_once('-')
            .ok_or_else(|| format!("entry hour window must look like 7-10, got {part:?}"))?;
        let start: i32 = left
            .trim()
            .parse()
            .map_err(|_| format!("entry hour window must be integers, got {part:?}"))?;
        let end: i32 = right
            .trim()
            .parse()
            .map_err(|_| format!("entry hour window must be integers, got {part:?}"))?;
        if !(0..=23).contains(&start) {
            return Err("entry window start must be 0–23".into());
        }
        if !(0..=24).contains(&end) {
            return Err("entry window end must be 0–24".into());
        }
        if start == end {
            return Err("entry window start and end must differ".into());
        }
        windows.push((start as u8, end as u8));
    }
    Ok(windows)
}

pub fn format_windows(windows: &[HourWindow]) -> String {
    if windows.is_empty() {
        return "круглосуточно".into();
    }
    let bits: Vec<String> = windows
        .iter()
        .map(|&(start, end)| {
            let end_h = if end == 24 { 0 } else { end };
            format!("{start:02}–{end_h:02}")
        })
        .collect();
    format!("{} UTC", bits.join(", "))
}

pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn make_utc_ts(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> f64 {
    Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
        .single()
        .expect("valid utc")
        .timestamp() as f64
}

pub fn utc_datetime(ts: f64) -> DateTime<Utc> {
    let secs = ts.trunc() as i64;
    let nsecs = ((ts.fract().abs()) * 1_000_000_000.0) as u32;
    Utc.timestamp_opt(secs, nsecs)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(secs, 0).unwrap())
}

pub fn hour_in_windows(hour: u32, windows: &[HourWindow]) -> bool {
    if windows.is_empty() {
        return true;
    }
    for &(start, end) in windows {
        let start = start as u32;
        let end = end as u32;
        if start < end {
            if hour >= start && hour < end {
                return true;
            }
        } else if hour >= start || hour < end {
            return true;
        }
    }
    false
}

pub fn in_entry_window(ts: f64, windows: Option<&[HourWindow]>, always: bool) -> bool {
    if always {
        return true;
    }
    let default = DEFAULT_ENTRY_WINDOWS;
    let use_w: &[HourWindow] = match windows {
        None => &default,
        Some(w) if w.is_empty() => return true,
        Some(w) => w,
    };
    hour_in_windows(utc_datetime(ts).hour(), use_w)
}

fn window_at(hour: u32, windows: &[HourWindow]) -> Option<HourWindow> {
    windows.iter().copied().find(|&w| hour_in_windows(hour, &[w]))
}

/// Exclusive end of the UTC window that contains `ts`, or `None` if 24/7 / closed.
pub fn window_end_ts(ts: f64, windows: &[HourWindow]) -> Option<f64> {
    if windows.is_empty() {
        return None;
    }
    let now = utc_datetime(ts);
    let w = window_at(now.hour(), windows)?;
    let end_h = (w.1 as u32) % 24;
    let mut end = Utc
        .with_ymd_and_hms(now.year(), now.month(), now.day(), end_h, 0, 0)
        .single()?;
    if (end.timestamp() as f64) <= ts {
        end += chrono::Duration::days(1);
    }
    Some(end.timestamp() as f64)
}

/// After a losing stop: sit out until this window ends (or `now + pause_sec` if 24/7).
pub fn pause_until_after_loss(now: f64, windows: &[HourWindow], pause_sec: f64) -> f64 {
    let floor = now + pause_sec.max(0.0);
    match window_end_ts(now, windows) {
        Some(end) if end > floor => end,
        _ => floor,
    }
}

pub fn next_window_start(ts: f64, windows: &[HourWindow]) -> Option<DateTime<Utc>> {
    if windows.is_empty() {
        return None;
    }
    let now = utc_datetime(ts);
    for day in 0..2 {
        let day0 = now + chrono::Duration::days(day);
        let day0 = day0
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let mut starts: Vec<u8> = windows.iter().map(|w| w.0).collect();
        starts.sort_unstable();
        for start in starts {
            let candidate = Utc
                .with_ymd_and_hms(day0.year(), day0.month(), day0.day(), start as u32, 0, 0)
                .single()?;
            if candidate > now {
                return Some(candidate);
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    pub open: bool,
    pub utc_clock: String,
    pub label: String,
    pub windows_text: String,
    pub next_open_clock: Option<String>,
}

pub fn session_status(ts: f64, windows: Option<&[HourWindow]>, always: bool) -> SessionStatus {
    let default = DEFAULT_ENTRY_WINDOWS;
    let use_w: Vec<HourWindow> = if always {
        Vec::new()
    } else {
        match windows {
            None => default.to_vec(),
            Some(w) => w.to_vec(),
        }
    };
    let now = utc_datetime(ts);
    let clock = now.format("%H:%M").to_string();
    if always || use_w.is_empty() {
        return SessionStatus {
            open: true,
            utc_clock: clock,
            label: "входы круглосуточно".into(),
            windows_text: "круглосуточно".into(),
            next_open_clock: None,
        };
    }
    if let Some(current) = window_at(now.hour(), &use_w) {
        return SessionStatus {
            open: true,
            utc_clock: clock,
            label: window_label_for(current),
            windows_text: format_windows(&use_w),
            next_open_clock: None,
        };
    }
    let nxt = next_window_start(ts, &use_w).map(|d| d.format("%H:%M").to_string());
    SessionStatus {
        open: false,
        utc_clock: clock,
        label: "вне часов старта".into(),
        windows_text: format_windows(&use_w),
        next_open_clock: nxt,
    }
}

pub fn outside_entry_reason(status: &SessionStatus) -> String {
    let nxt = status
        .next_open_clock
        .as_ref()
        .map(|c| format!("; следующий старт {c} UTC"))
        .unwrap_or_default();
    format!(
        "вне часов старта (сейчас {} UTC, входы {}{nxt})",
        status.utc_clock, status.windows_text
    )
}

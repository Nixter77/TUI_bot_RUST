//! Footer error formatting. Journals stay silent unless a log is attached.

use crate::errors::describe_exchange_error;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_ERROR_LOG_PATH: &str = ".state/errors.jsonl";

pub fn format_ui_error(raw: &str) -> String {
    format!("Ошибка: {}", describe_exchange_error(raw))
}

#[derive(Debug, Clone, Deserialize)]
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

pub fn read_error_events(path: Option<&Path>) -> Vec<ErrorEvent> {
    let target = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ERROR_LOG_PATH));
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

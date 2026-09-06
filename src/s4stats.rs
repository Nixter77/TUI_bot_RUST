//! Persist Strategy-4 skip tallies. Domain tally lives in `continuation`.

use crate::continuation::s4_skip_stats_top;
use std::fs;
use std::path::PathBuf;

const S4_SKIP_STATS_PATH: &str = ".state/s4_skip_stats.json";

/// Persist top skip reasons to `.state/s4_skip_stats.json` (best-effort).
pub fn flush_s4_skip_stats() {
    let top = s4_skip_stats_top(12);
    if top.is_empty() {
        return;
    }
    let mut map = serde_json::Map::new();
    for (reason, count) in &top {
        map.insert(reason.clone(), serde_json::json!(count));
    }
    let body = serde_json::Value::Object(map);
    let path = PathBuf::from(S4_SKIP_STATS_PATH);
    if let Some(parent) = path.parent() {
        crate::errors::ensure_private_dir(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&body) {
        if fs::write(&path, bytes).is_ok() {
            crate::errors::restrict_private_file(&path);
        }
    }
}

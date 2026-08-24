//! Environment-only configuration. Never reads keys from instructions.md.

use crate::dayrisk::default_daily_loss_usdt;
use crate::money::dec;
use crate::sessions::{parse_entry_windows, HourWindow, DEFAULT_ENTRY_HOURS};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const DEFAULT_TESTNET_BASE: &str = "https://testnet.binancefuture.com";
pub const MAINNET_BASE: &str = "https://fapi.binance.com";
pub const STRATEGY1_POLL_SECONDS: i32 = 60;
pub const DEFAULT_MAX_POSITIONS: i32 = 1;

pub fn default_notional() -> Decimal {
    Decimal::from(20)
}
pub fn default_tp_pct() -> Decimal {
    Decimal::new(25, 3) // 0.025
}
pub fn default_trail_pct() -> Decimal {
    Decimal::new(20, 3) // 0.020
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct ConfigError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub api_key: String,
    pub api_secret: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub credentials: Option<Credentials>,
    pub base_url: String,
    pub poll_seconds: i32,
    pub order_notional: Decimal,
    pub tp_pct: Decimal,
    pub trail_pct: Decimal,
    pub recv_window: i32,
    pub live: bool,
    pub http_timeout: f64,
    pub starting_equity: Option<Decimal>,
    pub entry_windows: Vec<HourWindow>,
    pub always_enter: bool,
    pub s4_entry_windows: Vec<HourWindow>,
    pub s4_always_enter: bool,
    pub leverage: Option<i32>,
    pub max_positions: i32,
    pub notional_from_exchange: bool,
    pub daily_loss_usdt: Decimal,
}

fn strip_quotes(value: &str) -> String {
    let v = value.trim();
    if v.len() >= 2 {
        let b = v.as_bytes();
        if (b[0] == b'\'' || b[0] == b'"') && b[0] == b[v.len() - 1] {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

pub fn load_dotenv_file(path: &Path) -> HashMap<String, String> {
    let Ok(text) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for raw in text.lines() {
        let mut line = raw.trim().to_string();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim().to_string();
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !is_identifier(key) {
            continue;
        }
        out.insert(key.to_string(), strip_quotes(value));
    }
    out
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn env_get(name: &str, file_vals: &HashMap<String, String>, environ: Option<&HashMap<String, String>>) -> String {
    if let Some(env) = environ {
        return env.get(name).cloned().unwrap_or_default().trim().to_string();
    }
    if let Ok(v) = std::env::var(name) {
        return v.trim().to_string();
    }
    file_vals.get(name).cloned().unwrap_or_default().trim().to_string()
}

pub fn load_config(
    live: bool,
    env_file: Option<&Path>,
    environ: Option<&HashMap<String, String>>,
) -> Result<Config, ConfigError> {
    let mut file_vals = HashMap::new();
    let env_path: PathBuf = env_file
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".env"));
    if environ.is_none() && env_path.is_file() {
        let mode = fs::metadata(&env_path)
            .map_err(|e| ConfigError(e.to_string()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(ConfigError(
                ".env is group/world-readable; chmod 600 and retry".into(),
            ));
        }
        file_vals = load_dotenv_file(&env_path);
    }

    let get = |name: &str, default: &str| -> String {
        let v = env_get(name, &file_vals, environ);
        if v.is_empty() {
            default.to_string()
        } else {
            v
        }
    };
    let get_opt = |name: &str| env_get(name, &file_vals, environ);

    let key = get_opt("BINANCE_API_KEY");
    let secret = get_opt("BINANCE_API_SECRET");
    let creds = if !key.is_empty() && !secret.is_empty() {
        if key.len() < 16 || secret.len() < 16 {
            return Err(ConfigError(
                "BINANCE_API_KEY / BINANCE_API_SECRET look truncated".into(),
            ));
        }
        Some(Credentials {
            api_key: key,
            api_secret: secret,
        })
    } else if !key.is_empty() || !secret.is_empty() {
        return Err(ConfigError(
            "both BINANCE_API_KEY and BINANCE_API_SECRET are required".into(),
        ));
    } else {
        None
    };

    let mut base = get("BINANCE_FAPI_BASE", DEFAULT_TESTNET_BASE);
    while base.ends_with('/') {
        base.pop();
    }
    if !base.starts_with("https://") {
        return Err(ConfigError("BINANCE_FAPI_BASE must be https".into()));
    }
    let allow = get_opt("BINANCE_ALLOW_MAINNET");
    let allow_mainnet = matches!(allow.as_str(), "1" | "true" | "TRUE" | "yes");
    if base.starts_with(MAINNET_BASE) && !allow_mainnet {
        return Err(ConfigError(
            "refusing mainnet base URL; set BINANCE_ALLOW_MAINNET=1 to override".into(),
        ));
    }

    let poll_raw = get("STRATEGY1_POLL_SECONDS", &STRATEGY1_POLL_SECONDS.to_string());
    let poll: i32 = poll_raw
        .parse()
        .map_err(|_| ConfigError("STRATEGY1_POLL_SECONDS must be an integer".into()))?;
    if poll != 60 && poll != 120 {
        return Err(ConfigError("STRATEGY1_POLL_SECONDS must be 60 or 120".into()));
    }

    let notional_raw = get("ORDER_NOTIONAL_USDT", "20");
    let notional_from_exchange = matches!(
        notional_raw.to_ascii_lowercase().as_str(),
        "binance" | "min" | "exchange" | "0"
    );
    let notional = if notional_from_exchange {
        default_notional()
    } else {
        dec(&notional_raw).map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?
    };
    let tp_pct = dec(&get("TAKE_PROFIT_PCT", "0.025"))
        .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;
    let trail_pct = dec(&get("TRAIL_PCT", "0.020"))
        .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;
    let recv_window: i32 = get("BINANCE_RECV_WINDOW", "5000")
        .parse()
        .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;
    let timeout: f64 = get("HTTP_TIMEOUT", "10")
        .parse()
        .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;
    let max_positions: i32 = get("STRATEGY1_MAX_POSITIONS", &DEFAULT_MAX_POSITIONS.to_string())
        .parse()
        .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;
    let daily_loss_usdt = dec(&get("DAILY_LOSS_USDT", "20"))
        .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;

    if notional <= Decimal::ZERO || tp_pct <= Decimal::ZERO || trail_pct <= Decimal::ZERO {
        return Err(ConfigError(
            "notional, take-profit, and trail percents must be positive".into(),
        ));
    }
    if !(1..=10).contains(&max_positions) {
        return Err(ConfigError("STRATEGY1_MAX_POSITIONS must be 1–10".into()));
    }
    if daily_loss_usdt < Decimal::ZERO {
        return Err(ConfigError("DAILY_LOSS_USDT cannot be negative".into()));
    }

    let lev_raw = get_opt("FUTURES_LEVERAGE");
    let leverage = if lev_raw.is_empty()
        || matches!(
            lev_raw.to_ascii_lowercase().as_str(),
            "0" | "binance" | "default" | "none" | "off"
        ) {
        None
    } else {
        let n: i32 = lev_raw
            .parse()
            .map_err(|_| ConfigError("FUTURES_LEVERAGE must be an integer or binance".into()))?;
        if !(1..=125).contains(&n) {
            return Err(ConfigError("FUTURES_LEVERAGE must be 1–125".into()));
        }
        Some(n)
    };
    if !(100..=60_000).contains(&recv_window) {
        return Err(ConfigError("BINANCE_RECV_WINDOW out of range".into()));
    }
    if timeout <= 0.0 || timeout > 60.0 {
        return Err(ConfigError("HTTP_TIMEOUT out of range".into()));
    }

    let start_raw = get_opt("BINANCE_STARTING_EQUITY");
    let starting = if start_raw.is_empty() {
        None
    } else {
        let v = dec(&start_raw).map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;
        if v < Decimal::ZERO {
            return Err(ConfigError("BINANCE_STARTING_EQUITY cannot be negative".into()));
        }
        Some(v)
    };

    let always_enter = matches!(
        get_opt("STRATEGY1_ALWAYS_ENTER").as_str(),
        "1" | "true" | "TRUE" | "yes"
    );
    let hours_raw = get("STRATEGY1_ENTRY_HOURS", DEFAULT_ENTRY_HOURS);
    let mut entry_windows =
        parse_entry_windows(&hours_raw).map_err(|e| ConfigError(format!("STRATEGY1_ENTRY_HOURS: {e}")))?;
    if always_enter {
        entry_windows.clear();
    }

    let s4_always_enter = matches!(
        get_opt("STRATEGY4_ALWAYS_ENTER").as_str(),
        "1" | "true" | "TRUE" | "yes"
    );
    let s4_hours_raw = get("STRATEGY4_ENTRY_HOURS", DEFAULT_ENTRY_HOURS);
    let mut s4_entry_windows = parse_entry_windows(&s4_hours_raw)
        .map_err(|e| ConfigError(format!("STRATEGY4_ENTRY_HOURS: {e}")))?;
    if s4_always_enter {
        s4_entry_windows.clear();
    }

    if live && creds.is_none() {
        return Err(ConfigError(
            "refusing --live without BINANCE_API_KEY and BINANCE_API_SECRET".into(),
        ));
    }

    let _ = default_daily_loss_usdt();
    Ok(Config {
        credentials: creds,
        base_url: base,
        poll_seconds: poll,
        order_notional: notional,
        tp_pct,
        trail_pct,
        recv_window,
        live,
        http_timeout: timeout,
        starting_equity: starting,
        entry_windows,
        always_enter,
        s4_entry_windows,
        s4_always_enter,
        leverage,
        max_positions,
        notional_from_exchange,
        daily_loss_usdt,
    })
}

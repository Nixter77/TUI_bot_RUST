//! Environment-only configuration. Never reads keys from instructions.md.

use crate::dayrisk::{default_daily_loss_r, default_daily_loss_usdt};
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
pub const ALLOWED_TESTNET_HOSTS: &[&str] = &[
    "testnet.binancefuture.com",
    "demo-fapi.binance.com",
];
pub const MAINNET_HOST: &str = "fapi.binance.com";

/// Continuation (strategy 4) kline interval. Scalp stays 1m-class, trend stays 1d.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeInterval {
    Minute5,
    Minute15,
    Minute30,
    Hour1,
}

impl TradeInterval {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s = raw
            .trim()
            .to_lowercase()
            .replace('м', "m")
            .replace('ч', "h")
            .replace(' ', "");
        match s.as_str() {
            "5" | "5m" | "m5" => Ok(Self::Minute5),
            "15" | "15m" | "m15" => Ok(Self::Minute15),
            "30" | "30m" | "m30" => Ok(Self::Minute30),
            "1" | "1h" | "60" | "60m" | "h1" => Ok(Self::Hour1),
            _ => Err(format!(
                "STRATEGY4_INTERVAL must be 5m, 15m, 30m, or 1h (got {raw})"
            )),
        }
    }

    pub fn as_binance(self) -> &'static str {
        match self {
            Self::Minute5 => "5m",
            Self::Minute15 => "15m",
            Self::Minute30 => "30m",
            Self::Hour1 => "1h",
        }
    }

    pub fn as_ru(self) -> &'static str {
        match self {
            Self::Minute5 => "5м",
            Self::Minute15 => "15м",
            Self::Minute30 => "30м",
            Self::Hour1 => "1ч",
        }
    }

    pub fn duration_ms(self) -> i64 {
        match self {
            Self::Minute5 => 5 * 60_000,
            Self::Minute15 => 15 * 60_000,
            Self::Minute30 => 30 * 60_000,
            Self::Hour1 => 60 * 60_000,
        }
    }

    pub fn fetch_limit(self) -> usize {
        50
    }

    pub fn chart_limit(self) -> usize {
        121
    }

    /// Continuation SL floor as a fraction of price. Wider on slower candles.
    pub fn min_stop_pct(self) -> Decimal {
        match self {
            Self::Minute5 => Decimal::new(15, 3),
            Self::Minute15 => Decimal::new(20, 3),
            Self::Minute30 => Decimal::new(25, 3),
            Self::Hour1 => Decimal::new(30, 3),
        }
    }

    /// Skip the setup if structure/ATR stop is wider than this.
    pub fn max_stop_pct(self) -> Decimal {
        match self {
            Self::Minute5 => Decimal::new(35, 3),
            Self::Minute15 => Decimal::new(50, 3),
            Self::Minute30 => Decimal::new(60, 3),
            Self::Hour1 => Decimal::new(80, 3),
        }
    }

    pub fn min_pullback_pct(self) -> Decimal {
        match self {
            Self::Minute5 => Decimal::new(10, 3),
            Self::Minute15 => Decimal::new(12, 3),
            Self::Minute30 => Decimal::new(15, 3),
            Self::Hour1 => Decimal::new(20, 3),
        }
    }

    /// Take-profit in R (risk units). Same on every TF: TP follows the stop.
    pub fn reward_r(self) -> Decimal {
        Decimal::from(2)
    }

    pub fn geometry_ru(self) -> String {
        format!(
            "SL {}–{}%  TP {}R",
            pct_label(self.min_stop_pct()),
            pct_label(self.max_stop_pct()),
            self.reward_r().normalize()
        )
    }
}

fn pct_label(frac: Decimal) -> String {
    (frac * Decimal::from(100)).normalize().to_string()
}

impl Default for TradeInterval {
    fn default() -> Self {
        Self::Minute5
    }
}

/// Host of an `https://` FAPI base, lowercased, no port.
pub fn fapi_host(base: &str) -> String {
    let rest = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .unwrap_or(base);
    let host = rest.split('/').next().unwrap_or("");
    host.split(':').next().unwrap_or("").to_ascii_lowercase()
}

pub fn fapi_base_allowed(base: &str, allow_mainnet: bool) -> bool {
    let host = fapi_host(base);
    if ALLOWED_TESTNET_HOSTS.contains(&host.as_str()) {
        return true;
    }
    host == MAINNET_HOST && allow_mainnet
}
pub const STRATEGY1_POLL_SECONDS: i32 = 60;
pub const DEFAULT_MAX_POSITIONS: i32 = 1;
/// Strategy 4 concurrent longs. Wider than S1 so the liquid book can fill.
pub const DEFAULT_S4_MAX_POSITIONS: i32 = 5;
/// Strategy 2 (scalp) max hold in signal bars (1m-class). Shorter than legacy 24.
pub const DEFAULT_S2_MAX_HOLD_BARS: usize = 8;

pub fn default_notional() -> Decimal {
    Decimal::from(20)
}
pub fn default_tp_pct() -> Decimal {
    Decimal::new(25, 3) // 0.025
}
pub fn default_trail_pct() -> Decimal {
    Decimal::new(20, 3) // 0.020
}
/// Strategy-4 risk fraction of account equity. `0` turns risk-% sizing off.
pub fn default_risk_pct() -> Decimal {
    Decimal::new(25, 4) // 0.0025 = 0.25%
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct ConfigError(pub String);

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub api_key: String,
    pub api_secret: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix: String = self.api_key.chars().take(4).collect();
        f.debug_struct("Credentials")
            .field("api_key", &format!("{prefix}…"))
            .field("api_secret", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub credentials: Option<Credentials>,
    pub base_url: String,
    pub poll_seconds: i32,
    pub order_notional: Decimal,
    /// Fraction of account equity (wallet + uPnL) risked per S4 entry. `0` = off.
    pub risk_pct: Decimal,
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
    pub s4_interval: TradeInterval,
    /// Strategy 4 basket size (STRATEGY4_MAX_POSITIONS). Independent of S1.
    pub s4_max_positions: i32,
    /// Strategy 2 (scalp) entry windows (STRATEGY2_ENTRY_HOURS).
    pub s2_entry_windows: Vec<HourWindow>,
    pub s2_always_enter: bool,
    /// Strategy 2 max hold bars (STRATEGY2_MAX_HOLD_BARS).
    pub s2_max_hold_bars: usize,
    pub leverage: Option<i32>,
    pub max_positions: i32,
    pub notional_from_exchange: bool,
    pub daily_loss_usdt: Decimal,
    /// Max day loss in R (1R = day_start_equity × risk_pct). 0 disables R halt.
    pub daily_loss_r: Decimal,
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
    if !fapi_base_allowed(&base, allow_mainnet) {
        return Err(ConfigError(format!(
            "BINANCE_FAPI_BASE host not allowlisted ({}); use testnet.binancefuture.com",
            fapi_host(&base)
        )));
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
    let risk_raw = get_opt("RISK_PCT");
    let risk_pct = if risk_raw.is_empty() {
        default_risk_pct()
    } else {
        dec(&risk_raw).map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?
    };
    if risk_pct < Decimal::ZERO {
        return Err(ConfigError("RISK_PCT cannot be negative".into()));
    }
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
    let s4_max_positions: i32 = get(
        "STRATEGY4_MAX_POSITIONS",
        &DEFAULT_S4_MAX_POSITIONS.to_string(),
    )
    .parse()
    .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;
    let daily_loss_usdt = dec(&get("DAILY_LOSS_USDT", "20"))
        .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;
    let daily_loss_r = dec(&get(
        "DAILY_LOSS_R",
        &default_daily_loss_r().normalize().to_string(),
    ))
    .map_err(|e| ConfigError(format!("invalid numeric config: {e}")))?;

    if notional <= Decimal::ZERO || tp_pct <= Decimal::ZERO || trail_pct <= Decimal::ZERO {
        return Err(ConfigError(
            "notional, take-profit, and trail percents must be positive".into(),
        ));
    }
    if !(1..=10).contains(&max_positions) {
        return Err(ConfigError("STRATEGY1_MAX_POSITIONS must be 1–10".into()));
    }
    if !(1..=10).contains(&s4_max_positions) {
        return Err(ConfigError("STRATEGY4_MAX_POSITIONS must be 1–10".into()));
    }
    if daily_loss_usdt < Decimal::ZERO {
        return Err(ConfigError("DAILY_LOSS_USDT cannot be negative".into()));
    }
    if daily_loss_r < Decimal::ZERO {
        return Err(ConfigError("DAILY_LOSS_R cannot be negative".into()));
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
    let s4_interval = TradeInterval::parse(&get("STRATEGY4_INTERVAL", "5m"))
        .map_err(ConfigError)?;

    let s2_always_enter = matches!(
        get_opt("STRATEGY2_ALWAYS_ENTER").as_str(),
        "1" | "true" | "TRUE" | "yes"
    );
    let s2_hours_raw = get("STRATEGY2_ENTRY_HOURS", DEFAULT_ENTRY_HOURS);
    let mut s2_entry_windows = parse_entry_windows(&s2_hours_raw)
        .map_err(|e| ConfigError(format!("STRATEGY2_ENTRY_HOURS: {e}")))?;
    if s2_always_enter {
        s2_entry_windows.clear();
    }
    let s2_hold_raw = get(
        "STRATEGY2_MAX_HOLD_BARS",
        &DEFAULT_S2_MAX_HOLD_BARS.to_string(),
    );
    let s2_max_hold_bars: usize = s2_hold_raw
        .parse()
        .map_err(|_| ConfigError("STRATEGY2_MAX_HOLD_BARS must be an integer".into()))?;
    if !(1..=240).contains(&s2_max_hold_bars) {
        return Err(ConfigError(
            "STRATEGY2_MAX_HOLD_BARS must be 1–240".into(),
        ));
    }

    if live && creds.is_none() {
        return Err(ConfigError(
            "refusing --live without BINANCE_API_KEY and BINANCE_API_SECRET".into(),
        ));
    }

    let _ = default_daily_loss_usdt();
    let _ = default_daily_loss_r();
    Ok(Config {
        credentials: creds,
        base_url: base,
        poll_seconds: poll,
        order_notional: notional,
        risk_pct,
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
        s4_interval,
        s4_max_positions,
        s2_entry_windows,
        s2_always_enter,
        s2_max_hold_bars,
        leverage,
        max_positions,
        notional_from_exchange,
        daily_loss_usdt,
        daily_loss_r,
    })
}

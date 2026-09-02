//! Binance / TestNet error catalog.

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

pub const ACTION_RETRY: &str = "retry";
pub const ACTION_SKIP: &str = "skip_symbol";
pub const ACTION_COOLDOWN: &str = "cooldown";
pub const ACTION_IGNORE: &str = "ignore";
pub const ACTION_OPERATOR: &str = "operator";
pub const ACTION_KEEP: &str = "keep";
pub const COOLDOWN_SEC: f64 = 1800.0;
/// After a retryable transport fault, do not mill new entries every 5s poll.
pub const RETRY_BACKOFF_SEC: f64 = 20.0;
/// Cap for exponential retry backoff (20s → 40s → 60s).
pub const RETRY_BACKOFF_CAP_SEC: f64 = 60.0;

/// Consecutive retryable faults: 20s, 40s, then 60s.
pub fn retry_backoff_sec(strikes: u8) -> f64 {
    let n = strikes.max(1).min(3);
    (RETRY_BACKOFF_SEC * f64::from(1u32 << (n - 1))).min(RETRY_BACKOFF_CAP_SEC)
}
/// After a losing close, keep that symbol off the buy list for 12 hours.
/// 12h so a loser skips the next UTC session window; 4h reprinted BCH same day;
/// 24h emptied the liquid book.
pub const LOSS_SYMBOL_COOLDOWN_SEC: f64 = 43_200.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedError {
    pub code: Option<i32>,
    pub action: String,
    pub message: String,
    pub exchange_msg: String,
}

fn policy(code: i32) -> Option<(&'static str, &'static str)> {
    Some(match code {
        -1001 | -1006 | -1007 | -1008 | -1016 | -1021 => (ACTION_RETRY, "TestNet retry"),
        -2011 | -2013 | -4130 => (ACTION_IGNORE, "ignore"),
        -2014 | -2015 | -1022 | -2023 | -1002 | -1003 | -1099 => (ACTION_OPERATOR, "operator"),
        -2018 | -2019 => (ACTION_COOLDOWN, "margin"),
        -2022 | -2026 | -2021 | -4087 | -2024 => (ACTION_KEEP, "keep"),
        -2027 | -4411 | -1121 | -4164 | -1013 | -1111 | -2010 => (ACTION_SKIP, "skip"),
        _ => return None,
    })
}

fn policy_message(code: i32) -> (&'static str, String) {
    match code {
        -1007 => (ACTION_RETRY, "TestNet не ответил вовремя (−1007).".into()),
        -1008 => (ACTION_RETRY, "TestNet перегружен (−1008/429).".into()),
        -1001 => (ACTION_RETRY, "TestNet оборвал соединение (−1001).".into()),
        -1021 => (
            ACTION_RETRY,
            "Часы рассинхронизированы (−1021). Подстрою время.".into(),
        ),
        -2027 => (
            ACTION_SKIP,
            "Лимит позиции при этом плече (−2027). Этот символ не берём.".into(),
        ),
        -4411 => (
            ACTION_SKIP,
            "Биржа отказала (−4411): TradFi-Perps (золото/акции). Этот символ больше не берём.".into(),
        ),
        -4130 => (
            ACTION_IGNORE,
            "TP/SL уже стоят на бирже (−4130). Не дублирую.".into(),
        ),
        -2022 => (
            ACTION_KEEP,
            "reduceOnly отклонён (−2022). Книга уже плоская — не открываю шорт.".into(),
        ),
        -1102 => (
            ACTION_KEEP,
            "Биржа отказала (−1102): обязательный параметр ордера (algoType).".into(),
        ),
        -2015 => (
            ACTION_OPERATOR,
            "Ключ отклонён (−2015). Права или IP-белый список.".into(),
        ),
        other => {
            if let Some((a, m)) = policy(other) {
                (a, m.to_string())
            } else {
                (ACTION_KEEP, format!("Биржа: код {other}."))
            }
        }
    }
}

pub fn parse_binance_error(text: &str) -> Option<(i32, String)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let body: Value = serde_json::from_str(&text[start..=end]).ok()?;
    let obj = body.as_object()?;
    let code = obj.get("code")?.as_i64()? as i32;
    let msg = obj.get("msg")?.as_str()?.to_string();
    if msg.is_empty() {
        return None;
    }
    Some((code, msg))
}

pub fn extract_int_code(text: &str) -> Option<i32> {
    if let Some((code, _)) = parse_binance_error(text) {
        return Some(code);
    }
    // − or - followed by 3-5 digits
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'-' || (b == 0xE2 && bytes.get(i + 1) == Some(&0x88) && bytes.get(i + 2) == Some(&0x92)) {
            let rest = if b == b'-' {
                &text[i + 1..]
            } else {
                &text[i + 3..]
            };
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if (3..=5).contains(&digits.len()) {
                if let Ok(n) = digits.parse::<i32>() {
                    return Some(-n);
                }
            }
        }
    }
    None
}

fn hint_code(text: &str) -> Option<i32> {
    let low = text.to_ascii_lowercase();
    let hints = [
        ("timeout waiting", -1007),
        ("tradfi-perps", -4411),
        ("sign tradfi", -4411),
        ("agreement contract", -4411),
        ("maximum allowable position", -2027),
        ("maximum allowable quantity", -2027),
        ("leverage cap", -2027),
        ("margin is insufficient", -2019),
        ("balance is insufficient", -2018),
        ("reduceonly", -2022),
        ("order would immediately trigger", -2021),
        ("closeposition in the direction", -4130),
        ("notional below exchange minimum", -4164),
        ("qty below minqty", -1013),
        ("order does not exist", -2013),
        ("unknown order sent", -2011),
        ("timestamp", -1021),
        ("invalid api-key", -2015),
        ("api-key format invalid", -2014),
    ];
    for (needle, code) in hints {
        if low.contains(needle) {
            return Some(code);
        }
    }
    if text.contains("HTTP 408") {
        return Some(-1007);
    }
    if text.contains("HTTP 429") {
        return Some(-1008);
    }
    None
}

fn fallback_policy(text: &str, parsed: Option<(i32, String)>) -> (String, String) {
    let low = text.to_ascii_lowercase();
    if low.contains("inflates risk") || text.contains("раздувает риск") {
        return (
            ACTION_SKIP.into(),
            "minNotional раздувает риск — символ пропускаю.".into(),
        );
    }
    if low.contains("timeout ") || low.contains("timed out") {
        return (ACTION_RETRY.into(), "Сеть: таймаут запроса.".into());
    }
    if text.contains("HTTP 408") {
        return (ACTION_RETRY.into(), "TestNet не ответил вовремя (−1007).".into());
    }
    if text.contains("HTTP 429") {
        return (ACTION_RETRY.into(), "TestNet перегружен (−1008/429).".into());
    }
    if text.contains("HTTP 502") || text.contains("HTTP 503") || text.contains("HTTP 504") {
        return (ACTION_RETRY.into(), "TestNet 5xx.".into());
    }
    if text.contains("HTTP 401") || text.contains("HTTP 403") {
        return (
            ACTION_OPERATOR.into(),
            "Доступ к API запрещён. Проверьте ключи.".into(),
        );
    }
    if let Some((code, msg)) = parsed {
        return (
            ACTION_SKIP.into(),
            format!("Биржа отказала ({code}): {}", clip_chars(&msg, 120)),
        );
    }
    (ACTION_KEEP.into(), clip_chars(text, 160))
}

fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let take = max.saturating_sub(1);
    let cut: String = text.chars().take(take).collect();
    format!("{cut}…")
}

pub fn classify(text: &str) -> ClassifiedError {
    let parsed = parse_binance_error(text);
    let mut code = parsed.as_ref().map(|p| p.0).or_else(|| extract_int_code(text));
    let exchange_msg = parsed.as_ref().map(|p| p.1.clone()).unwrap_or_default();
    if code.is_none() {
        code = hint_code(text);
    }
    let (action, message) = if let Some(c) = code {
        let (a, m) = policy_message(c);
        if a == ACTION_KEEP && m.starts_with("Биржа: код") {
            fallback_policy(text, parsed)
        } else {
            (a.to_string(), m)
        }
    } else {
        fallback_policy(text, parsed)
    };
    ClassifiedError {
        code,
        action,
        message,
        exchange_msg,
    }
}

fn secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(signature|api[_-]?key|secret|listenkey)=([^&\s"']+)"#).expect("secret redact regex")
    })
}

/// Strip HMAC/query credentials that ureq may put in transport errors.
pub fn redact_secrets(text: &str) -> String {
    secret_re().replace_all(text, "$1=***").into_owned()
}

pub fn describe_exchange_error(text: &str) -> String {
    let info = classify(text);
    let message = if !info.message.is_empty() {
        info.message
    } else {
        clip_chars(text, 160)
    };
    redact_secrets(&message)
}

pub fn is_retry_error(text: Option<&str>) -> bool {
    match text {
        None | Some("") => false,
        Some(t) => classify(t).action == ACTION_RETRY,
    }
}

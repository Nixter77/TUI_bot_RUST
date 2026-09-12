//! Live trade pings to Telegram. Token never logged; HTTP off the TUI thread.
//! Open / close / flatten only — amend (trail SL) is too noisy.

use crate::errors::redact_secrets;
use crate::journal::TradeEvent;
use crate::money::dec;
use rust_decimal::Decimal;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const SEND_TIMEOUT: Duration = Duration::from_secs(8);
const QUEUE_CAP: usize = 8;
const TEXT_MAX: usize = 3900;
const API_HOST: &str = "https://api.telegram.org";

#[derive(Clone, PartialEq, Eq)]
pub struct TelegramDest {
    bot_token: String,
    chat_id: String,
}

impl std::fmt::Debug for TelegramDest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramDest")
            .field("bot_token", &"[redacted]")
            .field("chat_id", &self.chat_id)
            .finish()
    }
}

impl TelegramDest {
    pub fn parse(token: &str, chat_id: &str) -> Result<Self, String> {
        let token = token.trim();
        let chat_id = chat_id.trim();
        if !valid_bot_token(token) {
            return Err("TELEGRAM_BOT_TOKEN looks invalid".into());
        }
        if !valid_chat_id(chat_id) {
            return Err("TELEGRAM_CHAT_ID must be an integer".into());
        }
        Ok(Self {
            bot_token: token.to_string(),
            chat_id: chat_id.to_string(),
        })
    }

    pub fn chat_id(&self) -> &str {
        &self.chat_id
    }
}

fn valid_bot_token(token: &str) -> bool {
    let Some((id, secret)) = token.split_once(':') else {
        return false;
    };
    (8..=12).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_digit())
        && (30..=50).contains(&secret.len())
        && secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_chat_id(id: &str) -> bool {
    let s = id.strip_prefix('-').unwrap_or(id);
    (5..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

static DEST: Mutex<Option<TelegramDest>> = Mutex::new(None);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
static TX: OnceLock<SyncSender<String>> = OnceLock::new();
static CAPTURE: Mutex<Option<Vec<String>>> = Mutex::new(None);

fn lock_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn install(dest: Option<TelegramDest>) {
    *lock_poison(&DEST) = dest;
}

pub fn is_configured() -> bool {
    lock_poison(&DEST).is_some()
}

pub fn take_last_error() -> Option<String> {
    lock_poison(&LAST_ERROR).take()
}

fn set_last_error(msg: String) {
    *lock_poison(&LAST_ERROR) = Some(redact_secrets(&msg));
}

/// Integration tests: capture instead of HTTP. Drop via [`install`]`(None)`.
pub fn begin_capture() {
    *lock_poison(&CAPTURE) = Some(Vec::new());
}

pub fn take_captured() -> Vec<String> {
    lock_poison(&CAPTURE).take().unwrap_or_default()
}

pub fn should_notify(ev: &TradeEvent) -> bool {
    ev.live && matches!(ev.event.as_str(), "open" | "close" | "flatten")
}

pub fn format_message(ev: &TradeEvent) -> Option<String> {
    if !should_notify(ev) {
        return None;
    }
    let symbol = crate::journal::journal_symbol(&ev.symbol);
    if symbol.is_empty() {
        return None;
    }
    let strat = strategy_label(ev.strategy_id);
    let text = match ev.event.as_str() {
        "open" => {
            let sl = ev.stop_loss.as_deref().unwrap_or("—");
            let tp = ev.take_profit.as_deref().unwrap_or("—");
            format!(
                "LIVE {strat} · LONG {symbol}\nвход  {}\nqty   {}\nSL    {sl}   TP {tp}\nпричина: {}",
                ev.price,
                ev.qty,
                ev.reason.trim()
            )
        }
        "close" => {
            let pnl = ev.pnl.as_deref().unwrap_or("—");
            let sign = pnl_mark(ev.pnl.as_deref());
            let r = ev
                .final_r
                .as_deref()
                .map(|r| format!("  ({r}R)"))
                .unwrap_or_default();
            format!(
                "LIVE {strat} · CLOSE {symbol}\nвыход {price}   PnL {sign}{pnl} USDT{r}\nпричина: {reason}",
                price = ev.price,
                reason = ev.reason.trim()
            )
        }
        "flatten" => format!(
            "LIVE {strat} · FLATTEN {symbol}\nпричина: {}",
            ev.reason.trim()
        ),
        _ => return None,
    };
    Some(truncate(&text))
}

fn pnl_mark(raw: Option<&str>) -> &'static str {
    let Some(p) = raw.and_then(|s| dec(s).ok()) else {
        return "";
    };
    if p > Decimal::ZERO {
        "+"
    } else if p < Decimal::ZERO {
        ""
    } else {
        ""
    }
}

fn strategy_label(id: i32) -> &'static str {
    match id {
        1 => "S1 Momentum",
        2 => "S2 Scalp",
        3 => "S3 Trend",
        4 => "S4 Continuation",
        5 => "S5 Verify",
        _ => "S?",
    }
}

fn truncate(text: &str) -> String {
    if text.len() <= TEXT_MAX {
        return text.to_string();
    }
    let mut end = TEXT_MAX;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub fn notify_event(ev: &TradeEvent) {
    let Some(text) = format_message(ev) else {
        return;
    };
    dispatch(text);
}

pub fn notify_startup(strategy_id: i32) {
    if lock_poison(&DEST).is_none() && lock_poison(&CAPTURE).is_none() {
        return;
    }
    let strat = strategy_label(strategy_id);
    dispatch(format!(
        "TUI_bot LIVE · {strat}\nсделки open / close / flatten → этот чат\namend (трейл SL) не шлём"
    ));
}

fn dispatch(text: String) {
    if let Some(buf) = lock_poison(&CAPTURE).as_mut() {
        buf.push(text);
        return;
    }
    if lock_poison(&DEST).is_none() {
        return;
    }
    enqueue(text);
}

fn enqueue(text: String) {
    let tx = TX.get_or_init(start_worker);
    if tx.try_send(text).is_err() {
        set_last_error("telegram queue full".into());
    }
}

fn start_worker() -> SyncSender<String> {
    let (tx, rx) = sync_channel(QUEUE_CAP);
    let _ = thread::Builder::new()
        .name("tg-notify".into())
        .spawn(move || worker(rx));
    tx
}

fn worker(rx: Receiver<String>) {
    let agent = ureq::AgentBuilder::new()
        .timeout(SEND_TIMEOUT)
        .redirects(0)
        .build();
    while let Ok(text) = rx.recv() {
        let dest = lock_poison(&DEST).clone();
        let Some(dest) = dest else {
            continue;
        };
        if let Err(e) = send_message(&agent, &dest, &text) {
            set_last_error(e);
        }
    }
}

fn send_message(agent: &ureq::Agent, dest: &TelegramDest, text: &str) -> Result<(), String> {
    let url = format!("{API_HOST}/bot{}/sendMessage", dest.bot_token);
    let body = serde_json::json!({
        "chat_id": dest.chat_id,
        "text": text,
        "disable_web_page_preview": true,
    });
    let resp = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(map_ureq)?;
    let status = resp.status();
    let raw = resp.into_string().unwrap_or_default();
    let ok = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("ok").and_then(|x| x.as_bool()))
        .unwrap_or(false);
    if status == 200 && ok {
        return Ok(());
    }
    let desc = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("description")
                .and_then(|x| x.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| redact_secrets(&raw));
    Err(format!("telegram HTTP {status}: {desc}"))
}

fn map_ureq(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            format!("telegram HTTP {code}: {}", redact_secrets(&body))
        }
        ureq::Error::Transport(t) => format!("telegram transport: {t}"),
    }
}

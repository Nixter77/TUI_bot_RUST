//! Binance HMAC-SHA256 query signing. Secret never appears in the query.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::BTreeMap;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SignError {
    #[error("empty signing secret")]
    EmptySecret,
    #[error("HMAC key rejected")]
    InvalidKey,
}

pub fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn canonical_query(params: &BTreeMap<String, String>) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn sign_query(secret: &str, query_string: &str) -> Result<String, SignError> {
    if secret.is_empty() {
        return Err(SignError::EmptySecret);
    }
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| SignError::InvalidKey)?;
    mac.update(query_string.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

pub fn signed_query_string(
    params: &BTreeMap<String, String>,
    secret: &str,
    timestamp_ms: i64,
    recv_window: i64,
) -> Result<String, SignError> {
    let mut payload = params.clone();
    payload.insert("timestamp".into(), timestamp_ms.to_string());
    payload.insert("recvWindow".into(), recv_window.to_string());
    let query = canonical_query(&payload);
    let signature = sign_query(secret, &query)?;
    Ok(format!("{query}&signature={signature}"))
}

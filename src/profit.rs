//! Account profit: current equity minus a pinned starting equity.

use crate::money::{dec, fmt_fixed};
use rust_decimal::Decimal;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_BASELINE_PATH: &str = ".state/starting_equity";

pub fn current_equity(wallet_balance: Decimal, unrealized_pnl: Decimal) -> Decimal {
    wallet_balance + unrealized_pnl
}

pub fn account_profit(wallet_balance: Decimal, unrealized_pnl: Decimal, starting_equity: Decimal) -> Decimal {
    current_equity(wallet_balance, unrealized_pnl) - starting_equity
}

pub fn pin_starting_equity(existing: Option<Decimal>, live_equity: Decimal) -> Decimal {
    existing.unwrap_or(live_equity)
}

pub fn load_persisted_starting_equity(path: Option<&Path>) -> Option<Decimal> {
    let target = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BASELINE_PATH));
    let text = fs::read_to_string(&target).ok()?;
    let first = text.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    let value = dec(first).ok()?;
    if value < Decimal::ZERO {
        return None;
    }
    Some(value)
}

pub fn persist_starting_equity(value: Decimal, path: Option<&Path>) -> PathBuf {
    let target = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BASELINE_PATH));
    if let Some(parent) = target.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let text = format!("{}\n", fmt_fixed(value));
    let _ = fs::write(&target, text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
    }
    target
}

#[derive(Debug, Clone)]
pub struct EquityPin {
    pub value: Option<Decimal>,
    pub persist: bool,
}

impl EquityPin {
    pub fn from_config(starting_equity: Option<Decimal>) -> Self {
        if let Some(v) = starting_equity {
            Self {
                value: Some(v),
                persist: false,
            }
        } else {
            Self {
                value: load_persisted_starting_equity(None),
                persist: true,
            }
        }
    }

    pub fn capture(&mut self, live_equity: Decimal) -> Decimal {
        if self.value.is_none() {
            self.value = Some(live_equity);
            if self.persist {
                persist_starting_equity(live_equity, None);
            }
        }
        self.value.unwrap()
    }
}

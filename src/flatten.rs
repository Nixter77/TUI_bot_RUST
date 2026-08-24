//! Panic flatten: close every open position. Fail-closed, no invented fills.

use crate::exchange::{parse_positions, ExchangeError, FlattenClient};
use crate::models::{Position, Side};
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlattenTarget {
    pub symbol: String,
    pub side: Side,
    pub qty: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlattenResult {
    pub closed: Vec<String>,
    pub errors: Vec<String>,
}

impl FlattenResult {
    pub fn error(&self) -> Option<String> {
        if self.errors.is_empty() {
            None
        } else {
            Some(self.errors.join("; "))
        }
    }

    pub fn symbols(&self) -> Vec<String> {
        self.closed
            .iter()
            .filter_map(|label| label.rsplit(' ').next().map(|s| s.to_string()))
            .collect()
    }
}

pub fn flatten_targets(positions: &[Position]) -> Vec<FlattenTarget> {
    let mut by_key: Vec<((String, Side), FlattenTarget)> = Vec::new();
    for pos in positions {
        if pos.qty <= Decimal::ZERO {
            continue;
        }
        if pos.side != Side::Long && pos.side != Side::Short {
            continue;
        }
        let symbol = pos.symbol.to_ascii_uppercase();
        if symbol.is_empty() {
            continue;
        }
        let key = (symbol.clone(), pos.side);
        let target = FlattenTarget {
            symbol,
            side: pos.side,
            qty: pos.qty,
        };
        if let Some(slot) = by_key.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = target;
        } else {
            by_key.push((key, target));
        }
    }
    by_key.into_iter().map(|(_, t)| t).collect()
}

pub fn close_targets(client: &mut dyn FlattenClient, positions: &[Position]) -> FlattenResult {
    let mut closed = Vec::new();
    let mut errors = Vec::new();
    for target in flatten_targets(positions) {
        let label = format!("{} {}", target.side, target.symbol);
        match (|| {
            client.cancel_protectives(&target.symbol)?;
            client.market_close(&target.symbol, target.side.as_str(), target.qty)?;
            // Sibling TP/SL can survive the close and later open a short.
            let _ = client.cancel_protectives(&target.symbol);
            Ok::<(), ExchangeError>(())
        })() {
            Ok(()) => closed.push(label),
            Err(exc) => errors.push(format!("{}: {exc}", target.symbol)),
        }
    }
    FlattenResult { closed, errors }
}

pub fn close_all_positions(
    live: bool,
    has_credentials: bool,
    client: &mut dyn FlattenClient,
    positions: &[Position],
) -> FlattenResult {
    if !live {
        return FlattenResult {
            closed: Vec::new(),
            errors: vec!["flatten refused: not live".into()],
        };
    }
    if !has_credentials {
        return FlattenResult {
            closed: Vec::new(),
            errors: vec!["flatten refused: no credentials".into()],
        };
    }
    close_targets(client, positions)
}

pub fn flatten_open_book(client: &mut dyn FlattenClient) -> FlattenResult {
    let positions = match client.position_risk() {
        Ok(raw) => match parse_positions(&raw) {
            Ok(p) => p,
            Err(exc) => {
                return FlattenResult {
                    closed: Vec::new(),
                    errors: vec![format!("не удалось прочитать позиции: {exc}")],
                };
            }
        },
        Err(exc) => {
            return FlattenResult {
                closed: Vec::new(),
                errors: vec![format!("не удалось прочитать позиции: {exc}")],
            };
        }
    };
    if positions.is_empty() {
        return FlattenResult::default();
    }
    let mut result = close_targets(client, &positions);
    if result.closed.is_empty() {
        return result;
    }
    let still = match client.position_risk() {
        Ok(raw) => parse_positions(&raw).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    if !still.is_empty() {
        let leftover = still
            .iter()
            .map(|p| format!("{} {}", p.side, p.symbol))
            .collect::<Vec<_>>()
            .join(", ");
        result.errors.push(format!("ещё открыты: {leftover}"));
    }
    result
}

//! Rank USDT-M futures tickers by 24h rise. Highest percent wins.

use crate::models::{near_24h_high, ticker_from_mapping, Ticker};
use regex::Regex;
use rust_decimal::Decimal;
use std::sync::OnceLock;

pub const LIQUID_MAJORS: [&str; 3] = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];

/// Strategy 1 buys names with at least this 24h % (same ranking as «Топ роста»).
pub fn momentum_min_change_percent() -> Decimal {
    Decimal::new(4, 1) // +0.4%
}

fn usdt_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z0-9]{2,20}USDT$").unwrap())
}

pub fn is_tradable_symbol(symbol: &str) -> bool {
    usdt_re().is_match(symbol) && !is_junk_symbol(symbol)
}

/// Levered 1000x names and the meme tape that SL'd the TestNet book in minutes.
pub fn is_junk_symbol(symbol: &str) -> bool {
    let s = symbol.trim().to_ascii_uppercase();
    let base = s.strip_suffix("USDT").unwrap_or(&s);
    if base.starts_with("1000") || base.starts_with("10000") || base.starts_with("1M") {
        return true;
    }
    matches!(
        base,
        "FARTCOIN"
            | "DOGS"
            | "BRETT"
            | "PEPE"
            | "SHIB"
            | "FLOKI"
            | "NEIRO"
            | "MEME"
            | "PENGU"
            | "ZORA"
    )
}

fn exclude_set(exclude: &[String]) -> std::collections::HashSet<String> {
    exclude
        .iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn parse_tickers(raw: &serde_json::Value) -> Vec<Ticker> {
    let Some(list) = raw.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in list {
        if let Ok(ticker) = ticker_from_mapping(item) {
            if ticker.last_price > Decimal::ZERO && is_tradable_symbol(&ticker.symbol) {
                out.push(ticker);
            }
        }
    }
    out
}

fn eligible(
    tickers: &[Ticker],
    min_quote_volume: Decimal,
    min_price: Decimal,
    min_change_percent: Option<Decimal>,
    max_change_percent: Option<Decimal>,
    quote_suffix: &str,
    exclude: &[String],
) -> Vec<Ticker> {
    let skip = exclude_set(exclude);
    tickers
        .iter()
        .filter(|t| {
            if skip.contains(&t.symbol) {
                return false;
            }
            if is_junk_symbol(&t.symbol) {
                return false;
            }
            if !t.symbol.ends_with(quote_suffix) {
                return false;
            }
            if t.last_price <= Decimal::ZERO || t.last_price < min_price {
                return false;
            }
            if t.quote_volume < min_quote_volume {
                return false;
            }
            if let Some(min_c) = min_change_percent {
                if t.price_change_percent < min_c {
                    return false;
                }
            }
            if let Some(max_c) = max_change_percent {
                if t.price_change_percent > max_c {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect()
}

pub fn rank_most_rising(tickers: &[Ticker], exclude: &[String]) -> Option<Ticker> {
    let rows = eligible(
        tickers,
        Decimal::ZERO,
        Decimal::ZERO,
        None,
        None,
        "USDT",
        exclude,
    );
    rows.into_iter().max_by(|a, b| {
        a.price_change_percent
            .cmp(&b.price_change_percent)
            .then(a.quote_volume.cmp(&b.quote_volume))
            .then(a.symbol.cmp(&b.symbol))
    })
}

fn fallback_btc(tickers: &[Ticker]) -> Option<Ticker> {
    tickers
        .iter()
        .find(|t| t.symbol == "BTCUSDT" && t.last_price > Decimal::ZERO)
        .cloned()
}

pub fn apply_liquidity_floor(tickers: &[Ticker], min_frac: Decimal) -> Vec<Ticker> {
    if tickers.len() < 2 {
        return tickers.to_vec();
    }
    let top = tickers
        .iter()
        .map(|t| t.quote_volume)
        .max()
        .unwrap_or(Decimal::ZERO);
    if top <= Decimal::ZERO {
        return tickers.to_vec();
    }
    let floor = top * min_frac;
    let filtered: Vec<Ticker> = tickers
        .iter()
        .filter(|t| t.quote_volume >= floor)
        .cloned()
        .collect();
    if filtered.is_empty() {
        tickers.to_vec()
    } else {
        filtered
    }
}

pub fn pick_momentum_book(
    tickers: &[Ticker],
    n: usize,
    min_quote_volume: Decimal,
    min_price: Decimal,
    min_change_percent: Decimal,
    max_change_percent: Option<Decimal>,
    exclude: &[String],
    drop_range_top: bool,
) -> Vec<Ticker> {
    if n == 0 {
        return Vec::new();
    }
    let mut rows = eligible(
        tickers,
        min_quote_volume,
        min_price,
        Some(min_change_percent),
        max_change_percent,
        "USDT",
        exclude,
    );
    rows.retain(|t| t.quote_volume > Decimal::ZERO);
    if drop_range_top {
        let frac = Decimal::new(2, 2); // 0.02
        rows.retain(|t| !near_24h_high(t, frac));
    }
    rows.sort_by(|a, b| {
        b.price_change_percent
            .cmp(&a.price_change_percent)
            .then(b.quote_volume.cmp(&a.quote_volume))
            .then(b.symbol.cmp(&a.symbol))
    });
    rows.truncate(n);
    rows
}

/// Strategy 1 book: top 24h % among tradable USDT-M, not BTC/ETH/SOL only.
pub fn pick_strategy1_book(tickers: &[Ticker], n: usize, exclude: &[String]) -> Vec<Ticker> {
    pick_momentum_book(
        tickers,
        n,
        Decimal::from(50_000),
        Decimal::ZERO,
        momentum_min_change_percent(),
        Some(Decimal::from(12)),
        exclude,
        true,
    )
}

pub fn pick_momentum_ticker(
    tickers: &[Ticker],
    min_quote_volume: Decimal,
    min_price: Decimal,
    min_change_percent: Decimal,
    max_change_percent: Option<Decimal>,
    exclude: &[String],
) -> Option<Ticker> {
    pick_momentum_book(
        tickers,
        1,
        min_quote_volume,
        min_price,
        min_change_percent,
        max_change_percent,
        exclude,
        false,
    )
    .into_iter()
    .next()
}

pub fn iter_liquid_majors(tickers: &[Ticker], exclude: &[String]) -> Vec<Ticker> {
    let skip = exclude_set(exclude);
    let mut majors: Vec<Ticker> = tickers
        .iter()
        .filter(|t| {
            LIQUID_MAJORS.contains(&t.symbol.as_str())
                && !skip.contains(&t.symbol)
                && t.last_price > Decimal::ZERO
        })
        .cloned()
        .collect();
    majors.sort_by(|a, b| {
        b.price_change_percent
            .cmp(&a.price_change_percent)
            .then(b.quote_volume.cmp(&a.quote_volume))
            .then(b.symbol.cmp(&a.symbol))
    });
    majors
}

pub fn pick_liquid_major(tickers: &[Ticker], exclude: &[String]) -> Option<Ticker> {
    let majors = iter_liquid_majors(tickers, exclude);
    if !majors.is_empty() {
        Some(majors[0].clone())
    } else {
        fallback_btc(tickers)
    }
}

pub fn pick_scalp_ticker(tickers: &[Ticker], exclude: &[String]) -> Option<Ticker> {
    pick_liquid_major(tickers, exclude)
}

pub fn pick_trend_ticker(tickers: &[Ticker], exclude: &[String]) -> Option<Ticker> {
    pick_liquid_major(tickers, exclude)
}

pub fn pick_chart_ticker(tickers: &[Ticker], strategy_id: i32, exclude: &[String]) -> Option<Ticker> {
    if strategy_id == 1 {
        pick_strategy1_book(tickers, 1, exclude).into_iter().next()
    } else if strategy_id == 2 {
        pick_scalp_ticker(tickers, exclude)
    } else {
        pick_trend_ticker(tickers, exclude)
    }
}

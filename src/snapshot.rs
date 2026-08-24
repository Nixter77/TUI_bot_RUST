//! Build a MarketSnapshot. Network stays out of the curses loop.

use crate::config::Config;
use crate::errors::describe_exchange_error;
use crate::exchange::{
    account_with_position_upnl, load_account, parse_positions, BinanceFutures, ExchangeError, SnapshotClient,
};
use crate::models::{
    last_closed_bar, pick_managed_long, pick_managed_longs, remembered_positions, Account, Bar, EngineState,
    MarketSnapshot, Position, Side,
};
use crate::profit::EquityPin;
use crate::ranking::{iter_liquid_majors, pick_chart_ticker, pick_strategy1_book, rank_most_rising};
use crate::trend::{CHART_INTERVAL, CHART_LIMIT};
use rust_decimal::Decimal;
use std::collections::HashMap;

pub fn empty_account(starting: Option<Decimal>) -> Account {
    let start = starting.unwrap_or(Decimal::ZERO);
    Account {
        wallet_balance: Decimal::ZERO,
        unrealized_pnl: Decimal::ZERO,
        available_balance: Decimal::ZERO,
        starting_equity: start,
    }
}

pub fn make_client(cfg: &Config) -> BinanceFutures {
    BinanceFutures::from_config(cfg)
}



fn merge_skip(state: &mut EngineState, extra: &[String]) {
    let mut have: std::collections::HashSet<String> =
        state.skip_symbols.iter().map(|s| s.to_ascii_uppercase()).collect();
    have.extend(extra.iter().map(|s| s.to_ascii_uppercase()));
    for symbol in extra {
        state
            .skip_reasons
            .entry(symbol.to_ascii_uppercase())
            .or_insert_with(|| "TradFi".into());
    }
    let mut sorted: Vec<String> = have.into_iter().collect();
    sorted.sort();
    state.skip_symbols = sorted;
}

fn held_book<'a>(
    prior: Option<&'a MarketSnapshot>,
    fallback_account: Option<&'a Account>,
    fallback_positions: &'a [Position],
    state_position: Option<&'a Position>,
) -> (Option<Account>, Option<Vec<Position>>, Option<Position>) {
    if let Some(prior) = prior {
        if prior.account_ok {
            return (
                Some(prior.account.clone()),
                Some(prior.open_positions.clone()),
                prior.position.clone(),
            );
        }
    }
    if let Some(acc) = fallback_account {
        let first = fallback_positions
            .first()
            .cloned()
            .or_else(|| state_position.cloned());
        return (Some(acc.clone()), Some(fallback_positions.to_vec()), first);
    }
    (None, None, state_position.cloned())
}

fn closed_klines(
    client: &mut dyn SnapshotClient,
    symbol: &str,
    interval: &str,
    limit: usize,
) -> Result<Vec<Bar>, ExchangeError> {
    let mut raw = client.klines(symbol, interval, limit)?;
    if raw.len() >= 2 {
        raw.pop();
    }
    Ok(raw)
}

fn chart_spec(strategy_id: i32) -> (&'static str, usize) {
    if strategy_id == 3 {
        (CHART_INTERVAL, CHART_LIMIT)
    } else {
        ("5m", 121)
    }
}

fn collect_s4_history(
    client: &mut dyn SnapshotClient,
    state: &EngineState,
    tickers: &[crate::models::Ticker],
    chart_symbol: &str,
    chart_bars: &[Bar],
    remembered: &[Position],
    n: i32,
) -> (HashMap<String, Bar>, HashMap<String, Vec<Bar>>) {
    let mut last_bars = HashMap::new();
    let mut universe = HashMap::new();
    if !chart_symbol.is_empty() && !chart_bars.is_empty() {
        universe.insert(chart_symbol.to_string(), chart_bars.to_vec());
        if let Some(closed) = chart_bars.last() {
            last_bars.insert(chart_symbol.to_string(), closed.clone());
        }
    }
    let book = crate::continuation::pick_strategy4_book(
        tickers,
        (n.max(1) as usize) + 4,
        &state.skip_symbols,
        None,
    );
    let mut want: Vec<String> = book.into_iter().map(|t| t.symbol).collect();
    for pos in remembered {
        if pos.qty > Decimal::ZERO
            && !want
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&pos.symbol))
        {
            want.push(pos.symbol.clone());
        }
    }
    for symbol in want {
        if universe.contains_key(&symbol) {
            continue;
        }
        match closed_klines(client, &symbol, "5m", 24) {
            Ok(extra) if !extra.is_empty() => {
                if let Some(closed) = extra.last() {
                    last_bars.insert(symbol.clone(), closed.clone());
                }
                universe.insert(symbol, extra);
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    (last_bars, universe)
}

fn collect_last_bars(
    client: &mut dyn SnapshotClient,
    state: &EngineState,
    tickers: &[crate::models::Ticker],
    chart_symbol: &str,
    bars: &[Bar],
    n: i32,
) -> HashMap<String, Bar> {
    let mut out = HashMap::new();
    if !chart_symbol.is_empty() {
        if let Some(closed) = last_closed_bar(bars) {
            out.insert(chart_symbol.to_string(), closed.clone());
        }
    }
    if state.strategy_id == 4 {
        let book = crate::continuation::pick_strategy4_book(
            tickers,
            n.max(1) as usize,
            &state.skip_symbols,
            None,
        );
        for ticker in book {
            if out.contains_key(&ticker.symbol) {
                continue;
            }
            let extra = match client.klines(&ticker.symbol, "5m", 3) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if let Some(candle) = last_closed_bar(&extra) {
                out.insert(ticker.symbol, candle.clone());
            }
        }
        return out;
    }
    if state.strategy_id != 1 {
        return out;
    }
    let book = pick_strategy1_book(tickers, n.max(1) as usize, &state.skip_symbols);
    for ticker in book {
        if out.contains_key(&ticker.symbol) {
            continue;
        }
        let extra = match client.klines(&ticker.symbol, "5m", 3) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if let Some(candle) = last_closed_bar(&extra) {
            out.insert(ticker.symbol, candle.clone());
        }
    }
    out
}

pub fn fetch_snapshot(
    cfg: &Config,
    client: Option<&mut dyn SnapshotClient>,
    state: &EngineState,
    offline: bool,
    starting_equity: Option<Decimal>,
    prior: Option<&MarketSnapshot>,
    fallback_account: Option<&Account>,
    fallback_positions: &[Position],
    overlay_positions: &[Position],
) -> MarketSnapshot {
    let baseline = starting_equity.or(cfg.starting_equity);
    if offline || client.is_none() {
        let mut snap = MarketSnapshot::empty(if offline {
            Decimal::ZERO
        } else {
            baseline.unwrap_or(Decimal::ZERO)
        });
        snap.position = state.position.clone();
        if !offline {
            snap.last_error = Some("нет сетевого клиента".into());
        }
        return snap;
    }
    let client = client.unwrap();

    let mut error: Option<String> = None;
    let mut tickers = Vec::new();
    let mut bars: Vec<Bar> = Vec::new();
    let mut last_bars: HashMap<String, Bar> = HashMap::new();
    let mut universe_bars: HashMap<String, Vec<Bar>> = HashMap::new();
    let (held_account, held_positions, held_position) =
        held_book(prior, fallback_account, fallback_positions, state.position.as_ref());
    let mut account = empty_account(Some(Decimal::ZERO));
    let mut account_ok = false;
    let mut account_fresh = false;
    let mut position = state.position.clone();
    let mut live_book = false;
    let mut open_positions: Vec<Position> = Vec::new();
    let mut chart_symbol = "BTCUSDT".to_string();

    match client.ticker_24h() {
        Ok(t) => tickers = t,
        Err(exc) => {
            error = Some(describe_exchange_error(&exc.0));
            if let Some(held) = held_account {
                account = held;
                account_ok = true;
                open_positions = held_positions.unwrap_or_default();
                position = held_position;
            }
            return MarketSnapshot {
                tickers,
                bars,
                account,
                position,
                chart_symbol,
                fetched: false,
                last_error: error,
                live_book,
                open_positions,
                account_ok,
                account_fresh,
                last_bars,
                universe_bars,
            };
        }
    }
    let overlay_longs: Vec<Position> = overlay_positions
        .iter()
        .filter(|p| p.side == Side::Long && p.qty > Decimal::ZERO)
        .cloned()
        .collect();
    let extra: Vec<Position> = overlay_longs
        .into_iter()
        .chain(state.positions.iter().cloned())
        .collect();
    let remembered = remembered_positions(state.position.as_ref(), &extra);
    let skip = &state.skip_symbols;
    if let Some(first) = remembered.first() {
        chart_symbol = first.symbol.clone();
    } else {
        let picked = if state.strategy_id == 1 {
            pick_strategy1_book(&tickers, cfg.max_positions.max(1) as usize, skip)
                .into_iter()
                .next()
        } else if state.strategy_id == 4 {
            crate::continuation::pick_strategy4_book(
                &tickers,
                cfg.max_positions.max(1) as usize,
                skip,
                None,
            )
            .into_iter()
            .next()
        } else {
            pick_chart_ticker(&tickers, state.strategy_id, skip)
        };
        let picked = picked.or_else(|| rank_most_rising(&tickers, skip));
        if let Some(p) = picked {
            chart_symbol = p.symbol;
        }
    }
    if cfg.credentials.is_some() {
        if let Some(held) = &held_account {
            account = held.clone();
            account_ok = true;
        }
        match load_account(client, baseline) {
            Ok(acc) => {
                account = acc;
                account_ok = true;
                account_fresh = true;
            }
            Err(exc) => {
                error = Some(describe_exchange_error(&exc.0));
                if let Some(held) = &held_account {
                    account = held.clone();
                    account_ok = true;
                }
            }
        }
        match client.position_risk() {
            Ok(raw) => match parse_positions(&raw) {
                Ok(positions) => {
                    live_book = true;
                    let managed = pick_managed_longs(&positions, &remembered);
                    let by_sym: HashMap<&str, &Position> =
                        managed.iter().map(|p| (p.symbol.as_str(), p)).collect();
                    open_positions = positions
                        .iter()
                        .map(|p| {
                            if p.side == Side::Long {
                                if let Some(m) = by_sym.get(p.symbol.as_str()) {
                                    return (*m).clone();
                                }
                            }
                            p.clone()
                        })
                        .collect();
                    position = if !managed.is_empty() {
                        Some(managed[0].clone())
                    } else {
                        pick_managed_long(&positions, state.position.as_ref())
                    };
                    account = account_with_position_upnl(account, &open_positions);
                }
                Err(exc) => {
                    if error.is_none() {
                        error = Some(describe_exchange_error(&exc.0));
                    }
                    if let Some(held) = &held_positions {
                        open_positions = held.clone();
                        position = held_position.clone();
                    }
                }
            },
            Err(exc) => {
                if error.is_none() {
                    error = Some(describe_exchange_error(&exc.0));
                }
                if let Some(held) = &held_positions {
                    open_positions = held.clone();
                    position = held_position.clone();
                }
            }
        }
    }
    if !chart_symbol.is_empty() {
        let (interval, limit) = chart_spec(state.strategy_id);
        match closed_klines(client, &chart_symbol, interval, limit) {
            Ok(b) => bars = b,
            Err(exc) => {
                if error.is_none() {
                    error = Some(describe_exchange_error(&exc.0));
                }
            }
        }
    }
    if matches!(state.strategy_id, 2 | 3) && remembered.is_empty() {
        let (interval, limit) = chart_spec(state.strategy_id);
        let mut want: Vec<String> = iter_liquid_majors(&tickers, skip)
            .into_iter()
            .map(|t| t.symbol)
            .collect();
        if !chart_symbol.is_empty() && !want.iter().any(|s| s == &chart_symbol) {
            want.push(chart_symbol.clone());
        }
        for symbol in want {
            if symbol == chart_symbol {
                universe_bars.insert(symbol, bars.clone());
                continue;
            }
            match closed_klines(client, &symbol, interval, limit) {
                Ok(extra) if !extra.is_empty() => {
                    universe_bars.insert(symbol, extra);
                }
                Ok(_) => {}
                Err(exc) => {
                    if error.is_none() {
                        error = Some(describe_exchange_error(&exc.0));
                    }
                }
            }
        }
    }
    if state.strategy_id == 4 {
        let (lb, ub) = collect_s4_history(
            client,
            state,
            &tickers,
            &chart_symbol,
            &bars,
            &remembered,
            cfg.max_positions,
        );
        last_bars = lb;
        universe_bars.extend(ub);
    } else {
        last_bars = collect_last_bars(client, state, &tickers, &chart_symbol, &bars, cfg.max_positions);
    }

    MarketSnapshot {
        tickers,
        bars,
        account,
        position,
        chart_symbol,
        fetched: error.is_none(),
        last_error: error,
        live_book,
        open_positions,
        account_ok,
        account_fresh,
        last_bars,
        universe_bars,
    }
}

pub fn pull_snapshot(
    cfg: &Config,
    mut client: Option<&mut dyn SnapshotClient>,
    state: &mut EngineState,
    pin: &mut EquityPin,
    offline: bool,
    prior: Option<&MarketSnapshot>,
) -> MarketSnapshot {
    if !offline {
        if let Some(c) = client.as_mut() {
            let tradfi = c.tradfi_symbols().unwrap_or_default();
            merge_skip(state, &tradfi);
        }
    }
    let overlay = state.positions.clone();
    let snapshot = fetch_snapshot(
        cfg,
        client,
        state,
        offline,
        if offline { None } else { pin.value },
        prior,
        None,
        &[],
        &overlay,
    );
    if snapshot.live_book && snapshot.account_fresh {
        pin.capture(snapshot.account.starting_equity);
    }
    snapshot
}

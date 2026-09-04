//! Build a MarketSnapshot. HTTP stays out of the TUI key poll.

use crate::config::Config;
use crate::errors::describe_exchange_error;
use crate::exchange::{
    account_with_position_upnl, load_account, parse_positions, BinanceFutures, ExchangeError, SnapshotClient,
};
use crate::models::{
    last_closed_bar, overlay_long_stop, pick_managed_long, pick_managed_longs, remembered_positions, Account,
    Bar, EngineState, MarketSnapshot, Position, Side,
};
use crate::profit::EquityPin;
use crate::ranking::{iter_liquid_majors, pick_chart_ticker, pick_strategy1_book, rank_most_rising};
use crate::sessions::unix_now;
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

/// Fill missing SL/TP from the journal so a restarted TUI still knows 1R.
pub fn merge_overlay_with_journal(mut overlay: Vec<Position>, journal: Vec<Position>) -> Vec<Position> {
    for j in journal {
        if j.side != Side::Long || j.qty <= Decimal::ZERO {
            continue;
        }
        if let Some(p) = overlay
            .iter_mut()
            .find(|p| p.symbol.eq_ignore_ascii_case(&j.symbol))
        {
            p.stop_loss = overlay_long_stop(p.side, p.stop_loss, j.stop_loss);
            if p.take_profit.is_none() {
                p.take_profit = j.take_profit;
            }
        } else {
            overlay.push(j);
        }
    }
    overlay
}



pub fn apply_tradfi_skip(state: &mut EngineState, extra: &[String]) {
    merge_skip(state, extra);
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

/// positionRisk 502 must not look like a flat book — that double-buys the still-open long.
fn restore_prior_book(
    prior: Option<&MarketSnapshot>,
    held_positions: Option<&[Position]>,
    held_position: Option<Position>,
    live_book: &mut bool,
    open_positions: &mut Vec<Position>,
    position: &mut Option<Position>,
) {
    if let Some(held) = held_positions {
        if open_positions.is_empty() {
            *open_positions = held.to_vec();
            *position = held_position;
        }
    }
    let Some(prior) = prior else {
        return;
    };
    if !prior.live_book {
        return;
    }
    *live_book = true;
    if open_positions.is_empty() {
        *open_positions = prior.open_positions.clone();
        *position = prior.position.clone();
    }
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

fn chart_spec(strategy_id: i32, s4: crate::config::TradeInterval) -> (&'static str, usize) {
    if strategy_id == 3 {
        (CHART_INTERVAL, CHART_LIMIT)
    } else if strategy_id == 4 {
        (s4.as_binance(), s4.chart_limit())
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
    scan_due: bool,
    interval: crate::config::TradeInterval,
) -> (
    HashMap<String, Bar>,
    HashMap<String, Vec<Bar>>,
    HashMap<String, Vec<Bar>>,
) {
    let mut last_bars = HashMap::new();
    let mut universe = HashMap::new();
    let mut htf_bars = HashMap::new();
    if !chart_symbol.is_empty() && !chart_bars.is_empty() {
        universe.insert(chart_symbol.to_string(), chart_bars.to_vec());
        if let Some(closed) = chart_bars.last() {
            last_bars.insert(chart_symbol.to_string(), closed.clone());
        }
    }
    let mut s4 = crate::continuation::ContinuationParams::default().with_interval(interval);
    s4.max_positions = n.max(1);
    let mut want: Vec<String> = if scan_due {
        crate::continuation::pick_strategy4_book(
            tickers,
            s4.liquid_n.max(1),
            &state.skip_symbols,
            Some(&s4),
        )
        .into_iter()
        .map(|t| t.symbol)
        .collect()
    } else {
        Vec::new()
    };
    for pos in remembered {
        if pos.qty > Decimal::ZERO
            && !want
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&pos.symbol))
        {
            want.push(pos.symbol.clone());
        }
    }
    let mut htf_want = want.clone();
    if !chart_symbol.is_empty()
        && !htf_want
            .iter()
            .any(|s| s.eq_ignore_ascii_case(chart_symbol))
    {
        htf_want.push(chart_symbol.to_string());
    }
    for symbol in want {
        if universe.contains_key(&symbol) {
            continue;
        }
        let limit = if remembered.iter().any(|p| p.symbol.eq_ignore_ascii_case(&symbol)) && !scan_due
        {
            6
        } else {
            interval.fetch_limit()
        };
        match closed_klines(client, &symbol, interval.as_binance(), limit) {
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
    for symbol in htf_want {
        match closed_klines(client, &symbol, "4h", 50) {
            Ok(extra) if !extra.is_empty() => {
                htf_bars.insert(symbol, extra);
            }
            _ => {}
        }
    }
    (last_bars, universe, htf_bars)
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
        let mut s4 = crate::continuation::ContinuationParams::default();
        s4.max_positions = n.max(1);
        let book = crate::continuation::pick_strategy4_book(
            tickers,
            s4.liquid_n.max(1),
            &state.skip_symbols,
            Some(&s4),
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
    let mut htf_bars: HashMap<String, Vec<Bar>> = HashMap::new();
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
            if let Some(prior) = prior {
                tickers = prior.tickers.clone();
                bars = prior.bars.clone();
                last_bars = prior.last_bars.clone();
                universe_bars = prior.universe_bars.clone();
                htf_bars = prior.htf_bars.clone();
                chart_symbol = prior.chart_symbol.clone();
            }
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
                htf_bars,
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
            let mut s4 = crate::continuation::ContinuationParams::default()
                .with_interval(cfg.s4_interval);
            s4.max_positions = cfg.s4_max_positions;
            crate::continuation::pick_strategy4_book(
                &tickers,
                s4.liquid_n.max(1),
                skip,
                Some(&s4),
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
                    let sl_overlay = merge_overlay_with_journal(
                        remembered.clone(),
                        crate::journal::unmatched_open_positions(),
                    );
                    let managed = pick_managed_longs(&positions, &sl_overlay);
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
                    restore_prior_book(
                        prior,
                        held_positions.as_deref(),
                        held_position.clone(),
                        &mut live_book,
                        &mut open_positions,
                        &mut position,
                    );
                }
            },
            Err(exc) => {
                if error.is_none() {
                    error = Some(describe_exchange_error(&exc.0));
                }
                restore_prior_book(
                    prior,
                    held_positions.as_deref(),
                    held_position.clone(),
                    &mut live_book,
                    &mut open_positions,
                    &mut position,
                );
            }
        }
    }
    if !chart_symbol.is_empty() {
        let (interval, limit) = chart_spec(state.strategy_id, cfg.s4_interval);
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
        let (interval, limit) = chart_spec(state.strategy_id, cfg.s4_interval);
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
        let now = unix_now();
        let scan_due = state.last_scan_ts <= 0.0 || (now - state.last_scan_ts) >= 60.0;
        let (lb, ub, htf) = collect_s4_history(
            client,
            state,
            &tickers,
            &chart_symbol,
            &bars,
            &remembered,
            cfg.s4_max_positions,
            scan_due,
            cfg.s4_interval,
        );
        if scan_due {
            last_bars = lb;
            universe_bars.extend(ub);
            htf_bars = htf;
        } else {
            // Between scans keep the last book klines. Replacing with the
            // chart-only stub made the next due scan see "нет 4ч истории".
            if let Some(prev) = prior {
                last_bars = prev.last_bars.clone();
                universe_bars.extend(prev.universe_bars.clone());
                htf_bars = prev.htf_bars.clone();
            }
            last_bars.extend(lb);
            universe_bars.extend(ub);
            for (sym, rows) in htf {
                htf_bars.insert(sym, rows);
            }
        }
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
        htf_bars,
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

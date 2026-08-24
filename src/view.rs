//! Assemble the TUI ViewModel from config + engine + snapshot.

use crate::config::Config;
use crate::errorlog::guess_source;
use crate::errors::is_retry_error;
use crate::models::{unmanaged_positions, EngineState, MarketSnapshot, Position};
use crate::ranking::{iter_liquid_majors, pick_strategy1_book};
use crate::render::ViewModel;
use crate::signals::signals_enabled;
use rust_decimal::Decimal;

pub fn view_positions(snapshot: &MarketSnapshot) -> Vec<Position> {
    if !snapshot.open_positions.is_empty() {
        return snapshot.open_positions.clone();
    }
    if let Some(pos) = &snapshot.position {
        if pos.qty > Decimal::ZERO {
            return vec![pos.clone()];
        }
    }
    Vec::new()
}

pub fn basket_symbols(cfg: &Config, state: &EngineState, snapshot: &MarketSnapshot) -> Vec<String> {
    if state.strategy_id == 1 {
        pick_strategy1_book(
            &snapshot.tickers,
            cfg.max_positions.max(1) as usize,
            &state.skip_symbols,
        )
        .into_iter()
        .map(|t| t.symbol)
        .collect()
    } else if state.strategy_id == 4 {
        crate::continuation::pick_strategy4_book(
            &snapshot.tickers,
            cfg.max_positions.max(1) as usize,
            &state.skip_symbols,
            None,
        )
        .into_iter()
        .map(|t| t.symbol)
        .collect()
    } else {
        iter_liquid_majors(&snapshot.tickers, &state.skip_symbols)
            .into_iter()
            .map(|t| t.symbol)
            .collect()
    }
}

fn hide_poll_retry(snapshot: &MarketSnapshot) -> bool {
    snapshot
        .last_error
        .as_deref()
        .map(|e| snapshot.account_ok && is_retry_error(Some(e)))
        .unwrap_or(false)
}

fn footer_errors(snapshot: &MarketSnapshot, state: &EngineState) -> (Option<String>, Option<String>, String) {
    let poll = snapshot.last_error.clone();
    let live = state.last_error.clone();
    if let Some(p) = &poll {
        if !hide_poll_retry(snapshot) {
            return (Some(p.clone()), Some(p.clone()), "poll".into());
        }
    }
    if let Some(l) = live {
        let source = guess_source(&l, "live");
        return (Some(l.clone()), Some(l), source);
    }
    if let Some(p) = poll {
        return (None, Some(p), "poll".into());
    }
    (None, None, String::new())
}

pub fn build_view(
    cfg: &Config,
    state: &EngineState,
    snapshot: &MarketSnapshot,
    last_decision: &str,
    flatten_armed: bool,
) -> ViewModel {
    let acc = &snapshot.account;
    let mut note = if cfg.live {
        "LIVE TestNet: ордера разрешены.".to_string()
    } else {
        "Режим просмотра: ордера не отправляются (добавьте --live и ключи в env).".to_string()
    };
    if cfg.credentials.is_none() {
        note.push_str(" BINANCE_API_KEY/SECRET не заданы.");
    }
    let (ui_error, logged_error, error_source) = footer_errors(snapshot, state);
    let shown = view_positions(snapshot);
    let tail = unmanaged_positions(&shown, &state.positions);
    let day_pnl = state.day_start_equity.map(|start| acc.wallet_balance + acc.unrealized_pnl - start);
    ViewModel {
        strategy_id: state.strategy_id,
        wallet_balance: acc.wallet_balance,
        unrealized_pnl: acc.unrealized_pnl,
        starting_equity: acc.starting_equity,
        available_balance: acc.available_balance,
        positions: shown,
        recent_actions: state.recent_actions.clone(),
        tickers: snapshot.tickers.clone(),
        chart_symbol: snapshot.chart_symbol.clone(),
        chart_closes: snapshot.bars.iter().map(|b| b.close).collect(),
        last_error: ui_error,
        logged_error,
        live: cfg.live,
        has_credentials: cfg.credentials.is_some(),
        poll_seconds: cfg.poll_seconds,
        last_decision: last_decision.to_string(),
        mode_note: note,
        flatten_armed,
        entries_paused: state.entries_paused,
        now_ts: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
        ),
        entry_windows: if state.strategy_id == 4 {
            cfg.s4_entry_windows.clone()
        } else {
            cfg.entry_windows.clone()
        },
        always_enter: if state.strategy_id == 4 {
            cfg.s4_always_enter
        } else {
            cfg.always_enter
        },
        signals_on: signals_enabled(),
        journal_lines: Vec::new(),
        leverage: cfg.leverage,
        order_notional: cfg.order_notional,
        notional_from_exchange: cfg.notional_from_exchange,
        max_positions: cfg.max_positions,
        basket_symbols: basket_symbols(cfg, state, snapshot),
        cooldown_until: state.cooldown_until,
        cooldowns: state.cooldowns.clone(),
        error_source,
        unmanaged_symbols: tail.iter().map(|p| p.symbol.clone()).collect(),
        flatten_leftovers: !tail.is_empty(),
        daily_halt: state.daily_halt,
        daily_loss_usdt: cfg.daily_loss_usdt,
        day_pnl,
    }
}

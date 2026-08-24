//! Walk-forward simulator. Same decide()/tick() as live; no orders.

use crate::engine::{tick, MomentumParams};
use crate::journal::{long_pnl, taker_fee};
use crate::models::{Account, Bar, Decision, EngineState, MarketSnapshot, Position, Side, Ticker};
use crate::scalp::ScalpParams;
use crate::trend::TrendParams;
use rust_decimal::Decimal;
use std::collections::HashMap;

pub fn default_notional() -> Decimal {
    Decimal::from(20)
}
pub fn default_slip() -> Decimal {
    Decimal::new(1, 4)
}

#[derive(Debug, Clone)]
pub struct ClosedTrade {
    pub symbol: String,
    pub strategy_id: i32,
    pub entry: Decimal,
    pub exit: Decimal,
    pub qty: Decimal,
    pub pnl: Decimal,
    pub fee: Decimal,
    pub reason: String,
    pub bars_held: usize,
}

#[derive(Debug, Clone)]
pub struct SimResult {
    pub name: String,
    pub strategy_id: i32,
    pub trades: Vec<ClosedTrade>,
    pub start_equity: Decimal,
    pub end_equity: Decimal,
    pub max_drawdown: Decimal,
    pub holds: usize,
}

impl SimResult {
    pub fn pnl(&self) -> Decimal {
        self.end_equity - self.start_equity
    }
    pub fn wins(&self) -> usize {
        self.trades.iter().filter(|t| t.pnl > Decimal::ZERO).count()
    }
    pub fn profit_factor(&self) -> Option<Decimal> {
        if self.trades.is_empty() {
            return None;
        }
        let mut gains = Decimal::ZERO;
        let mut losses = Decimal::ZERO;
        for t in &self.trades {
            if t.pnl > Decimal::ZERO {
                gains += t.pnl;
            } else if t.pnl < Decimal::ZERO {
                losses += -t.pnl;
            }
        }
        if losses <= Decimal::ZERO {
            return if gains > Decimal::ZERO {
                Some(Decimal::from(1000))
            } else {
                Some(Decimal::ZERO)
            };
        }
        Some(gains / losses)
    }
    pub fn summary_line(&self) -> String {
        let n = self.trades.len();
        let wr = if n == 0 {
            "—".into()
        } else {
            format!("{:.1}%", self.wins() as f64 / n as f64 * 100.0)
        };
        let pf = match self.profit_factor() {
            None => "—".into(),
            Some(v) if v >= Decimal::from(100) => "∞".into(),
            Some(v) => format!("{v:.2}"),
        };
        format!(
            "{:28} n={n:4}  wr={wr:>6}  pnl={:+.4}  pf={pf}  dd={:.4}",
            self.name,
            self.pnl(),
            self.max_drawdown
        )
    }
}

fn apply_slip(price: Decimal, buy: bool, slip: Decimal) -> Decimal {
    if slip <= Decimal::ZERO {
        return price;
    }
    if buy {
        price * (Decimal::ONE + slip)
    } else {
        price * (Decimal::ONE - slip)
    }
}

fn hit_protectives(pos: &Position, bar: &Bar) -> Option<(Decimal, String)> {
    let hit_sl = pos.stop_loss.map(|sl| bar.low <= sl).unwrap_or(false);
    let hit_tp = pos.take_profit.map(|tp| bar.high >= tp).unwrap_or(false);
    if hit_sl && hit_tp {
        return Some((pos.stop_loss.unwrap(), "stop (wick, both sides — assume SL)".into()));
    }
    if hit_sl {
        return Some((pos.stop_loss.unwrap(), "stop (wick)".into()));
    }
    if hit_tp {
        return Some((pos.take_profit.unwrap(), "take profit (wick)".into()));
    }
    None
}

pub fn change_percent(bars: &[Bar], index: usize, lookback: usize) -> Decimal {
    if index >= bars.len() {
        return Decimal::ZERO;
    }
    let prev_i = if index < lookback { 0 } else { index - lookback };
    let prev = bars[prev_i].close;
    let last = bars[index].close;
    if prev <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    (last - prev) / prev * Decimal::from(100)
}

pub fn simulate_bars(
    strategy_id: i32,
    bars: &[Bar],
    symbol: &str,
    name: &str,
    notional: Decimal,
    fee_rate: Decimal,
    slip: Decimal,
    warmup: Option<usize>,
    start_equity: Decimal,
    momentum: Option<&MomentumParams>,
    scalp: Option<&ScalpParams>,
    trend: Option<&TrendParams>,
) -> SimResult {
    let warmup = warmup.unwrap_or(if strategy_id == 2 {
        80
    } else if strategy_id == 3 {
        70
    } else {
        40
    });
    let mut result = SimResult {
        name: if name.is_empty() {
            format!("s{strategy_id}:{symbol}")
        } else {
            name.into()
        },
        strategy_id,
        trades: Vec::new(),
        start_equity,
        end_equity: start_equity,
        max_drawdown: Decimal::ZERO,
        holds: 0,
    };
    if bars.len() <= warmup + 2 {
        return result;
    }
    let lookback = if bars.len() >= 2 && bars[1].open_time - bars[0].open_time <= 60_000 {
        1440
    } else {
        288
    };
    let mut state = EngineState::new(strategy_id);
    let mut pos: Option<Position> = None;
    let mut pending: Option<Decision> = None;
    let mut peak = start_equity;
    let mut equity = start_equity;
    let window = 120;

    for i in warmup..bars.len() {
        let bar = &bars[i];
        let now = bar.open_time as f64 / 1000.0;
        if pending.is_some() && pos.is_none() {
            if let Some(Decision::EnterLong {
                symbol: s,
                stop_loss,
                take_profit,
                ..
            }) = pending.take()
            {
                let px = apply_slip(bar.open, true, slip);
                let qty = notional / px;
                pos = Some(Position {
                    symbol: s,
                    side: Side::Long,
                    qty,
                    entry_price: px,
                    stop_loss: Some(stop_loss),
                    take_profit: Some(take_profit),
                    unrealized_pnl: Decimal::ZERO,
                    opened_bar_time: Some(bar.open_time),
                    leverage: 0,
                });
                state.position = pos.clone();
            }
        }
        if let Some(p) = &pos {
            if let Some((px, reason)) = hit_protectives(p, bar) {
                let exit_px = apply_slip(px, false, slip);
                let (pnl, fee) = long_pnl(p.entry_price, exit_px, p.qty, fee_rate);
                result.trades.push(ClosedTrade {
                    symbol: p.symbol.clone(),
                    strategy_id,
                    entry: p.entry_price,
                    exit: exit_px,
                    qty: p.qty,
                    pnl,
                    fee,
                    reason,
                    bars_held: 0,
                });
                equity += pnl;
                if equity > peak {
                    peak = equity;
                }
                let dd = peak - equity;
                if dd > result.max_drawdown {
                    result.max_drawdown = dd;
                }
                pos = None;
                state.position = None;
                continue;
            }
        }
        let start = i.saturating_add(1).saturating_sub(window);
        let chunk = bars[start..=i].to_vec();
        let chg = change_percent(bars, i, lookback);
        let week_lb = lookback.saturating_mul(7).min(i);
        let week_chg = change_percent(bars, i, week_lb.max(1));
        let window_high = chunk
            .iter()
            .map(|b| b.high)
            .max()
            .unwrap_or(bar.high);
        let mut ticker = Ticker::new(
            symbol,
            bar.close,
            chg,
            Decimal::from(50_000_000),
        );
        ticker.high_price = window_high;
        ticker.week_change_percent = week_chg;
        let tickers = vec![ticker];
        let mut snap = MarketSnapshot::empty(equity);
        snap.tickers = tickers;
        snap.bars = chunk;
        snap.account = Account {
            wallet_balance: equity,
            unrealized_pnl: Decimal::ZERO,
            available_balance: equity,
            starting_equity: equity,
        };
        snap.position = pos.clone();
        snap.chart_symbol = symbol.into();
        snap.fetched = true;
        snap.live_book = true;
        snap.open_positions = pos.clone().into_iter().collect();
        snap.account_ok = true;
        let (new_state, decision) = tick(&state, &snap, now, momentum, scalp, trend);
        state = new_state;
        match decision {
            Decision::EnterLong { .. } if pos.is_none() => pending = Some(decision),
            Decision::ExitPosition { reason, .. } if pos.is_some() => {
                let p = pos.take().unwrap();
                let exit_px = apply_slip(bar.close, false, slip);
                let (pnl, fee) = long_pnl(p.entry_price, exit_px, p.qty, fee_rate);
                result.trades.push(ClosedTrade {
                    symbol: p.symbol.clone(),
                    strategy_id,
                    entry: p.entry_price,
                    exit: exit_px,
                    qty: p.qty,
                    pnl,
                    fee,
                    reason,
                    bars_held: 0,
                });
                equity += pnl;
                state.position = None;
            }
            Decision::AmendStop { stop_loss, .. } if pos.is_some() => {
                if let Some(p) = pos.as_mut() {
                    p.stop_loss = Some(stop_loss);
                    state.position = Some(p.clone());
                }
            }
            _ => result.holds += 1,
        }
    }
    if let Some(p) = pos {
        let last = bars.last().unwrap();
        let exit_px = apply_slip(last.close, false, slip);
        let (pnl, fee) = long_pnl(p.entry_price, exit_px, p.qty, fee_rate);
        result.trades.push(ClosedTrade {
            symbol: p.symbol,
            strategy_id,
            entry: p.entry_price,
            exit: exit_px,
            qty: p.qty,
            pnl,
            fee,
            reason: "end of series".into(),
            bars_held: 0,
        });
        equity += pnl;
    }
    result.end_equity = equity;
    let _ = (taker_fee(), HashMap::<String, Bar>::new());
    result
}

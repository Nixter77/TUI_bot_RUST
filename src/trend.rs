//! Daily trend: Turtle-style Donchian 40/20, long-only.

use crate::indicators::{channel_high, channel_low, last_adx, last_atr, last_ema};
use crate::models::{Bar, Decision, Position, Side};
use crate::sessions::{in_entry_window, HourWindow};
use crate::trail::{long_stop_is_valid, trail_stop_upward};
use rust_decimal::Decimal;

pub const CHART_INTERVAL: &str = "1d";
/// Closed daily bars. EMA100 + Donchian 40 need >101; forming bar is dropped.
pub const CHART_LIMIT: usize = 150;
/// Last closed 1d bar is the signal. Do not chase it all the next UTC day.
pub const ENTRY_GRACE_SEC: f64 = 2.0 * 3_600.0;
pub const DAY_SEC: f64 = 86_400.0;
/// Skip 2-ATR stops that are a third of the coin (TestNet microcap pumps).
pub fn max_stop_pct() -> Decimal {
    Decimal::new(10, 2)
}
/// Live ticker may not have already run away from the closed daily close.
pub fn max_close_extension_pct() -> Decimal {
    Decimal::new(3, 2)
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrendParams {
    pub channel: usize,
    pub exit_channel: usize,
    pub atr_period: usize,
    pub sl_atr: Decimal,
    pub min_stop_pct: Decimal,
    pub max_stop_pct: Decimal,
    pub trail_atr: Decimal,
    pub reward_r: Decimal,
    pub ema_filter: usize,
    pub adx_period: usize,
    pub adx_min: Decimal,
    pub cooldown_sec: f64,
    /// 0 = off (tests). Live default is `ENTRY_GRACE_SEC`.
    pub entry_grace_sec: f64,
    pub entry_windows: Vec<HourWindow>,
}

impl Default for TrendParams {
    fn default() -> Self {
        Self {
            // 90d+ of 1d majors: 20/10 EMA50 is +EV on the bull train and −EV
            // on the last 30% (chop). 40/20 + EMA100 stayed +EV on both splits.
            channel: 40,
            exit_channel: 20,
            atr_period: 20,
            sl_atr: Decimal::from(2),
            min_stop_pct: Decimal::new(6, 3),
            max_stop_pct: max_stop_pct(),
            trail_atr: Decimal::new(25, 1),
            reward_r: Decimal::from(8),
            ema_filter: 100,
            adx_period: 14,
            adx_min: Decimal::ZERO,
            cooldown_sec: 3600.0,
            entry_grace_sec: ENTRY_GRACE_SEC,
            entry_windows: Vec::new(),
        }
    }
}

/// Unix seconds when the daily bar that opened at `open_time_ms` closes.
pub fn daily_bar_close_ts(open_time_ms: i64) -> f64 {
    (open_time_ms as f64) / 1000.0 + DAY_SEC
}

/// Live last vs the closed daily close that formed the Donchian signal.
pub fn live_left_daily_close(
    signal_close: Decimal,
    live_mark: Decimal,
    max_ext: Decimal,
) -> Option<&'static str> {
    if signal_close <= Decimal::ZERO || live_mark <= Decimal::ZERO {
        return Some("нет цены");
    }
    if live_mark < signal_close {
        return Some("цена ниже закрытия пробоя");
    }
    if max_ext > Decimal::ZERO && live_mark > signal_close * (Decimal::ONE + max_ext) {
        return Some("цена ушла от закрытия дня");
    }
    None
}

pub fn trend_decision(
    bars: &[Bar],
    position: Option<&Position>,
    symbol: &str,
    params: Option<&TrendParams>,
    now: Option<f64>,
) -> Decision {
    let owned = TrendParams::default();
    let p = params.unwrap_or(&owned);
    let need = (p.channel + 2)
        .max(p.exit_channel + 2)
        .max(p.ema_filter + 1)
        .max(p.atr_period + 2)
        .max(2 * p.adx_period + 1);
    if bars.len() < need {
        return Decision::hold("not enough bars for trend");
    }
    let last = &bars[bars.len() - 1];
    let mark = last.close;
    if mark <= Decimal::ZERO {
        return Decision::hold("invalid mark");
    }
    let Some(atr) = last_atr(bars, p.atr_period).filter(|a| *a > Decimal::ZERO) else {
        return Decision::hold("trend ATR unavailable");
    };
    if let Some(pos) = position {
        if pos.qty > Decimal::ZERO {
            if pos.side != Side::Long {
                return Decision::hold("trend is buy-only; short not managed");
            }
            return manage_long(bars, pos, mark, atr, p);
        }
    }

    let ts = last.open_time as f64 / 1000.0;
    if !in_entry_window(ts, Some(&p.entry_windows), false) {
        return Decision::hold("вне сессии тренда");
    }
    if p.entry_grace_sec > 0.0 {
        let close_ts = daily_bar_close_ts(last.open_time);
        let now = now.unwrap_or(close_ts);
        // Live snapshot already dropped the forming 1d bar, so now is after close_ts.
        // The sim still passes the in-progress bar (now < close_ts) — do not block it.
        if now + 1.0 >= close_ts && now - close_ts > p.entry_grace_sec {
            return Decision::hold("пробой не на закрытии дня");
        }
    }
    if last.close <= last.open {
        return Decision::hold("нет подтверждения (красная свеча)");
    }
    let Some(prior_high) = channel_high(bars, p.channel, true) else {
        return Decision::hold("Donchian недоступен");
    };
    if mark <= prior_high {
        return Decision::hold(format!("нет пробоя Donchian {}", p.channel));
    }
    if p.ema_filter > 0 {
        let closes: Vec<Decimal> = bars.iter().map(|b| b.close).collect();
        let ema = last_ema(&closes, p.ema_filter);
        if ema.is_none() || mark <= ema.unwrap() {
            return Decision::hold("ниже EMA фильтра");
        }
    }
    if p.adx_min > Decimal::ZERO {
        let adx = last_adx(bars, p.adx_period);
        if adx.is_none() || adx.unwrap() < p.adx_min {
            return Decision::hold("нет тренда (ADX)");
        }
    }

    let mut sl = mark - p.sl_atr * atr;
    sl = at_least_min_stop(mark, sl, p.min_stop_pct);
    if !long_stop_is_valid(sl, mark) {
        return Decision::hold("trend stop invalid");
    }
    let risk = mark - sl;
    if risk <= Decimal::ZERO {
        return Decision::hold("risk is zero");
    }
    if p.max_stop_pct > Decimal::ZERO && risk > mark * p.max_stop_pct {
        return Decision::hold("стоп слишком широкий");
    }
    let tp = mark + p.reward_r * risk;
    Decision::EnterLong {
        symbol: symbol.to_string(),
        reason: format!("тренд: пробой Donchian {}", p.channel),
        take_profit: tp,
        stop_loss: sl,
    }
}

fn at_least_min_stop(mark: Decimal, sl: Decimal, min_pct: Decimal) -> Decimal {
    if min_pct <= Decimal::ZERO {
        return sl;
    }
    let floor = mark * (Decimal::ONE - min_pct);
    if sl <= floor {
        sl
    } else {
        floor
    }
}

fn manage_long(
    bars: &[Bar],
    position: &Position,
    mark: Decimal,
    atr: Decimal,
    p: &TrendParams,
) -> Decision {
    let sl = position.stop_loss;
    let sym = position.symbol.clone();
    if let Some(sl) = sl {
        if mark <= sl {
            return Decision::ExitPosition {
                reason: "trend stop loss".into(),
                symbol: sym,
            };
        }
    }
    if let Some(exit_low) = channel_low(bars, p.exit_channel, true) {
        if mark < exit_low {
            return Decision::ExitPosition {
                reason: format!("trend broken (Donchian {})", p.exit_channel),
                symbol: sym,
            };
        }
    }
    if let Some(tp) = position.take_profit {
        if mark >= tp {
            return Decision::ExitPosition {
                reason: "trend take profit".into(),
                symbol: sym,
            };
        }
    }
    let opened = position.opened_bar_time;
    let held: Vec<&Bar> = bars
        .iter()
        .filter(|b| opened.map(|t| b.open_time >= t).unwrap_or(true))
        .collect();
    let mut peak = held.iter().map(|b| b.high).max().unwrap_or(mark);
    if peak < mark {
        peak = mark;
    }
    let chandelier = peak - p.trail_atr * atr;
    if sl.is_none() {
        if long_stop_is_valid(chandelier, mark) {
            return Decision::AmendStop {
                stop_loss: chandelier,
                reason: "trend attach stop".into(),
                symbol: sym,
            };
        }
        return Decision::hold("trend hold, cannot attach stop");
    }
    let sl = sl.unwrap();
    if let Ok(new_sl) = trail_stop_upward(Some(sl), chandelier, "LONG") {
        if new_sl > sl && long_stop_is_valid(new_sl, mark) {
            return Decision::AmendStop {
                stop_loss: new_sl,
                reason: "trend chandelier trail".into(),
                symbol: sym,
            };
        }
    }
    Decision::hold("trend hold")
}

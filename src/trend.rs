//! Daily trend: Turtle Donchian 20/10, long-only.

use crate::indicators::{channel_high, channel_low, last_adx, last_atr, last_ema};
use crate::models::{Bar, Decision, Position, Side};
use crate::sessions::{in_entry_window, HourWindow};
use crate::trail::{long_stop_is_valid, trail_stop_upward};
use rust_decimal::Decimal;

pub const CHART_INTERVAL: &str = "1d";
pub const CHART_LIMIT: usize = 90;

#[derive(Debug, Clone, PartialEq)]
pub struct TrendParams {
    pub channel: usize,
    pub exit_channel: usize,
    pub atr_period: usize,
    pub sl_atr: Decimal,
    pub min_stop_pct: Decimal,
    pub trail_atr: Decimal,
    pub reward_r: Decimal,
    pub ema_filter: usize,
    pub adx_period: usize,
    pub adx_min: Decimal,
    pub cooldown_sec: f64,
    pub entry_windows: Vec<HourWindow>,
}

impl Default for TrendParams {
    fn default() -> Self {
        Self {
            channel: 20,
            exit_channel: 10,
            atr_period: 20,
            sl_atr: Decimal::from(2),
            min_stop_pct: Decimal::new(6, 3),
            trail_atr: Decimal::new(25, 1),
            reward_r: Decimal::from(8),
            ema_filter: 50,
            adx_period: 14,
            adx_min: Decimal::ZERO,
            cooldown_sec: 3600.0,
            entry_windows: Vec::new(),
        }
    }
}

pub fn trend_decision(bars: &[Bar], position: Option<&Position>, symbol: &str, params: Option<&TrendParams>) -> Decision {
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
    if last.close <= last.open {
        return Decision::hold("нет подтверждения (красная свеча)");
    }
    let Some(prior_high) = channel_high(bars, p.channel, true) else {
        return Decision::hold("Donchian недоступен");
    };
    if mark <= prior_high {
        return Decision::hold("нет пробоя Donchian 20");
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
    let tp = mark + p.reward_r * risk;
    Decision::EnterLong {
        symbol: symbol.to_string(),
        reason: "тренд: пробой Donchian 20".into(),
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

fn manage_long(bars: &[Bar], position: &Position, mark: Decimal, atr: Decimal, p: &TrendParams) -> Decision {
    let sl = position.stop_loss;
    if let Some(sl) = sl {
        if mark <= sl {
            return Decision::ExitPosition {
                reason: "trend stop loss".into(),
                symbol: String::new(),
            };
        }
    }
    if let Some(exit_low) = channel_low(bars, p.exit_channel, true) {
        if mark < exit_low {
            return Decision::ExitPosition {
                reason: "trend broken (Donchian 10)".into(),
                symbol: String::new(),
            };
        }
    }
    if let Some(tp) = position.take_profit {
        if mark >= tp {
            return Decision::ExitPosition {
                reason: "trend take profit".into(),
                symbol: String::new(),
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
                symbol: String::new(),
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
                symbol: String::new(),
            };
        }
    }
    Decision::hold("trend hold")
}



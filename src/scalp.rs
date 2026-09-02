//! 1m-class VWAP + EMA9 pullback scalp.

use crate::config::{Config, DEFAULT_S2_MAX_HOLD_BARS};
use crate::indicators::{ema_series, last_atr, last_ema, last_rsi, mean_volume, vwap};
use crate::models::{Decision, Position, Side};
use crate::models::Bar;
use crate::sessions::{in_entry_window, HourWindow};
use crate::trail::{long_stop_is_valid, trail_stop_upward};
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq)]
pub struct ScalpParams {
    pub ema_fast: usize,
    pub ema_slow: usize,
    pub atr_period: usize,
    pub rsi_period: usize,
    pub rsi_min: Decimal,
    pub rsi_max: Decimal,
    pub reward_r: Decimal,
    pub sl_atr: Decimal,
    pub min_stop_pct: Decimal,
    pub trail_atr: Decimal,
    pub pullback_bars: usize,
    pub pullback_atr: Decimal,
    pub extend_atr: Decimal,
    pub min_atr_pct: Decimal,
    pub max_atr_pct: Decimal,
    pub min_volume_frac: Decimal,
    pub max_hold_bars: usize,
    pub cooldown_sec: f64,
    pub entry_windows: Vec<HourWindow>,
    pub always_enter: bool,
}

impl Default for ScalpParams {
    fn default() -> Self {
        Self {
            ema_fast: 9,
            ema_slow: 21,
            atr_period: 14,
            rsi_period: 14,
            rsi_min: Decimal::from(40),
            rsi_max: Decimal::from(68),
            reward_r: Decimal::from(2),
            sl_atr: Decimal::new(12, 1),
            min_stop_pct: Decimal::new(25, 4),
            trail_atr: Decimal::ONE,
            pullback_bars: 5,
            pullback_atr: Decimal::new(35, 2),
            extend_atr: Decimal::new(9, 1),
            min_atr_pct: Decimal::new(4, 4),
            max_atr_pct: Decimal::new(8, 3),
            min_volume_frac: Decimal::new(8, 1),
            max_hold_bars: DEFAULT_S2_MAX_HOLD_BARS,
            cooldown_sec: 1200.0,
            // Empty = open (hour_in_windows). Live uses `from_config` + STRATEGY2_ENTRY_HOURS.
            entry_windows: Vec::new(),
            always_enter: false,
        }
    }
}

impl ScalpParams {
    /// Live/TUI knobs from env-backed `Config` (windows + max hold).
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            entry_windows: cfg.s2_entry_windows.clone(),
            always_enter: cfg.s2_always_enter,
            max_hold_bars: cfg.s2_max_hold_bars,
            ..Self::default()
        }
    }
}

pub fn scalp_decision(
    bars: &[Bar],
    position: Option<&Position>,
    symbol: &str,
    params: Option<&ScalpParams>,
    now: Option<f64>,
) -> Decision {
    let owned = ScalpParams::default();
    let p = params.unwrap_or(&owned);
    let need = p.ema_slow.max(p.atr_period).max(p.rsi_period) + p.pullback_bars + 2;
    if bars.len() < need {
        return Decision::hold("not enough bars for scalp");
    }
    let last = &bars[bars.len() - 1];
    let mark = last.close;
    if mark <= Decimal::ZERO {
        return Decision::hold("invalid mark");
    }

    if let Some(pos) = position {
        if pos.qty > Decimal::ZERO {
            if pos.side != Side::Long {
                return Decision::hold("scalp is buy-only; short not managed");
            }
            return manage_long(bars, pos, mark, p, now);
        }
    }

    let ts = now.unwrap_or(last.open_time as f64 / 1000.0);
    if !in_entry_window(ts, Some(&p.entry_windows), p.always_enter) {
        return Decision::hold("вне сессии скальпа (Лондон/Нью-Йорк)");
    }

    let closes: Vec<Decimal> = bars.iter().map(|b| b.close).collect();
    let ema_f = last_ema(&closes, p.ema_fast);
    let ema_s = last_ema(&closes, p.ema_slow);
    let atr = last_atr(bars, p.atr_period);
    let rsi = last_rsi(&closes, p.rsi_period);
    let session = vwap(bars);
    let (Some(ema_f), Some(ema_s), Some(atr), Some(rsi), Some(session)) =
        (ema_f, ema_s, atr, rsi, session)
    else {
        return Decision::hold("scalp indicators unavailable");
    };
    if atr <= Decimal::ZERO {
        return Decision::hold("atr is zero");
    }
    let atr_pct = atr / mark;
    if atr_pct < p.min_atr_pct {
        return Decision::hold("слишком тихо для скальпа");
    }
    if atr_pct > p.max_atr_pct {
        return Decision::hold("слишком широко — не скальп");
    }
    if ema_f <= ema_s {
        return Decision::hold("нет микротренда вверх");
    }
    if mark <= session {
        return Decision::hold("ниже VWAP — лонг не беру");
    }
    if rsi < p.rsi_min {
        return Decision::hold("RSI слабый");
    }
    if rsi > p.rsi_max {
        return Decision::hold("RSI перекуплен — не догоняю");
    }
    if last.close <= last.open {
        return Decision::hold("сигнальная свеча не зелёная");
    }
    if last.close <= bars[bars.len() - 2].close {
        return Decision::hold("нет более высокого close");
    }
    if last.close < ema_f {
        return Decision::hold("close ниже EMA9");
    }
    let stretch = mark - ema_f;
    if stretch > p.extend_atr * atr {
        return Decision::hold("далеко от EMA — догон");
    }
    if !pulled_into(bars, session, atr, p) {
        return Decision::hold("нет отката к EMA/VWAP");
    }
    let start_vol = bars.len().saturating_sub(20);
    if let Some(avg_vol) = mean_volume(&bars[start_vol..]) {
        if avg_vol > Decimal::ZERO && last.volume < avg_vol * p.min_volume_frac {
            return Decision::hold("объём слабый");
        }
    }

    let mut sl = entry_stop(bars, mark, ema_f, atr, p);
    sl = at_least_min_stop(mark, sl, p.min_stop_pct);
    if !long_stop_is_valid(sl, mark) {
        return Decision::hold("stop would be at or above mark");
    }
    let risk = mark - sl;
    if risk <= Decimal::ZERO {
        return Decision::hold("risk is zero");
    }
    let tp = mark + p.reward_r * risk;
    Decision::EnterLong {
        symbol: symbol.to_string(),
        reason: "скальп: откат к VWAP/EMA9".into(),
        take_profit: tp,
        stop_loss: sl,
    }
}

fn pulled_into(bars: &[Bar], session_vwap: Decimal, atr: Decimal, p: &ScalpParams) -> bool {
    let closes: Vec<Decimal> = bars.iter().map(|b| b.close).collect();
    let emas = ema_series(&closes, p.ema_fast);
    if bars.len() < p.pullback_bars + 1 {
        return false;
    }
    let start = bars.len() - p.pullback_bars - 1;
    let band = p.pullback_atr * atr;
    let mut dipped = false;
    let mut tagged = false;
    for i in start..bars.len() - 1 {
        let Some(ema_i) = emas[i] else {
            continue;
        };
        let bar = &bars[i];
        if bar.close < ema_i {
            dipped = true;
        }
        let level = if ema_i > session_vwap {
            ema_i
        } else {
            session_vwap
        };
        if bar.low <= level + band {
            tagged = true;
        }
    }
    dipped && tagged
}

fn entry_stop(bars: &[Bar], mark: Decimal, ema_fast: Decimal, atr: Decimal, p: &ScalpParams) -> Decimal {
    let start = bars.len().saturating_sub(p.pullback_bars + 1);
    let window = &bars[start..];
    let swing = window.iter().map(|b| b.low).min().unwrap_or(mark);
    let atr_stop = mark - p.sl_atr * atr;
    let under_ema = ema_fast - Decimal::new(25, 2) * atr;
    let candidates: Vec<Decimal> = [swing, atr_stop, under_ema]
        .into_iter()
        .filter(|s| *s < mark)
        .collect();
    candidates.into_iter().max().unwrap_or(atr_stop)
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

fn peak_since_entry(bars: &[Bar], pos: &Position, mark: Decimal) -> Decimal {
    let mut peak = mark;
    if pos.qty > Decimal::ZERO && pos.unrealized_pnl > Decimal::ZERO {
        let implied = pos.entry_price + pos.unrealized_pnl / pos.qty;
        if implied > peak {
            peak = implied;
        }
    }
    for b in bars {
        let after = match pos.opened_bar_time {
            Some(since) => b.open_time >= since,
            None => true,
        };
        if after && b.high > peak {
            peak = b.high;
        }
    }
    peak
}

fn manage_long(
    bars: &[Bar],
    position: &Position,
    mark: Decimal,
    p: &ScalpParams,
    now: Option<f64>,
) -> Decision {
    let entry = position.entry_price;
    let sl = position.stop_loss;
    let tp = position.take_profit;
    if let Some(tp) = tp {
        if mark >= tp {
            return Decision::ExitPosition {
                reason: "scalp take profit".into(),
                symbol: String::new(),
            };
        }
    }
    if let Some(sl) = sl {
        if mark <= sl {
            return Decision::ExitPosition {
                reason: "scalp stop loss".into(),
                symbol: String::new(),
            };
        }
    }
    // End of scalp session: flatten open long (mirror S4 «конец окна входа»).
    let ts = now.unwrap_or_else(|| {
        bars.last()
            .map(|b| b.open_time as f64 / 1000.0)
            .unwrap_or(0.0)
    });
    if !p.always_enter
        && !p.entry_windows.is_empty()
        && !in_entry_window(ts, Some(&p.entry_windows), false)
    {
        return Decision::ExitPosition {
            reason: "конец сессии".into(),
            symbol: String::new(),
        };
    }
    if let Some(opened) = position.opened_bar_time {
        let held = bars.iter().filter(|b| b.open_time > opened).count();
        if held >= p.max_hold_bars {
            return Decision::ExitPosition {
                reason: "scalp time stop".into(),
                symbol: String::new(),
            };
        }
    }
    if bars.len() >= 2 {
        let prev = &bars[bars.len() - 2];
        let last = &bars[bars.len() - 1];
        let two_red = prev.close < prev.open && last.close < last.open;
        let session = vwap(bars);
        let lost_vwap = session.map(|s| last.close < s).unwrap_or(false);
        if two_red && last.close < entry && lost_vwap {
            return Decision::ExitPosition {
                reason: "scalp reversal".into(),
                symbol: String::new(),
            };
        }
    }

    let atr = last_atr(bars, p.atr_period);
    let Some(atr) = atr.filter(|a| *a > Decimal::ZERO) else {
        if sl.is_none() {
            return Decision::hold("scalp hold, no atr for stop");
        }
        return Decision::hold("scalp hold");
    };
    let risk = if let Some(sl) = sl {
        if sl < entry {
            entry - sl
        } else {
            p.sl_atr * atr
        }
    } else {
        p.sl_atr * atr
    };

    // Pre-full-BE peak giveback: peak≥0.8R, mark < entry+0.25R.
    if let Some(cur_sl) = sl {
        if cur_sl < entry && risk > Decimal::ZERO {
            let peak = peak_since_entry(bars, position, mark);
            let peak_08 = entry + Decimal::new(8, 1) * risk;
            let near_025 = entry + Decimal::new(25, 2) * risk;
            if peak >= peak_08 && mark < near_025 {
                let lock_025 = near_025;
                if lock_025 > cur_sl && lock_025 < mark && long_stop_is_valid(lock_025, mark) {
                    return Decision::AmendStop {
                        stop_loss: lock_025,
                        reason: "откат с пика — замок 0.25R".into(),
                        symbol: String::new(),
                    };
                }
                return Decision::ExitPosition {
                    reason: "откат с пика".into(),
                    symbol: String::new(),
                };
            }
        }
    }

    // Existing 1R BE + trail (unchanged geometry).
    let in_profit = risk > Decimal::ZERO && mark >= entry + risk;
    if in_profit {
        let breakeven = entry + atr * Decimal::new(5, 2);
        let trail = mark - p.trail_atr * atr;
        let candidate = if trail > breakeven { trail } else { breakeven };
        if let Ok(new_sl) = trail_stop_upward(sl, candidate, "LONG") {
            if sl.is_none() || (new_sl > sl.unwrap() && long_stop_is_valid(new_sl, mark)) {
                return Decision::AmendStop {
                    stop_loss: new_sl,
                    reason: "scalp trail / breakeven".into(),
                    symbol: String::new(),
                };
            }
        }
    } else if sl.is_none() {
        let attached = mark - p.sl_atr * atr;
        if long_stop_is_valid(attached, mark) {
            return Decision::AmendStop {
                stop_loss: attached,
                reason: "scalp attach stop".into(),
                symbol: String::new(),
            };
        }
    }
    Decision::hold("scalp hold")
}

//! Decimal OHLC indicators. No float, no lookahead beyond the last bar given.

use crate::models::Bar;
use rust_decimal::Decimal;

pub fn sma(values: &[Decimal], period: usize) -> Option<Decimal> {
    if period == 0 || values.len() < period {
        return None;
    }
    let window = &values[values.len() - period..];
    Some(window.iter().copied().sum::<Decimal>() / Decimal::from(period))
}

pub fn ema_series(values: &[Decimal], period: usize) -> Vec<Option<Decimal>> {
    let mut out = vec![None; values.len()];
    if period == 0 || values.len() < period {
        return out;
    }
    let seed = values[..period].iter().copied().sum::<Decimal>() / Decimal::from(period);
    out[period - 1] = Some(seed);
    let k = Decimal::TWO / (Decimal::from(period) + Decimal::ONE);
    let one_k = Decimal::ONE - k;
    let mut prev = seed;
    for i in period..values.len() {
        prev = values[i] * k + prev * one_k;
        out[i] = Some(prev);
    }
    out
}

pub fn last_ema(values: &[Decimal], period: usize) -> Option<Decimal> {
    ema_series(values, period).last().copied().flatten()
}

/// Pivot swing lows (`wing` bars on each side). Chronological.
pub fn swing_lows(bars: &[Bar], wing: usize) -> Vec<(usize, Decimal)> {
    let wing = wing.max(1);
    let n = bars.len();
    let mut out = Vec::new();
    if n < wing * 2 + 1 {
        return out;
    }
    for i in wing..n - wing {
        let lo = bars[i].low;
        if lo <= Decimal::ZERO {
            continue;
        }
        let mut is_low = true;
        for k in 1..=wing {
            if bars[i - k].low <= lo || bars[i + k].low <= lo {
                is_low = false;
                break;
            }
        }
        if is_low {
            out.push((i, lo));
        }
    }
    out
}

/// Last two swing lows (older, newer). None if fewer than two pivots.
pub fn last_two_swing_lows(bars: &[Bar]) -> Option<(Decimal, Decimal)> {
    let lows = swing_lows(bars, 1);
    if lows.len() < 2 {
        return None;
    }
    Some((lows[lows.len() - 2].1, lows[lows.len() - 1].1))
}

pub fn true_range(bar: &Bar, prev_close: Decimal) -> Decimal {
    let high_low = bar.high - bar.low;
    let high_close = (bar.high - prev_close).abs();
    let low_close = (bar.low - prev_close).abs();
    high_low.max(high_close).max(low_close)
}

pub fn atr_series(bars: &[Bar], period: usize) -> Vec<Option<Decimal>> {
    let mut out = vec![None; bars.len()];
    if period == 0 || bars.len() < period + 1 {
        return out;
    }
    let mut trs = Vec::new();
    for i in 1..bars.len() {
        trs.push(true_range(&bars[i], bars[i - 1].close));
    }
    let first = trs[..period].iter().copied().sum::<Decimal>() / Decimal::from(period);
    out[period] = Some(first);
    let mut prev = first;
    let n = Decimal::from(period);
    for i in (period + 1)..bars.len() {
        prev = (prev * (n - Decimal::ONE) + trs[i - 1]) / n;
        out[i] = Some(prev);
    }
    out
}

pub fn last_atr(bars: &[Bar], period: usize) -> Option<Decimal> {
    atr_series(bars, period).last().copied().flatten()
}

fn rsi_from_avgs(avg_g: Decimal, avg_l: Decimal) -> Decimal {
    if avg_l == Decimal::ZERO {
        return if avg_g > Decimal::ZERO {
            Decimal::from(100)
        } else {
            Decimal::from(50)
        };
    }
    let rs = avg_g / avg_l;
    Decimal::from(100) - (Decimal::from(100) / (Decimal::ONE + rs))
}

pub fn rsi_series(closes: &[Decimal], period: usize) -> Vec<Option<Decimal>> {
    let mut out = vec![None; closes.len()];
    if period == 0 || closes.len() < period + 1 {
        return out;
    }
    let mut gains = vec![Decimal::ZERO; closes.len()];
    let mut losses = vec![Decimal::ZERO; closes.len()];
    for i in 1..closes.len() {
        let delta = closes[i] - closes[i - 1];
        if delta > Decimal::ZERO {
            gains[i] = delta;
        } else if delta < Decimal::ZERO {
            losses[i] = -delta;
        }
    }
    let mut avg_g = gains[1..=period].iter().copied().sum::<Decimal>() / Decimal::from(period);
    let mut avg_l = losses[1..=period].iter().copied().sum::<Decimal>() / Decimal::from(period);
    out[period] = Some(rsi_from_avgs(avg_g, avg_l));
    let n = Decimal::from(period);
    for i in (period + 1)..closes.len() {
        avg_g = (avg_g * (n - Decimal::ONE) + gains[i]) / n;
        avg_l = (avg_l * (n - Decimal::ONE) + losses[i]) / n;
        out[i] = Some(rsi_from_avgs(avg_g, avg_l));
    }
    out
}

pub fn last_rsi(closes: &[Decimal], period: usize) -> Option<Decimal> {
    rsi_series(closes, period).last().copied().flatten()
}

pub fn adx_series(bars: &[Bar], period: usize) -> Vec<Option<Decimal>> {
    let mut out = vec![None; bars.len()];
    if period == 0 || bars.len() < 2 * period + 1 {
        return out;
    }
    let mut plus_dm = vec![Decimal::ZERO];
    let mut minus_dm = vec![Decimal::ZERO];
    let mut trs = vec![Decimal::ZERO];
    for i in 1..bars.len() {
        let up = bars[i].high - bars[i - 1].high;
        let down = bars[i - 1].low - bars[i].low;
        plus_dm.push(if up > down && up > Decimal::ZERO {
            up
        } else {
            Decimal::ZERO
        });
        minus_dm.push(if down > up && down > Decimal::ZERO {
            down
        } else {
            Decimal::ZERO
        });
        trs.push(true_range(&bars[i], bars[i - 1].close));
    }
    let n = Decimal::from(period);
    let mut sm_tr = trs[1..=period].iter().copied().sum::<Decimal>();
    let mut sm_p = plus_dm[1..=period].iter().copied().sum::<Decimal>();
    let mut sm_m = minus_dm[1..=period].iter().copied().sum::<Decimal>();
    let mut dx_vals: Vec<Decimal> = Vec::new();
    let hundred = Decimal::from(100);
    for i in (period + 1)..bars.len() {
        sm_tr = sm_tr - (sm_tr / n) + trs[i];
        sm_p = sm_p - (sm_p / n) + plus_dm[i];
        sm_m = sm_m - (sm_m / n) + minus_dm[i];
        if sm_tr <= Decimal::ZERO {
            dx_vals.push(Decimal::ZERO);
        } else {
            let plus_di = hundred * sm_p / sm_tr;
            let minus_di = hundred * sm_m / sm_tr;
            let denom = plus_di + minus_di;
            dx_vals.push(if denom == Decimal::ZERO {
                Decimal::ZERO
            } else {
                hundred * (plus_di - minus_di).abs() / denom
            });
        }
        if dx_vals.len() == period {
            let adx = dx_vals.iter().copied().sum::<Decimal>() / n;
            out[i] = Some(adx);
        } else if dx_vals.len() > period {
            let prev = out[i - 1].unwrap();
            let adx = (prev * (n - Decimal::ONE) + *dx_vals.last().unwrap()) / n;
            out[i] = Some(adx);
        }
    }
    out
}

pub fn last_adx(bars: &[Bar], period: usize) -> Option<Decimal> {
    adx_series(bars, period).last().copied().flatten()
}

pub fn vwap(bars: &[Bar]) -> Option<Decimal> {
    if bars.is_empty() {
        return None;
    }
    let three = Decimal::from(3);
    let mut num = Decimal::ZERO;
    let mut den = Decimal::ZERO;
    for bar in bars {
        let typical = (bar.high + bar.low + bar.close) / three;
        let vol = if bar.volume > Decimal::ZERO {
            bar.volume
        } else {
            Decimal::ONE
        };
        num += typical * vol;
        den += vol;
    }
    if den <= Decimal::ZERO {
        None
    } else {
        Some(num / den)
    }
}

pub fn mean_volume(bars: &[Bar]) -> Option<Decimal> {
    if bars.is_empty() {
        return None;
    }
    Some(bars.iter().map(|b| b.volume).sum::<Decimal>() / Decimal::from(bars.len()))
}

fn channel_window(bars: &[Bar], period: usize, exclude_last: bool) -> Option<&[Bar]> {
    if period == 0 {
        return None;
    }
    let end = if exclude_last {
        bars.len().saturating_sub(1)
    } else {
        bars.len()
    };
    if end < period {
        return None;
    }
    let start = end - period;
    if start >= end {
        return None;
    }
    Some(&bars[start..end])
}

pub fn channel_high(bars: &[Bar], period: usize, exclude_last: bool) -> Option<Decimal> {
    channel_window(bars, period, exclude_last).map(|w| w.iter().map(|b| b.high).max().unwrap())
}

pub fn channel_low(bars: &[Bar], period: usize, exclude_last: bool) -> Option<Decimal> {
    channel_window(bars, period, exclude_last).map(|w| w.iter().map(|b| b.low).min().unwrap())
}

//! Durable per-symbol open-trade meta: initial R, MFE/MAE, scale flag.
//! Survives restart under `.state/open_meta.json`. Never raises into the TUI.

use crate::errors::{ensure_private_dir, restrict_private_file};
use crate::models::{Bar, EngineState, MarketSnapshot, Position, Ticker};
use crate::money::{dec, fmt_fixed};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub const DEFAULT_OPEN_META_PATH: &str = ".state/open_meta.json";

static ACTIVE_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static STORE: LazyLock<Mutex<HashMap<String, OpenTradeMeta>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Pure R / excursion helpers (unit-tested).
pub fn initial_risk_usdt(entry: Decimal, stop_loss: Decimal, qty: Decimal) -> Option<Decimal> {
    if entry <= Decimal::ZERO || qty <= Decimal::ZERO {
        return None;
    }
    let dist = (entry - stop_loss).abs();
    if dist <= Decimal::ZERO {
        return None;
    }
    let risk = dist * qty;
    if risk > Decimal::ZERO {
        Some(risk)
    } else {
        None
    }
}

pub fn r_multiple(usdt: Decimal, risk: Decimal) -> Option<Decimal> {
    if risk <= Decimal::ZERO {
        None
    } else {
        Some(usdt / risk)
    }
}

/// Long mark PnL in USDT vs entry using *initial* qty (stable MFE/MAE base).
pub fn mark_pnl_usdt(entry: Decimal, mark: Decimal, initial_qty: Decimal) -> Decimal {
    (mark - entry) * initial_qty
}

/// Update MFE (max favorable) / MAE (max adverse, stored as positive USDT).
pub fn apply_mark_excursion(
    mfe_usdt: Decimal,
    mae_usdt: Decimal,
    mfe_peak_ts: Option<f64>,
    time_to_1r_ts: Option<f64>,
    entry: Decimal,
    mark: Decimal,
    initial_qty: Decimal,
    initial_risk: Decimal,
    now: f64,
) -> (Decimal, Decimal, Option<f64>, Option<f64>) {
    let pnl = mark_pnl_usdt(entry, mark, initial_qty);
    let mut mfe = mfe_usdt;
    let mut mae = mae_usdt;
    let mut peak = mfe_peak_ts;
    let mut t1r = time_to_1r_ts;
    if pnl > mfe {
        mfe = pnl;
        peak = Some(now);
    }
    if pnl < Decimal::ZERO {
        let adverse = -pnl;
        if adverse > mae {
            mae = adverse;
        }
    }
    if t1r.is_none() && initial_risk > Decimal::ZERO && pnl >= initial_risk {
        t1r = Some(now);
    }
    (mfe, mae, peak, t1r)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenTradeMeta {
    pub symbol: String,
    pub strategy_id: i32,
    pub entry: String,
    pub initial_sl: String,
    pub initial_qty: String,
    pub initial_risk_usdt: String,
    pub opened_ts: f64,
    #[serde(default)]
    pub mfe_usdt: String,
    #[serde(default)]
    pub mae_usdt: String,
    #[serde(default)]
    pub mfe_peak_ts: Option<f64>,
    #[serde(default)]
    pub time_to_1r_ts: Option<f64>,
    #[serde(default)]
    pub scaled_at_1r: bool,
    /// BTC regime tag at entry (phase-2); carried to journal closes.
    #[serde(default)]
    pub btc_regime: Option<String>,
}

impl OpenTradeMeta {
    pub fn entry_dec(&self) -> Option<Decimal> {
        dec(&self.entry).ok()
    }
    pub fn risk_dec(&self) -> Option<Decimal> {
        dec(&self.initial_risk_usdt).ok()
    }
    pub fn qty_dec(&self) -> Option<Decimal> {
        dec(&self.initial_qty).ok()
    }
    pub fn mfe_dec(&self) -> Decimal {
        dec(&self.mfe_usdt).unwrap_or(Decimal::ZERO)
    }
    pub fn mae_dec(&self) -> Decimal {
        dec(&self.mae_usdt).unwrap_or(Decimal::ZERO)
    }
}

/// Fields merged into a journal close (and optionally open) line.
#[derive(Debug, Clone, Default)]
pub struct CloseMetrics {
    pub initial_risk_usdt: Option<String>,
    pub initial_r: Option<String>,
    pub final_r: Option<String>,
    pub mfe_r: Option<String>,
    pub mae_r: Option<String>,
    pub mfe_usdt: Option<String>,
    pub mae_usdt: Option<String>,
    pub hold_sec: Option<i64>,
    pub time_to_mfe_sec: Option<i64>,
    pub time_to_1r_sec: Option<i64>,
    pub scaled_at_1r: Option<bool>,
    pub btc_regime: Option<String>,
}

/// Lean entry features — never block entry if missing.
#[derive(Debug, Clone, Default)]
pub struct EntrySnapshot {
    pub ret_24h: Option<String>,
    pub ret_1h: Option<String>,
    pub ret_4h: Option<String>,
    pub quote_volume: Option<String>,
    pub near_high_frac: Option<String>,
    pub pullback_pct: Option<String>,
    pub stop_distance: Option<String>,
    pub risk_pct: Option<String>,
    pub btc_ret_1h: Option<String>,
    pub btc_regime: Option<String>,
}

pub fn set_active_path(path: Option<PathBuf>) {
    *lock_poison(&ACTIVE_PATH) = path;
    // Reload from disk when path changes.
    let p = lock_poison(&ACTIVE_PATH).clone();
    let mut map = p.as_ref().map(|path| load_file(path)).unwrap_or_default();
    let dropped = sanitize_map(&mut map);
    *lock_poison(&STORE) = map;
    if dropped > 0 {
        persist_now();
    }
}

fn active_path() -> PathBuf {
    lock_poison(&ACTIVE_PATH)
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OPEN_META_PATH))
}

fn load_file(path: &Path) -> HashMap<String, OpenTradeMeta> {
    let Ok(text) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn persist_locked(store: &HashMap<String, OpenTradeMeta>) {
    let path = active_path();
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent);
    }
    let Ok(json) = serde_json::to_string_pretty(store) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, format!("{json}\n")).is_ok() {
        restrict_private_file(&tmp);
        let _ = fs::rename(&tmp, &path);
        restrict_private_file(&path);
    }
}

pub fn persist_now() {
    let store = lock_poison(&STORE);
    persist_locked(&store);
}

pub fn get(symbol: &str) -> Option<OpenTradeMeta> {
    let key = symbol.to_ascii_uppercase();
    lock_poison(&STORE).get(&key).cloned()
}

/// Drop garbage / inverted / unparseable rows. Long-book meta only.
/// - qty/entry/sl <= 0 or unparseable
/// - strategy_id outside 1..=5
/// - SL > entry by more than 5% (not a BE bump; e.g. entry=1000 sl=49000)
/// - initial stop distance > 50% of entry
pub fn meta_is_sane(m: &OpenTradeMeta) -> bool {
    if !(1..=5).contains(&m.strategy_id) {
        return false;
    }
    let (Some(entry), Some(qty)) = (m.entry_dec(), m.qty_dec()) else {
        return false;
    };
    let Some(sl) = dec(&m.initial_sl).ok() else {
        return false;
    };
    if entry <= Decimal::ZERO || qty <= Decimal::ZERO || sl <= Decimal::ZERO {
        return false;
    }
    if sl > entry {
        let prem = (sl - entry) / entry;
        if prem > Decimal::new(5, 2) {
            return false;
        }
    } else {
        let dist = (entry - sl) / entry;
        if dist > Decimal::new(5, 1) {
            return false;
        }
    }
    initial_risk_usdt(entry, sl, qty).is_some()
}

fn sanitize_map(map: &mut HashMap<String, OpenTradeMeta>) -> usize {
    let before = map.len();
    map.retain(|_, m| meta_is_sane(m));
    before.saturating_sub(map.len())
}

/// Sanitize in-memory store and persist if anything dropped. Returns drop count.
pub fn sanitize_store() -> usize {
    let mut store = lock_poison(&STORE);
    let dropped = sanitize_map(&mut store);
    if dropped > 0 {
        persist_locked(&store);
    }
    dropped
}

/// S4/S5 manage isolation: only touch positions tagged for this strategy
/// (S5 may also manage `s4_inherited` slots after an explicit 4→5 adopt).
pub fn continuation_owns(symbol: &str, strategy_id: i32, inherited_s4: &HashSet<String>) -> bool {
    let key = symbol.to_ascii_uppercase();
    match get(&key).map(|m| m.strategy_id) {
        Some(sid) if sid == strategy_id => true,
        Some(4) if strategy_id == 5 && inherited_s4.iter().any(|s| s == &key) => true,
        // Tagged for another strategy → do not manage (S4 must not trail S5, etc.).
        Some(_) => false,
        // Untagged (paper / first tick before on_open) → current lens may manage.
        None => true,
    }
}

pub fn on_open(
    strategy_id: i32,
    symbol: &str,
    entry: Decimal,
    stop_loss: Decimal,
    qty: Decimal,
    opened_ts: f64,
    btc_regime: Option<String>,
) {
    let Some(risk) = initial_risk_usdt(entry, stop_loss, qty) else {
        return;
    };
    let key = symbol.to_ascii_uppercase();
    let meta = OpenTradeMeta {
        symbol: key.clone(),
        strategy_id,
        entry: fmt_fixed(entry),
        initial_sl: fmt_fixed(stop_loss),
        initial_qty: fmt_fixed(qty),
        initial_risk_usdt: fmt_fixed(risk),
        opened_ts,
        mfe_usdt: "0".into(),
        mae_usdt: "0".into(),
        mfe_peak_ts: None,
        time_to_1r_ts: None,
        scaled_at_1r: false,
        btc_regime,
    };
    let mut store = lock_poison(&STORE);
    store.insert(key, meta);
    persist_locked(&store);
}

/// True when durable meta says this symbol already scaled at 1R for the *same* entry + strategy.
pub fn meta_scaled_for_entry(symbol: &str, entry: Decimal, strategy_id: i32) -> bool {
    let key = symbol.to_ascii_uppercase();
    let store = lock_poison(&STORE);
    let Some(m) = store.get(&key) else {
        return false;
    };
    if m.strategy_id != strategy_id {
        return false;
    }
    if !m.scaled_at_1r {
        return false;
    }
    match m.entry_dec() {
        Some(e) => (e - entry).abs() <= Decimal::new(1, 8),
        None => false,
    }
}

pub fn mark_scaled(symbol: &str) {
    let key = symbol.to_ascii_uppercase();
    let mut store = lock_poison(&STORE);
    if let Some(m) = store.get_mut(&key) {
        m.scaled_at_1r = true;
        persist_locked(&store);
    }
}

fn apply_mark_to_meta(m: &mut OpenTradeMeta, mark: Decimal, now: f64) -> bool {
    if mark <= Decimal::ZERO {
        return false;
    }
    let (Some(entry), Some(qty), Some(risk)) = (m.entry_dec(), m.qty_dec(), m.risk_dec()) else {
        return false;
    };
    let (mfe, mae, peak, t1r) = apply_mark_excursion(
        m.mfe_dec(),
        m.mae_dec(),
        m.mfe_peak_ts,
        m.time_to_1r_ts,
        entry,
        mark,
        qty,
        risk,
        now,
    );
    m.mfe_usdt = fmt_fixed(mfe);
    m.mae_usdt = fmt_fixed(mae);
    m.mfe_peak_ts = peak;
    m.time_to_1r_ts = t1r;
    true
}

pub fn update_mark(symbol: &str, mark: Decimal, now: f64) {
    let key = symbol.to_ascii_uppercase();
    let mut store = lock_poison(&STORE);
    let Some(m) = store.get_mut(&key) else {
        return;
    };
    if apply_mark_to_meta(m, mark, now) {
        persist_locked(&store);
    }
}

/// Fold S5 1h bar high/low into MFE/MAE. Last-price polls miss Hour1 wicks
/// (live S5 closes wrote mfe_r=0 after «откат с пика»). Journal only — no trade rule.
fn apply_s5_bars_to_meta(m: &mut OpenTradeMeta, bars: &[Bar], since_ms: i64) -> bool {
    if m.strategy_id != 5 {
        return false;
    }
    let before_mfe = m.mfe_usdt.clone();
    let before_mae = m.mae_usdt.clone();
    let before_peak = m.mfe_peak_ts;
    let before_t1r = m.time_to_1r_ts;
    for bar in bars {
        if bar.open_time < since_ms {
            continue;
        }
        let ts = (bar.open_time as f64) / 1000.0;
        if bar.high > Decimal::ZERO {
            apply_mark_to_meta(m, bar.high, ts);
        }
        if bar.low > Decimal::ZERO {
            apply_mark_to_meta(m, bar.low, ts);
        }
    }
    m.mfe_usdt != before_mfe
        || m.mae_usdt != before_mae
        || m.mfe_peak_ts != before_peak
        || m.time_to_1r_ts != before_t1r
}

/// Touch all open positions from a snapshot (manage tick). Single lock + one persist.
pub fn update_from_positions(positions: &[Position], snapshot: &MarketSnapshot, now: f64) {
    let mut store = lock_poison(&STORE);
    let mut dirty = false;
    for pos in positions {
        if pos.qty <= Decimal::ZERO {
            continue;
        }
        let mark = snapshot
            .tickers
            .iter()
            .find(|t| t.symbol.eq_ignore_ascii_case(&pos.symbol))
            .map(|t| t.last_price)
            .filter(|p| *p > Decimal::ZERO)
            .or_else(|| {
                snapshot
                    .bars_for(&pos.symbol)
                    .last()
                    .map(|b| b.close)
                    .filter(|c| *c > Decimal::ZERO)
            });
        let key = pos.symbol.to_ascii_uppercase();
        if let Some(m) = store.get_mut(&key) {
            if let Some(mark) = mark {
                if apply_mark_to_meta(m, mark, now) {
                    dirty = true;
                }
            }
            if m.strategy_id == 5 {
                let since = pos.opened_bar_time.unwrap_or((m.opened_ts * 1000.0) as i64);
                if apply_s5_bars_to_meta(m, snapshot.bars_for(&pos.symbol), since) {
                    dirty = true;
                }
            }
        }
    }
    if dirty {
        persist_locked(&store);
    }
}

/// Build close metrics; remove meta on full close. Partial keeps meta + scaled flag.
pub fn metrics_for_close(
    symbol: &str,
    pnl: Option<Decimal>,
    now: f64,
    partial: bool,
) -> CloseMetrics {
    let key = symbol.to_ascii_uppercase();
    let mut store = lock_poison(&STORE);
    let Some(m) = store.get(&key).cloned() else {
        return CloseMetrics::default();
    };
    let risk = m.risk_dec().unwrap_or(Decimal::ZERO);
    let mfe = m.mfe_dec();
    let mae = m.mae_dec();
    let hold = if m.opened_ts > 0.0 && now >= m.opened_ts {
        Some((now - m.opened_ts) as i64)
    } else {
        None
    };
    let time_to_mfe = m.mfe_peak_ts.map(|t| (t - m.opened_ts).max(0.0) as i64);
    let time_to_1r = m.time_to_1r_ts.map(|t| (t - m.opened_ts).max(0.0) as i64);
    let final_r = pnl.and_then(|p| r_multiple(p, risk)).map(fmt_fixed);
    let out = CloseMetrics {
        initial_risk_usdt: Some(m.initial_risk_usdt.clone()),
        initial_r: if risk > Decimal::ZERO {
            Some("1".into())
        } else {
            None
        },
        final_r,
        mfe_r: r_multiple(mfe, risk).map(fmt_fixed),
        mae_r: r_multiple(mae, risk).map(fmt_fixed),
        mfe_usdt: Some(fmt_fixed(mfe)),
        mae_usdt: Some(fmt_fixed(mae)),
        hold_sec: hold,
        time_to_mfe_sec: time_to_mfe,
        time_to_1r_sec: time_to_1r,
        scaled_at_1r: Some(m.scaled_at_1r || partial),
        btc_regime: m.btc_regime.clone(),
    };
    if partial {
        if let Some(row) = store.get_mut(&key) {
            row.scaled_at_1r = true;
        }
        persist_locked(&store);
    } else {
        store.remove(&key);
        persist_locked(&store);
    }
    out
}

pub fn remove(symbol: &str) {
    let key = symbol.to_ascii_uppercase();
    let mut store = lock_poison(&STORE);
    if store.remove(&key).is_some() {
        persist_locked(&store);
    }
}

/// Restore `scaled_one_r` and ensure meta exists for unmatched opens.
pub fn seed_from_positions(state: &mut EngineState, positions: &[Position], now: f64) {
    // Drop durable garbage before seeding latches / claiming opens.
    let _ = sanitize_store();
    let mut store = lock_poison(&STORE);
    for pos in positions {
        let key = pos.symbol.to_ascii_uppercase();
        if let Some(m) = store.get(&key) {
            // Foreign strategy meta must not latch scaled_one_r on this lens.
            if m.strategy_id != state.strategy_id {
                continue;
            }
            if m.scaled_at_1r {
                state.scaled_one_r.insert(key.clone());
            }
            continue;
        }
        let Some(sl) = pos.stop_loss else {
            continue;
        };
        let Some(risk) = initial_risk_usdt(pos.entry_price, sl, pos.qty) else {
            continue;
        };
        let opened = pos
            .opened_bar_time
            .map(|ms| ms as f64 / 1000.0)
            .unwrap_or(now);
        let meta = OpenTradeMeta {
            symbol: key.clone(),
            strategy_id: state.strategy_id,
            entry: fmt_fixed(pos.entry_price),
            initial_sl: fmt_fixed(sl),
            initial_qty: fmt_fixed(pos.qty),
            initial_risk_usdt: fmt_fixed(risk),
            opened_ts: opened,
            mfe_usdt: "0".into(),
            mae_usdt: "0".into(),
            mfe_peak_ts: None,
            time_to_1r_ts: None,
            scaled_at_1r: false,
            btc_regime: None,
        };
        if !meta_is_sane(&meta) {
            continue;
        }
        store.insert(key, meta);
    }
    persist_locked(&store);
}

fn ret_over_secs(bars: &[Bar], last_price: Decimal, secs: i64) -> Option<Decimal> {
    if bars.is_empty() || last_price <= Decimal::ZERO {
        return None;
    }
    let tip = bars.last()?;
    let target = tip.open_time.saturating_sub(secs.saturating_mul(1000));
    let past = bars.iter().rev().find(|b| b.open_time <= target)?;
    if past.close <= Decimal::ZERO {
        return None;
    }
    Some((last_price - past.close) / past.close * Decimal::from(100))
}

fn approx_pullback_pct(bars: &[Bar]) -> Option<Decimal> {
    if bars.len() < 3 {
        return None;
    }
    let n = bars.len().min(12);
    let recent = &bars[bars.len() - n..];
    let swing_high = recent.iter().map(|b| b.high).max()?;
    let pullback_low = recent.iter().map(|b| b.low).min()?;
    if swing_high <= Decimal::ZERO {
        return None;
    }
    Some((swing_high - pullback_low) / swing_high)
}

fn near_high_distance_frac(ticker: &Ticker) -> Option<Decimal> {
    if ticker.high_price <= Decimal::ZERO || ticker.last_price <= Decimal::ZERO {
        return None;
    }
    Some((ticker.high_price - ticker.last_price) / ticker.high_price)
}

/// Best-effort entry features from the live snapshot. Never errors.
pub fn build_entry_snapshot(
    snapshot: &MarketSnapshot,
    symbol: &str,
    entry: Decimal,
    stop_loss: Decimal,
    risk_pct: Option<Decimal>,
    equity: Option<Decimal>,
) -> EntrySnapshot {
    let mut out = EntrySnapshot::default();
    if let Some(t) = snapshot
        .tickers
        .iter()
        .find(|t| t.symbol.eq_ignore_ascii_case(symbol))
    {
        out.ret_24h = Some(fmt_fixed(t.price_change_percent));
        if t.quote_volume > Decimal::ZERO {
            out.quote_volume = Some(fmt_fixed(t.quote_volume));
        }
        out.near_high_frac = near_high_distance_frac(t).map(fmt_fixed);
    }
    let bars = snapshot.bars_for(symbol);
    out.ret_1h = ret_over_secs(bars, entry, 3600).map(fmt_fixed);
    out.ret_4h = ret_over_secs(bars, entry, 4 * 3600).map(fmt_fixed);
    out.pullback_pct = approx_pullback_pct(bars).map(fmt_fixed);
    if entry > Decimal::ZERO {
        let dist = (entry - stop_loss).abs() / entry;
        out.stop_distance = Some(fmt_fixed(dist));
    }
    if let (Some(rp), Some(eq)) = (risk_pct, equity) {
        if eq > Decimal::ZERO && rp > Decimal::ZERO {
            out.risk_pct = Some(fmt_fixed(rp));
            let _ = eq; // risk_pct itself is the configured fraction
        } else if rp > Decimal::ZERO {
            out.risk_pct = Some(fmt_fixed(rp));
        }
    } else if let Some(rp) = risk_pct {
        if rp > Decimal::ZERO {
            out.risk_pct = Some(fmt_fixed(rp));
        }
    }
    // Optional BTC 1h — skip if bars missing / slow; leave null.
    let btc_bars = snapshot.bars_for("BTCUSDT");
    if !btc_bars.is_empty() {
        let btc_px = snapshot
            .tickers
            .iter()
            .find(|t| t.symbol.eq_ignore_ascii_case("BTCUSDT"))
            .map(|t| t.last_price)
            .or_else(|| btc_bars.last().map(|b| b.close));
        if let Some(px) = btc_px {
            out.btc_ret_1h = ret_over_secs(btc_bars, px, 3600).map(fmt_fixed);
        }
    }
    out.btc_regime = Some(
        crate::regime::classify_snapshot(snapshot)
            .journal_tag()
            .into(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn risk_and_r_math() {
        let risk = initial_risk_usdt(d("100"), d("98"), d("2")).unwrap();
        assert_eq!(risk, d("4")); // 2 * 2
        assert_eq!(r_multiple(d("6"), risk).unwrap(), d("1.5"));
        assert!(initial_risk_usdt(d("100"), d("100"), d("1")).is_none());
    }

    #[test]
    fn mfe_mae_update() {
        let (mfe, mae, peak, t1r) = apply_mark_excursion(
            d("0"),
            d("0"),
            None,
            None,
            d("100"),
            d("103"),
            d("1"),
            d("2"),
            10.0,
        );
        assert_eq!(mfe, d("3"));
        assert_eq!(mae, d("0"));
        assert_eq!(peak, Some(10.0));
        assert_eq!(t1r, Some(10.0)); // 3 >= 2 risk

        let (mfe2, mae2, _, _) =
            apply_mark_excursion(mfe, mae, peak, t1r, d("100"), d("97"), d("1"), d("2"), 20.0);
        assert_eq!(mfe2, d("3"));
        assert_eq!(mae2, d("3"));
    }

    #[test]
    fn s5_folds_1h_bar_high_low_into_close_mfe_mae_r() {
        let dir = tempfile::tempdir().unwrap();
        set_active_path(Some(dir.path().join("open_meta.json")));
        on_open(
            5,
            "AVAXUSDT",
            d("100"),
            d("97"),
            d("1"),
            1_700_000_000.0,
            None,
        );
        let mut pos = Position::long("AVAXUSDT", d("1"), d("100"), Some(d("97")), Some(d("106")));
        pos.opened_bar_time = Some(1_700_000_000_000);
        let bar = Bar {
            open_time: 1_700_000_000_000,
            open: d("100"),
            high: d("106"),
            low: d("98"),
            close: d("100"),
            volume: d("20"),
        };
        let mut snap = MarketSnapshot::empty(d("10000"));
        snap.tickers = vec![Ticker::new("AVAXUSDT", d("100"), d("0"), d("50000000"))];
        snap.universe_bars.insert("AVAXUSDT".into(), vec![bar]);
        update_from_positions(&[pos], &snap, 1_700_003_600.0);
        let m = metrics_for_close("AVAXUSDT", Some(d("-0.1")), 1_700_003_600.0, false);
        assert_eq!(m.mfe_r.as_deref(), Some("2"), "1h high 106 vs 3 USDT risk");
        let mae: Decimal = m.mae_r.as_ref().unwrap().parse().unwrap();
        assert!(
            mae > d("0.6") && mae < d("0.7"),
            "1h low 98 is ~0.67R: {mae}"
        );
        assert_eq!(m.mfe_usdt.as_deref(), Some("6"));
        assert_eq!(m.mae_usdt.as_deref(), Some("2"));
        set_active_path(None);
    }

    #[test]
    fn garbage_meta_is_rejected() {
        let bad_btc = OpenTradeMeta {
            symbol: "BTCUSDT".into(),
            strategy_id: 1,
            entry: "1000".into(),
            initial_sl: "49000".into(),
            initial_qty: "0.02".into(),
            initial_risk_usdt: "960".into(),
            opened_ts: 1.0,
            mfe_usdt: "0".into(),
            mae_usdt: "0".into(),
            mfe_peak_ts: None,
            time_to_1r_ts: None,
            scaled_at_1r: false,
            btc_regime: None,
        };
        assert!(!meta_is_sane(&bad_btc));
        let ok = OpenTradeMeta {
            symbol: "ORCAUSDT".into(),
            strategy_id: 4,
            entry: "1.542".into(),
            initial_sl: "1.48".into(),
            initial_qty: "49.4".into(),
            initial_risk_usdt: "3.0628".into(),
            opened_ts: 1.0,
            mfe_usdt: "0".into(),
            mae_usdt: "0".into(),
            mfe_peak_ts: None,
            time_to_1r_ts: None,
            scaled_at_1r: false,
            btc_regime: None,
        };
        assert!(meta_is_sane(&ok));
        let be = OpenTradeMeta {
            symbol: "RAYSOLUSDT".into(),
            strategy_id: 4,
            entry: "1.0943".into(),
            initial_sl: "1.1411968".into(), // ~4.3% above entry — still within 5% BE band
            initial_qty: "39.6".into(),
            initial_risk_usdt: "1.857".into(),
            opened_ts: 1.0,
            mfe_usdt: "0".into(),
            mae_usdt: "0".into(),
            mfe_peak_ts: None,
            time_to_1r_ts: None,
            scaled_at_1r: false,
            btc_regime: None,
        };
        assert!(meta_is_sane(&be));
    }

    #[test]
    fn continuation_owns_isolates_s4_s5() {
        let mut map = HashMap::new();
        map.insert(
            "AAAUSDT".into(),
            OpenTradeMeta {
                symbol: "AAAUSDT".into(),
                strategy_id: 4,
                entry: "10".into(),
                initial_sl: "9.5".into(),
                initial_qty: "1".into(),
                initial_risk_usdt: "0.5".into(),
                opened_ts: 1.0,
                mfe_usdt: "0".into(),
                mae_usdt: "0".into(),
                mfe_peak_ts: None,
                time_to_1r_ts: None,
                scaled_at_1r: false,
                btc_regime: None,
            },
        );
        *lock_poison(&STORE) = map;
        let empty = HashSet::new();
        assert!(continuation_owns("AAAUSDT", 4, &empty));
        assert!(!continuation_owns("AAAUSDT", 5, &empty));
        let mut inh = HashSet::new();
        inh.insert("AAAUSDT".into());
        assert!(continuation_owns("AAAUSDT", 5, &inh));
        assert!(continuation_owns("MISSING", 4, &empty)); // untagged → allow
    }
}

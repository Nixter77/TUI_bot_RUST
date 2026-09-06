# План: стратегия 2 (scalp)

## Исследование журнала
- `.state/trades.jsonl`: **n=0** закрытий с `strategy_id==2` (и без scalp-меток в reason).
- В журнале есть только S4 и S1. Живой PnL/WR по скальпу пока нет — опираемся на код + офлайн-бэктест.

## Офлайн-бэктест
- **Offline −EV** на публичных klines (ожидание отрицательное; не edge до live closes).
- `src/backtest.rs`: scalp на 5m-классе, `ScalpParams::default()` (окна пустые = круглосуточно в CLI).
- После смены `max_hold_bars` default **8** мёртвые холды режутся раньше, чем legacy 24.
- Live/TUI: `ScalpParams::from_config` подхватывает `STRATEGY2_ENTRY_HOURS` / `STRATEGY2_MAX_HOLD_BARS`.

## Код до правок
- Manage: TP/SL/time-stop/reversal + trail/BE после 1R; **не было** end-of-session flatten и peak-giveback.
- TUI звал `tick_decisions(..., scalp=None)` → окна из `.env` не доходили.
- `max_hold_bars` default был **24**.

## Отгружено (ship-now)
1. **Сессия:** `STRATEGY2_ENTRY_HOURS` / `STRATEGY2_ALWAYS_ENTER` → `Config.s2_*` → `ScalpParams::from_config`. Вне окна открытый лонг → Exit «конец сессии». Monitor session knobs для sid=2.
2. **Тайм-стоп:** default `max_hold_bars=8`; env `STRATEGY2_MAX_HOLD_BARS` (1–240).
3. **Откат с пика (pre-BE):** peak ≥ 0.8R и mark < entry+0.25R → AmendStop «откат с пика — замок 0.25R» или Exit «откат с пика».
4. **1.5R bank:** mark (или peak≥1.5R при mark≥1R) → Exit «scalp 1.5R — фиксирую».
5. **Fee-aware BE** + **fee-padded entry TP** (S4 money helpers).

## НЕ делали (бриф)
RISK_PCT для S2 · dump filter · exclude majors · ReduceLong scale-out · новые индикаторы · шорты · websocket-first · ratatui · telegram · sqlite-as-alpha · grid search.

## Риски остатка
- Нет live closes → EV гипотетический; после сессии сверить close reasons («конец сессии» / «откат с пика» / time stop).
- CLI-бэктест без env-окон.
- Daily halt уже глобальный (dayrisk) — без изменений.

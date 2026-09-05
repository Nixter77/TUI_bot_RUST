# План: стратегия 4 (continuation)

## Сейчас
**Сначала живой стол, воронка ослаблена** (UI hang fix + funnel soften).

Воронка расширена чтобы снова были входы (max 24h **35%**):
- `max_change_percent` default **35** (было 20) — мега-пампы всё ещё out.
- `liquid_frac` default **0.5%** (было 2%) — outlier vol больше не обнуляет книгу.
- `near_high_frac` default **0.05** (было 0.02).
- 4h entry: только close > EMA20 — **без** 4h higher-low (был choke).
- Новые слоты до `max_positions` **без** требования «все открытые в плюсе».
- `volume_confirm_frac` default **0.3** (было 0.5 / ранее 0.8).
- `min_pullback_pct` на **15m** **1.0%** (было 1.2%) — меньше пустых книг на 15m.
- Loss cooldown 12h — без изменений (не env).

## Уже сделано / KEEP
- Majors exclude (BTC/ETH/BNB/XRP/SOL/BCH); фильтр дампа 24h; книга по `liquid_n`; `STRATEGY4_MAX_POSITIONS=5`.
- 4h exit: last close ≤ EMA20 → «4ч сломал тренд».
- 1R → fee-aware BE + scale-out ~50%; fail-closed protectives; daily halt (USDT / R).
- 1.5R bank; mark trail runner после BE; замок 0.5R; откат с пика; тайм-стоп.
- UI: orphan probe без REST-шторма; paint-then-live; try_lock (hang fix).

## НЕ делать
шорты · websocket-first · ratatui · telegram · sqlite-as-alpha · куча индикаторов · grid search

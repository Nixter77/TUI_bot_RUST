# План: стратегия 4 (continuation)

## Сейчас
**Сначала живой стол, воронка ослаблена** (UI hang fix + funnel soften).

Воронка (чтобы бот чаще входил):
- `near_high_frac` default **0.05** (было 0.02).
- 4h entry: только close > EMA20 — **без** 4h higher-low (был choke).
- Новые слоты до `max_positions` **без** требования «все открытые в плюсе».
- `volume_confirm_frac` default **0.5** (было 0.8).
- Loss cooldown 12h — без изменений (не env).

## Уже сделано / KEEP
- Majors exclude (BTC/ETH/BNB/XRP/SOL/BCH); фильтр дампа 24h; книга по `liquid_n`; `STRATEGY4_MAX_POSITIONS=5`.
- 4h exit: last close ≤ EMA20 → «4ч сломал тренд».
- 1R → fee-aware BE + scale-out ~50%; fail-closed protectives; daily halt (USDT / R).
- 1.5R bank; mark trail runner после BE; замок 0.5R; откат с пика; тайм-стоп.
- UI: orphan probe без REST-шторма; paint-then-live; try_lock (hang fix).

## НЕ делать
шорты · websocket-first · ratatui · telegram · sqlite-as-alpha · куча индикаторов · grid search

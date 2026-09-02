# План: стратегия 4 (continuation)

## Уже сделано
- 4h entry: close выше EMA20; при двух swing low нужен higher low («4ч нет higher low»).
- Фильтр дампа 24h; majors исключены из S4 book (`is_major_symbol`); книга по `liquid_n`; `STRATEGY4_MAX_POSITIONS=5`.
- КД 12h после стопа; звуки; снимок рынка вне UI (`5188510`).
- 4h exit: last close ≤ EMA20 → «4ч сломал тренд».
- 1R → fee-aware BE; иначе «1R был — фиксирую».
- 1.5R bank: mark≥1.5R (pre-BE или post-BE) → Exit «1.5R — фиксирую» (не ждём 2R TP; не ставим замок 0.5R пока mark на 1.5R).
- Mark trail runner после BE: `max(bar low, mark×(1−trail_pct))`.
- Замок 0.5R: peak≥1.5R и mark отдал назад но ещё >0.5R → AmendStop «замок 0.5R».
- Откат с пика: peak≥0.8R от entry и mark < entry+0.25R → «откат с пика» (до 1R BE).
- Тайм-стоп 4ч / конец окна входа (`!always_enter`).
- Exclude majors from S4: BTC/ETH/BNB/XRP/SOL/BCH.
- Scale-out ~50% на +1R (`Decision::ReduceLong` + latch `scaled_one_r`); BE на остатке; runner — post-BE 0.5R / 1.5R / trail.
- Fail-closed: если protectives не встают после 3 failed rearm **или** 90s (`REARM_FAIL_MAX` / `REARM_FAIL_BUDGET_SEC`) → market flatten «нет protectives — flatten» (не flatten на -4130).
- R-aware daily loss halt: `DAILY_LOSS_USDT` **or** `DAILY_LOSS_R×(day_start_equity×RISK_PCT)` (defaults 20 / 3) — either layer trips `daily_halt` until next UTC day. `RISK_PCT=0` or `DAILY_LOSS_R=0` → R off (USDT-only).

## Сейчас
1. Paper/TUI `ReduceLong` apply landed (qty half + BE + `scaled_one_r` latch; sim latches too).
2. Skip-rate: see `SKIP_RATE.md` — post-Aug25 S4 closes **14**, WR **4/14 (28.6%)**, net **-9.9192**; top close reasons: биржа закрыла лонг (6), TP (4), continuation stop from entry (2). Runtime skip tallies → `.state/s4_skip_stats.json` (empty until next S4 TUI session). After skip soak, R-halt shipped.
3. `near_high_frac` / окна входа — still data-gated; retune **only after** session skip counts show a dominant false skip.


## НЕ делать
шорты · websocket-first · ratatui · telegram · sqlite-as-alpha · куча индикаторов · grid search

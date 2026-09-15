# План: стратегия 2 (scalp)

## Исследование журнала
- `.state/trades.jsonl` sid=2: **N=2** closes (оба убыток, WR n/a), open=2; R/MFE на старых строках пустые.
- **Нельзя** писать «fixed» / edge: live N≪30, offline PF≪1.

## Офлайн-бэктест (baseline, public 5m cache)
- `DUMP_S2=1 KEEP_KLINES=1 cargo run --release -- --backtest` → `.state/s2-research.txt`
- Fee: **0.04% taker/side** → RT ≈ **0.08%** notional. **Funding не моделируется**.
- Не трогать `RISK_PCT` при PF_net≤1. **edge? no** (offline −EV).

| arm | n | WR | PF_net | sum PnL | avg_win / 2×RT fee | note |
|-----|---|----|--------|---------|--------------------|------|
| default 24/7 max_hold=8 | 25 | 24% | **0.31** | −0.61 | 0.045 / 0.032 fee-floor OK | time-stop 11, wick SL 6, peak 3, TP 2 |
| session DEFAULT hours | 16 | 25% | **0.26** | −0.38 | 0.034 / 0.032 fee-floor OK | **конец сессии 6**, wick SL 4, TP 1 |

- Exit-mix confirms session flatten + peak giveback fire; avg hold ~4–5 bars (<8).
- Peak **MFE not on ClosedTrade** — exit-mix only; do not claim «fixed» from BT alone.
- Live journal: **N=2** closes sid=2 (оба −), fee≈0.08, R snapshot missing on old rows — still no edge.

## Locked params (audit)
| Knob | Value | Notes |
|------|-------|-------|
| Universe | BTC/ETH/SOL majors book | как S1; **не** топ 24h tape |
| Signal | EMA9/21 + VWAP + RSI band, 5m bounce | без новых индикаторов |
| SL | ATR×1.2 (floor min_stop) | TP entry **2R** fee-padded |
| Session | `STRATEGY2_ENTRY_HOURS` + end-of-session flatten | TUI: `ScalpParams::from_config` |
| ALWAYS_ENTER | `STRATEGY2_ALWAYS_ENTER` | |
| max_hold | default **8** bars (`STRATEGY2_MAX_HOLD_BARS` 1–240) | не 24 |
| Peak giveback | peak≥0.8R & mark<entry+0.25R → lock 0.25R / Exit | |
| 1.5R bank | mark≥1.5R (или peak≥1.5R & mark≥1R) | |
| Slot | **1** open | live cap + monitor gate |

## Код до правок (история)
- Manage: TP/SL/time-stop/reversal + trail/BE после 1R; **не было** end-of-session flatten и peak-giveback.
- TUI звал `tick_decisions(..., scalp=None)` → окна из `.env` не доходили.
- `max_hold_bars` default был **24**.

## Отгружено (ship-now + solidify)
1. **Сессия:** `STRATEGY2_ENTRY_HOURS` / `STRATEGY2_ALWAYS_ENTER` → `Config.s2_*` → `ScalpParams::from_config`. Вне окна открытый лонг → Exit «конец сессии». Monitor session knobs для sid=2.
2. **Тайм-стоп:** default `max_hold_bars=8`; env `STRATEGY2_MAX_HOLD_BARS` (1–240).
3. **Откат с пика (pre-BE):** peak ≥ 0.8R и mark < entry+0.25R → AmendStop «откат с пика — замок 0.25R» или Exit «откат с пика».
4. **1.5R bank:** mark (или peak≥1.5R при mark≥1R) → Exit «scalp 1.5R — фиксирую».
5. **Fee-aware BE** + **fee-padded entry TP** (S4 money helpers).
6. **Solidify (feat/s2-live-solid):** majors isolation S2/S3; live slot cap=1; S2 loss-pause uses `s2` windows (not S1); monitor «ожидание» = majors book ≠ 24h tape; notional/fill-confirm tests aligned with fail-closed live path.

## НЕ делали (бриф)
RISK_PCT для S2 · dump filter · exclude majors · ReduceLong scale-out · новые индикаторы · шорты · websocket-first · ratatui · telegram · sqlite-as-alpha · grid search · live soak / merge без Chief GO.

## Риски остатка
- Нет live closes → EV гипотетический; после сессии сверить close reasons («конец сессии» / «откат с пика» / time stop) + MFE/MAE из journal.
- CLI-бэктест без env-окон; funding omitted → optimistic vs live.
- Daily halt уже глобальный (dayrisk) — без изменений.
- costR: если avg win ≤ ~2× RT fee — **не edge**, даже при красивом WR.

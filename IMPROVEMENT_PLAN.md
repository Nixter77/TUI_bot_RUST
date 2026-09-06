# План улучшений TUI_bot_RUST

Опирается на: разбор how-it-works (внешний агент), реальный код/журнал, фиксы Sep 2025–2026 (UI hang, воронка S4, scan-steal, Ready⟺enter).

**Тезис:** execution/reliability уже сильнее, чем доказанный edge. Дальше не «ещё индикатор», а измерение → regime → score → risk → точечные правки exit/entry.

**НЕ делать:** шорты · websocket-first · ratatui · telegram · sqlite-as-alpha · indicator soup · grid search «на глаз» · плодить S5/S6 до аналитики.

---

## Уже сделано (не повторять)

| Тема | Статус |
|------|--------|
| UI hang (orphan REST storm) | `7db5c96` |
| Воронка S4 (near_high, max 24h, liquid_frac, volume) | `4bd7409` + follow-ups |
| 15m история на весь liquid desk | `a0d12ee` |
| Scan-steal (`last_scan_ts` до tick) | `fa981d9` |
| Monitor `[готов]` = live enter path | `4058a22` |
| 1R BE / scale-out / fail-closed / daily halt USDT\|R | в main |
| S2 session / max_hold / peak giveback | `d49d30b`… |

---

## Фаза 0 — стабильный стол (сейчас)

Цель: бот **реально входит** на TestNet и пишет журнал без зависаний.

1. Всегда гонять **пересобранный** binary после фиксов; `--live` и `--monitor` перезапускать вместе.
2. Одна стратегия soak: **только S4** 3–7 дней; S1/S2/S3 не крутить параллельно «для разнообразия».
3. В TUI смотреть: `Последнее решение` ≠ вечный `waiting for next scan`; при `[готов]` в monitor — Enter на скане (~60с).
4. Критерий готовности фазы: ≥20–30 **новых** live closes S4 после `4058a22`, без UI freeze.

---

## Фаза 1 — аналитика (P0, первый большой шаг) ✅ done

Без этого любая оптимизация — вслепую. Журнал сейчас: open/close/amend, PnL USDT, reason — **мало**.

### 1.1 Trade record в R + MFE/MAE

На каждую закрытую сделку дописать (journal или рядом `.state/trades_ext.jsonl`):

- `initial_risk_usdt`, `initial_r` (1R = |entry−sl|×qty)
- `final_r`, `mfe_r`, `mae_r`, `mfe_usdt`, `mae_usdt`
- `time_to_1r_sec`, `time_to_mfe_sec`, `hold_sec`
- `fees`, `exit_reason`, `strategy_id`, `symbol`
- `scaled_at_1r` (bool), durable — не только RAM

Обновлять MFE/MAE на manage-tick по mark (или bar high/low).

### 1.2 Entry snapshot (immutable)

В момент `EnterLong` сохранить фичи:

`price, ret_1h/4h/24h, volume_ratio, atr, ema20_tf, ema20_4h, vwap_dist, pullback_pct, near_high_pct, btc_ret_1h/4h, entry_score (позже), stop_pct, risk_pct`

### 1.3 Отчёт

`cargo run -- --report` (или новый `--research`):

| Metric | S1 | S2 | S3 | S4 |
|--------|----|----|----|----|
| Trades / WR / Exp R / PF / Max DD R / Avg MFE / MAE / Hold | | | | |

Плюс разбивка по `exit_reason` и часу UTC.

**Критерий:** один markdown/CSV отчёт из живого журнала без ручных скриптов.

---

## Фаза 2 — regime layer (P1) ✅ done

Один слой на все альт-лонги (особенно S4):

`BTC_REGIME ∈ {STRONG_BULL, BULL, NEUTRAL, BEAR, PANIC}`

из простых правил (4h EMA slope + цена vs EMA20 + ATR percentile) — без индикаторного супа.

| Regime | Поведение |
|--------|-----------|
| STRONG_BULL / BULL | полный `RISK_PCT` |
| NEUTRAL | 0.5× risk или только top score |
| BEAR | нет новых alt longs |
| PANIC | flatten / halt entries |

Писать `btc_regime` в entry snapshot и в report.

---

## Фаза 3 — S4: hard gates + score (P1)

Согласны с разбором: boolean AND-стек → мало сделок и overfitting.

**Hard (оставить):** liquidity/junk/majors, dump (neg 24h), daily halt, desk cooldown, stop too wide, no bar/data, position limit, fail-closed protectives.

**Soft → score 0–100:** trend (TF+4h EMA), momentum 24h quality, pullback depth, volume confirm, VWAP reclaim, structure quality.

| Score | Действие |
|-------|----------|
| ≥75 | Enter |
| 65–74 | Watch (monitor ACTIVE) |
| <65 | Skip |

Убрать дублирующие hard-фильтры, которые уже в score. Не добавлять новые индикаторы — только переразложить существующие признаки.

---

## Фаза 4 — risk portfolio (P2)

Сейчас: per-trade `RISK_PCT` + daily halt — ок, но альты коррелированы.

1. `max_open_risk_r` (например 4R суммарно ≈ 1% equity при 0.25%/R).
2. `max_correlated_risk` — грубо: если BTC 1h сильно вниз, не наращивать корзину; или лимит одновременных alt longs.
3. Durable `PositionState` (scaled qty, initial R, MFE/MAE) — убрать «scaled_one_r только в RAM».

---

## Фаза 5 — S4 exits A/B/C (P2, только после фазы 1)

**Exit A LOCKED (2026-09-06, user choice):** keep current — ~50% scale at 1R + fee-aware BE, then bank ~1.5R on remainder. **B/C deferred** until research report has enough closes; do not switch exits «по вкусу».

| | 1R | Дальше | Статус |
|---|----|--------|--------|
| **A (locked)** | ~50% + BE | bank ~1.5R | **live / soak** |
| **B** | 25% + BE | trail / 2R | deferred |
| **C** | только BE | trail, без forced 1.5R | deferred |

**Long-only desk:** Binance SELL only to close longs (`reduceOnly` / sized protectives — never naked `closePosition` SELL that can open a leftover short). Rogue shorts are swept closed (BUY reduce-only); no intentional short entries.

Сравнивать Expectancy R, PF, Max DD, MFE capture — выбрать plateau, не max PnL точку — **только когда B/C разблокируют после метрик**.

Стопы: исследовать ATR×k + reject по percentile вместо жёстких % caps как главной геометрии (после метрик).

---

## Фаза 6 — S1 / S2 / S3 (P3)

| Strat | Вердикт | Действие |
|-------|---------|----------|
| **S4** | Главный фокус | score + regime + exits |
| **S2** | Ок база | event-driven RSI reclaim / VWAP slope — **после** метрик; soak отдельно |
| **S3** | Редкий breakout | не оптимизировать под частоту; свой report horizon |
| **S1** | Самый слабый edge | multi-horizon momentum score **или** заморозить до research; не крутить в live soak |

Discovery 60s + confirm кандидатов чаще (5–15s) — опционально после стабильных входов.

---

## Фаза 7 — архитектура кода (P3, параллельно осторожно)

1. `Strategy` trait: `candidates` / `decide` / `manage` — убрать S4 knobs через `MomentumParams`.
2. Monitor: ленты TAPE / WAIT / **ACTIVE** (score 65–74).
3. Не раздувать UI; docs в `docs/how-it-works.html` обновлять после фаз 1–3.

---

## Порядок работ (чеклист)

- [ ] **0.1** Soak S4 live ≥ N сделок после Ready-fix  
- [x] **1.1** MFE/MAE + final_r в journal  
- [x] **1.2** Entry snapshot  
- [x] **1.3** Research report (таблица по стратегиям)  
- [x] **2.1** BTC regime + запись в snapshot  
- [ ] **3.1** S4 SetupScore + hard/soft split  
- [ ] **4.1** Portfolio heat + durable position state  
- [x] **5.0** Exit A locked (50% @ 1R + BE → ~1.5R bank); B/C deferred; long-only sell-to-close
- [ ] **5.1** Exit A/B/C сравнение на данных (после unlock B/C)  
- [ ] **6.x** S1/S2/S3 точечно  
- [ ] **7.x** Strategy trait / ACTIVE tape  

---

## Критерии успеха (через 2–4 недели)

1. Отчёт: expectancy **в R**, не только USDT.  
2. S4: понятно, какой exit и какой score-порог на plateau.  
3. Нет регресса: hang / scan-steal / Ready≠enter.  
4. Одна стратегия в live soak; остальные research-only.


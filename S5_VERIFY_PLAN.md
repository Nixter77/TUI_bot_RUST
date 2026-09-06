# S5 — проверочная стратегия (кандидат)

**Дата:** 2026-09-06  
**Статус:** отчёт / design freeze — **код S5 не писать**, пока нет замеров ATR% + cost-in-R и A/B 15m vs 1h.  
**Цель:** зафиксировать research по 15m continuation как «проверочный» эталон рядом с live S4 (не плодить индикаторный суп; см. `IMPROVEMENT_PLAN.md`).

Охват: desk research (Resercher + Researchy), сверка кода Chief, notes CodeReviewer/Backender.

---

## 1. Зачем S5

S4 уже live soak на 15m. S5 — не «ещё одна альфа», а **verification track**:

1. Те же правила ядра, другой TF (1h) или те же правила с жёстким cost-gate — сравнить **net expectancy / SQN**, не winrate.
2. Чеклист: fees+noise не убивают edge при стопе 2–5%.
3. Документ для регрессии: если правим S4, сверяем, не сломали ли лок ядра.

**НЕ делать в S5:** shorts · RSI/MACD/ADX soup · grid search · websocket-first · telegram · sqlite-as-alpha · возврат HL-серий «на глаз».

---

## 2. Лок ядра (факт из кода, 2026-09-06)

Файл: `src/continuation.rs` (Strategy 4).

| Компонент | Статус |
|-----------|--------|
| 4h close > EMA20 | **да** (filter; нет 4h → skip) |
| 4h higher-low | **снят** (был choke) |
| Signal-TF EMA20 (close > EMA) | **да** |
| Signal-TF higher-low series | **снят** |
| Pullback resume (red history → green / min pullback) | **да** |
| near_24h_high skip | **да** |
| Long-only; sell = reduce-only close | **да** |
| Stop band | **2–5%** (min/max) |
| Reward target | **~2R** (+ Exit A: ~50%@+1R → BE → ~1.5R bank) |

**Формула ядра одной строкой:**  
`4h EMA20 + signal-TF EMA20 + pullback` — **не** EMA-only, **не** «4h+HL».

Interval: `STRATEGY4_INTERVAL` (default / soak: **15m**). История 15m тянется на весь liquid desk.

---

## 3. 15m vs 5m vs 1h (research)

| TF | Плюсы | Минусы | Для S4/S5 |
|----|-------|--------|-----------|
| **5m** | больше сэмплов, быстрее pullback | noise, churn → fee/slip, ложные EMA | только если **net** после fees доказан |
| **15m** | баланс структуры vs число сделок; стык с 4h filter | thin hours; SL 2–5% + ретесты едят cost | **default continuation** |
| **1h** | чище trend, fee доля от move ниже | меньше сделок, позже вход, больше funding-окон | **A/B** на том же ядре |

**Вывод:** 15m — разумный default; 1h — кандидат verification (S5 TF-arm); 5m не трогать без net proof.

---

## 4. Fees в R (наш futures desk)

- Bot / policy: taker **~0.04%/side** → round-trip **~0.08%** notional (до slip/funding).
- При стопе **2%**: costR ≈ **0.04R**; при **5%**: ≈ **0.016R**.
- Spot VIP0 ~0.10%/side (Researchy secondary) даёт ~0.10R / ~0.04R — **хуже** нашего futures; для sizing использовать **наши** 0.04%/side.

**Следствие:** при RR≈2 fees сами по себе **не** убивают expectancy на 2–5% стопе. Узкое горло — **noise/ATR vs stop** и качество сигнала, не комиссия.

Breakeven WR (грубо, с costR): \((1+\mathrm{costR})/(1+\mathrm{RR})\). При RR=2 и costR=0.04 → ~34.7% (против ~33.3% без fees).

---

## 5. Noise / ATR (гипотезы + что мерить)

**Гипотезы (не факты):**
- На liquid majors 15m «шум свечи» обычно << 2% → фиксированный min 2% чаще *вне* candle noise, ближе к structure/ATR multiple.
- На тихом BTC 2% может быть много ATR → редкие хиты, жирные losers / медленный 2R.
- На alts 2% может быть тесно относительно ATR.

**Замер (Backender, на текущем ядре):**
1. **14-ATR% / close** по liquid book: min / median / p90 по символам (15m и для A/B — 1h).
2. **Cost в R** на закрытую сделку: fee + slip (+ funding, если доступно) / initial risk USDT.
3. Сравнить stop% vs ATR%: доля сделок где stop < ~1.5–2× ATR (подозрительно тесно).

Порог гейта символов/сессий: если median cost ≥ X% от 1R — ужесточить (X калибровать на TestNet).

---

## 6. Entry/exit — практика vs миф

| Элемент | Статус |
|---------|--------|
| HTF EMA bias + LTF pullback | практика trend systems, не доказанная альфа |
| EMA20 pullback | практика; нет peer-reviewed «EMA20 magic» |
| SL structure / % band / ATR | практика + методология (Tharp / Wilder ATR) |
| R-multiples, risk%, ~2R | методология Tharp; 2R = design target |
| Scale-out 50%@1R→BE (Exit A) | практика спорная; не раздувать без runner expectancy |
| Session windows | ToD объём — факт; окна = edge — гипотеза |
| Time-stop / max-hold | risk control, не альфа |
| «Больше индикаторов = лучше» | **миф / overfit** |
| «Crypto 24/7 → сессии не важны» | **опровергнуто** ToD-паттернами |

---

## 7. Риски 15m alts USDT-M

1. Noise / stop-hunts / dump → fat-tail >1R slippage.  
2. Thin book вне пика → execution cost ↑.  
3. Funding ~8h (часто 00/08/16 UTC) — drag на long при перегретом funding; max-hold режет пересечения.  
4. Top liquid ≠ независимые ставки (корреляция).  
5. Метрика врага: **turnover × (fee+slip+funding) / средний R**, не «неправильная EMA».

---

## 8. Что усиливать / не трогать

**Усиливать (измерение → потом точечный gate):**
- Лог fee (+slip/funding) в R на сделку; session/hold калибровка по ликвидности.
- Dump filter + symbol cooldown + fail-closed protectives (anti-ruin) — держать.
- A/B **только** TF/hold: 15m vs 1h, **то же ядро**, метрика = net expectancy / SQN.

**Не трогать:**
- Ядро `4h EMA20 + TF EMA20 + pullback`.
- Не возвращать HL-серии без отдельного A/B proof.
- Не лить RSI/MACD/ADX; не открывать shorts; не менять Exit A без данных R-labeled closes.

---

## 9. Спека кандидата S5 (verification arm)

Пока **только дизайн**. Рабочее имя: `S5 Verify` / strategy_id **5** — *когда* решим кодить.

| Параметр | Предложение |
|----------|-------------|
| Ядро сигнала | Идентично S4 (см. §2) |
| Signal interval | **1h** (A/B arm) *или* 15m с жёстким cost-gate — выбрать после замеров |
| HTF filter | тот же 4h EMA20 |
| Risk / stop / TP | те же 2–5% / ~2R / Exit A |
| Long-only | да |
| Отличие от S4 | только TF (и/или cost-gate), плюс отдельный journal `strategy_id=5` для сравнения |
| Критерий победы | net Exp R / SQN / PF на ≥N полных closes; не winrate |

**Критерий «не запускать S5 в live»:** нет min/median/p90 ATR% и нет cost-in-R baseline по S4 15m.

---

## 10. Открытые вопросы

- [ ] Фактический fee tier TestNet/аккаунта (сверить Binance FAQ live).  
- [ ] Измеренный 14-ATR% на liquid book (15m и 1h).  
- [ ] Cost-in-R распределение на закрытых S4.  
- [ ] Достаточно ли R-labeled closes после phase-1 journal для Exp R.  
- [ ] S5 = отдельный `strategy_id` в том же бинаре или offline-only backtest arm.

---

## 11. Источники

**Academic / primary-ish**
- Brauneis / Mestel / Theissen — crypto time-of-day: https://doi.org/10.1007/s11156-024-01304-1  
- Van Tharp — position sizing / R: https://vantharpinstitute.com/why-size-really-does-matter-by-van-k-tharp-ph-d/  
- Van Tharp — sizing overview: https://vantharpinstitute.com/van-tharp-teaches-position-sizing-strategies-and-risk-management/  
- Binance fees/funding FAQ (сверить live): https://www.binance.com/en/support/faq/detail/98488a516eb84e3eb34605683dffd554  

**Secondary (практика / TF / fee-in-R — не academic)**
- fomoed.com/en/blog/best-timeframe-for-trading-bots/  
- futurespulse.io/en/futures-trading-fees/  
- tradernest.ai/blog/best-timeframe-for-day-trading  
- coinxsight.com (TF noise table)  
- coinquant.ai (15m fee drag notes)  
- dev.to/ndidichenko (fees in R)  
- fxnx.com (costR / stop vs hit rate)

Official fee page fetch у Researchy timed out — перед sizing сверить schedule на бирже.

---

## 12. Факты vs гипотезы

**Факты**
- ToD пик volume/vol/illiquidity ~16:00–17:00 UTC (Brauneis et al.).  
- Tharp: size от % equity и unit risk; система в R/expectancy.  
- Perp: fee на fill + funding на settlement; часто 8h.  
- В коде S4 ядро = 4h EMA20 + TF EMA20 + pullback; HL series removed.  
- RT fees desk ≈0.08% notional → costR ≈0.04R @2% stop / ≈0.016R @5%.

**Гипотезы**
- 15m оптимален именно для нашего edge.  
- Session windows = net edge.  
- Exit A scale-out повышает expectancy.  
- EMA20+pullback = устойчивый edge.  
- ATR-based floor улучшит alts без потери majors.

---

## 13. Одной строкой

Держим S4 на 15m с ядром без HL; fees не враг. S5 = verification arm (скорее 1h на том же ядре) **после** ATR%/cost-in-R замеров — не новый сигнал-суп.

---

*Собрано: Chief coder · Research: Resercher + Researchy · Code notes: CodeReviewer + Backender · 2026-09-06*

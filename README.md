# tui-bot — Binance USDT-M TestNet TUI (Rust)

Терминальный бот для **USDT-M фьючерсов Binance TestNet**: счёт, позиции, график, журнал сделок и три стратегии. Порт Python-проекта `TUI_bot`.

Это фьючерсы. Плечо на аккаунте всегда есть. Бот **сам его не ставит**, пока нет `FUTURES_LEVERAGE`.

Ключи — только из окружения или gitignore-файла `.env` в корне (`chmod 600`). В исходники и в git их не копировать.

Default REST: `https://testnet.binancefuture.com`. Mainnet (`https://fapi.binance.com`) запрещён без `BINANCE_ALLOW_MAINNET=1`.

Без `--live` ордера не отправляются (режим просмотра). `--live` без обоих ключей выходит с кодом 2.

## Примеры запуска

Все команды — из корня репозитория. После `--` идут флаги бота, не cargo. Если `cargo` «не найден»:

```bash
source "$HOME/.cargo/env"
cd /Users/nikolay/gemini_projekts/TUI_bot_RUST
```

### Посмотреть экран, без ордеров

```bash
# один кадр и выход
cargo run -- --dump-frame

# тот же кадр, без сети
cargo run -- --dump-frame --offline

# живой TUI, только смотреть
cargo run

# сразу скальп или тренд, ордеров нет
cargo run -- --strategy 2
cargo run -- --strategy 3 --dump-frame --offline
```

### Торговать на TestNet (нужны ключи в `.env`)

```bash
# стратегия 1, параметры из .env
cargo run -- --live

# сразу скальп / тренд
cargo run -- --live --strategy 2
cargo run -- --live --strategy 3

# стратегия 4 — только в рекомендуемые часы UTC
# 00–02 (Москва 03–05), 07–10 (Москва 10–13), 13–16 (Москва 16–19)
# не зависит от STRATEGY1_ALWAYS_ENTER в .env
cargo run -- --live --strategy 4
```

### Круглосуточно, 40 USDT, до трёх пар

Так сейчас задумано в `.env`. Одноразово, не трогая файл:

```bash
STRATEGY1_ALWAYS_ENTER=1 \
ORDER_NOTIONAL_USDT=40 \
STRATEGY1_MAX_POSITIONS=3 \
cargo run -- --live
```

С явным плечом 5x:

```bash
STRATEGY1_ALWAYS_ENTER=1 \
ORDER_NOTIONAL_USDT=40 \
STRATEGY1_MAX_POSITIONS=3 \
FUTURES_LEVERAGE=5 \
cargo run -- --live
```

Только одна пара (как дефолт кода, не `.env`):

```bash
STRATEGY1_MAX_POSITIONS=1 cargo run -- --live
```

Вернуть окна UTC и номинал 20:

```bash
STRATEGY1_ALWAYS_ENTER=0 ORDER_NOTIONAL_USDT=20 cargo run -- --live
```

`STRATEGY1_ALWAYS_ENTER=1` круглосуточно открывает **только стратегию 1**. Continuation (клавиша `4` / `--strategy 4`) остаётся на окнах UTC, пока нет `STRATEGY4_ALWAYS_ENTER=1`.

### Стратегия 4 по рекомендуемым часам

Новые входы только в UTC-окна **00–02 / 07–10 / 13–16** (конец часа не включён: в 16:00 UTC окно Нью-Йорка уже закрыто). Вне окон статус `вне часов старта`, открытые TP/SL продолжают работать.

```bash
# так и надо запускать Continuation
cargo run -- --live --strategy 4

# те же окна, до трёх пар, 40 USDT
STRATEGY1_MAX_POSITIONS=3 ORDER_NOTIONAL_USDT=40 cargo run -- --live --strategy 4

# только лондонское окно 07–10 UTC
STRATEGY4_ENTRY_HOURS=7-10 cargo run -- --live --strategy 4

# круглосуточно — только стратегия 4, стратегию 1 не трогает
STRATEGY4_ALWAYS_ENTER=1 cargo run -- --live --strategy 4
```

В TUI то же самое: `cargo run -- --live`, затем клавиша `4`. В подвале должно быть:

```text
Continuation: старт Лондон  |  сейчас 07:12 UTC  |  входы 00–02, 07–10, 13–16 UTC
```

или вне окон:

```text
Continuation: вне часов старта  |  сейчас 04:12 UTC  |  входы 00–02, 07–10, 13–16 UTC  |  следующий старт 07:00 UTC
```

Если видите `Continuation: входы круглосуточно` — в `.env` стоит `STRATEGY4_ALWAYS_ENTER=1`. Уберите или запустите с `STRATEGY4_ALWAYS_ENTER=0`.

### Звуки

В TUI звуки включены сами: вход — два высоких, выход — два низких.

```bash
# как обычно (звуки есть)
cargo run -- --live

# без звуков
TRADER_SIGNALS=0 cargo run -- --live
```

### Отчёт, бэктест, тесты, быстрый бинарь

```bash
cargo run -- --backtest
cargo run -- --report
tail -20 .state/trades.jsonl
tail -20 .state/errors.jsonl

cargo test
cargo test --offline

cargo build --release
./target/release/tui-bot --live
./target/release/tui-bot --dump-frame --offline
./target/release/tui-bot --report
```

### Флаги

| Флаг | Что делает |
| --- | --- |
| *(без флагов, TTY)* | интерактивный TUI, ордеров нет |
| `--dump-frame` | напечатать первый кадр и выйти |
| `--offline` | не ходить в сеть, пустой снимок |
| `--strategy 1\|2\|3\|4` | стартовая стратегия (по умолчанию 1) |
| `--live` | реальные ордера на TestNet |
| `--backtest` | прогон по klines, ордеров нет |
| `--report` | сводка `.state/trades.jsonl` и `.state/errors.jsonl` |

## Файл `.env`

Лежит в корне, в `.gitignore`. Права только для вас:

```bash
chmod 600 .env
```

Минимум для `--live`:

```bash
BINANCE_API_KEY=...
BINANCE_API_SECRET=...
BINANCE_FAPI_BASE=https://testnet.binancefuture.com
```

Рабочий пример (входы всегда, 40 USDT на вход, до трёх разных пар, плечо как на Binance, звуки в TUI):

```bash
BINANCE_API_KEY=...
BINANCE_API_SECRET=...
BINANCE_FAPI_BASE=https://testnet.binancefuture.com

STRATEGY1_ALWAYS_ENTER=1
ORDER_NOTIONAL_USDT=40
STRATEGY1_MAX_POSITIONS=3
# FUTURES_LEVERAGE=5
# TRADER_SIGNALS=0
```

После правки `.env` перезапустите процесс (`q` в TUI, затем снова `cargo run -- --live`). Переменные читаются при старте. Окружение процесса важнее файла.

Проверка, что подхватилось — в подвале кадра:

```text
плечо как на Binance  |  сумма 40 USDT  |  корзина до 3: —
Momentum: входы круглосуточно  |  сейчас HH:MM UTC  |  входы круглосуточно
Звуки: покупка — два высоких, продажа — два низких.
```

С `FUTURES_LEVERAGE=5`:

```text
плечо 5x  |  сумма 40 USDT  |  корзина до 3: —
```

## Параметры (env / `.env`)

Пустое значение = взять значение по умолчанию.

### Ключи и биржа

| Переменная | По умолчанию | Смысл |
| --- | --- | --- |
| `BINANCE_API_KEY` | — | ключ TestNet, ≥ 16 символов |
| `BINANCE_API_SECRET` | — | секрет TestNet, ≥ 16 символов |
| `BINANCE_FAPI_BASE` | `https://testnet.binancefuture.com` | REST, только https |
| `BINANCE_ALLOW_MAINNET` | выкл | `1` / `true` / `yes` — иначе `https://fapi.binance.com` отказ |
| `BINANCE_RECV_WINDOW` | `5000` | окно подписи, 100–60000 мс |
| `HTTP_TIMEOUT` | `10` | таймаут HTTP, секунды (до 60) |

### Входы стратегии 1 (моментум)

По умолчанию окна UTC: `00–02` (Азия), `07–10` (Лондон), `13–16` (Нью-Йорк). Скальп и тренд **без окон** (24/7, кроме стопа дня и паузы после сделки). Вне окон новые входы моментума не открываются; уже стоящие TP/SL работают. **Стратегия 4 эти переменные не читает** — у неё свои, ниже.

| Переменная | По умолчанию | Смысл |
| --- | --- | --- |
| `STRATEGY1_ALWAYS_ENTER` | `0` | `1` / `true` / `yes` — входы круглосуточно **только у стратегии 1** |
| `STRATEGY1_ENTRY_HOURS` | `0-2,7-10,13-16` | окна UTC стратегии 1; `*` / `24` / `all` / пусто = 24/7 |
| `STRATEGY1_POLL_SECONDS` | `60` | только `60` или `120` |
| `STRATEGY1_MAX_POSITIONS` | `1` | сколько **разных** лонгов сразу; `1`–`10`. Действует на стратегии **1 и 4**. В примере `.env` — 3 |
| `DAILY_LOSS_USDT` | `20` | стоп дня; `0` = выкл. `r` его не снимает |

`ALWAYS_ENTER` снимает **только часы стратегии 1**. Пауза после стопа, стоп дня и красная 5м свеча остаются. `r` снимает паузу после flatten (`x` `x`), не паузу после стопа.

```bash
STRATEGY1_ALWAYS_ENTER=1 cargo run -- --live
STRATEGY1_ENTRY_HOURS=* cargo run -- --live
STRATEGY1_ENTRY_HOURS=13-16 cargo run -- --live
STRATEGY1_MAX_POSITIONS=3 cargo run -- --live
```

### Входы стратегии 4 (Continuation)

Те же рекомендуемые окна UTC, **отдельно от стратегии 1**. Если в `.env` стоит `STRATEGY1_ALWAYS_ENTER=1`, Continuation всё равно ждёт 00–02 / 07–10 / 13–16.

| Переменная | По умолчанию | Смысл |
| --- | --- | --- |
| `STRATEGY4_ALWAYS_ENTER` | `0` | `1` / `true` / `yes` — входы стратегии 4 круглосуточно |
| `STRATEGY4_ENTRY_HOURS` | `0-2,7-10,13-16` | окна UTC стратегии 4; `*` / `24` / `all` = 24/7 |

Конец окна не включён: `13-16` — с 13:00 до 15:59 UTC. В 16:00 уже `вне часов старта`.

```bash
STRATEGY4_ALWAYS_ENTER=0 cargo run -- --live --strategy 4
STRATEGY4_ENTRY_HOURS=* cargo run -- --live --strategy 4
STRATEGY4_ENTRY_HOURS=13-16 cargo run -- --live --strategy 4
STRATEGY1_MAX_POSITIONS=3 cargo run -- --live --strategy 4
```

### Сумма сделки

`ORDER_NOTIONAL_USDT` — **номинал позиции** в USDT на одну покупку, не маржа.

| Значение | Смысл |
| --- | --- |
| `20` (по умолчанию в коде) | контракт ≈ 20 USDT; если `minNotional` биржи выше (BTC часто 100) — берём минимум |
| `40` | то же на 40 USDT |
| `binance` / `0` / `min` / `exchange` | биржевой `minNotional` по символу |

Маржа примерно `номинал / плечо`. Пример: 40 USDT и 20x → около **2 USDT** залога + комиссия.

Комиссия заложена так: Binance USDT-M **taker 0.04% на сторону** (вход+выход ≈ **0.08%** номинала). Бэктест и журнал считают **нетто** после обеих сторон. TP 2.5% у momentum/continuation ставится **выше на этот круг**, чтобы после комиссий оставалось ~2.5%, а не 2.42%. Скальп/тренд держат пол стопа, чтобы 2R перекрывал комиссию. На live «Прибыль счета» идёт с кошелька биржи — там уже фактическая комиссия Binance.

```bash
ORDER_NOTIONAL_USDT=40 cargo run -- --live
ORDER_NOTIONAL_USDT=binance cargo run -- --live
```

### Плечо

| `FUTURES_LEVERAGE` | Что происходит |
| --- | --- |
| не задан, пусто, `0`, `binance`, `default`, `none`, `off` | бот **не вызывает** API плеча; остаётся то, что на символе в Binance (на TestNet часто 20x) |
| число `1`–`125` | перед каждой покупкой `POST /fapi/v1/leverage` |

Плечо не меняет номинал позиции. Оно меняет **сколько маржи заморожено** и как близко ликвидация.

```bash
cargo run -- --live
FUTURES_LEVERAGE=5 cargo run -- --live
```

### Звуки и прочее

| Переменная | По умолчанию | Смысл |
| --- | --- | --- |
| `TRADER_SIGNALS` | TUI включает сам | `0` / `off` — без звуков; `1` — звуки даже без TUI |
| `TAKE_PROFIT_PCT` | `0.025` | TP стратегии 1 (+2.5%) |
| `TRAIL_PCT` | `0.020` | трейл SL стратегии 1, только вверх |
| `BINANCE_STARTING_EQUITY` | первый живой equity | якорь «Прибыль счета»; не переписывается каждый тик |

Полный шаблон `.env`:

```bash
BINANCE_API_KEY=...
BINANCE_API_SECRET=...
BINANCE_FAPI_BASE=https://testnet.binancefuture.com

STRATEGY1_POLL_SECONDS=60
STRATEGY1_ENTRY_HOURS=0-2,7-10,13-16
STRATEGY1_ALWAYS_ENTER=1
STRATEGY1_MAX_POSITIONS=3
DAILY_LOSS_USDT=20

# стратегия 4: окна UTC 00–02 / 07–10 / 13–16, даже если выше ALWAYS_ENTER=1
# STRATEGY4_ALWAYS_ENTER=0
# STRATEGY4_ENTRY_HOURS=0-2,7-10,13-16

# FUTURES_LEVERAGE=5
ORDER_NOTIONAL_USDT=40
TAKE_PROFIT_PCT=0.025
TRAIL_PCT=0.020

# TRADER_SIGNALS=0
# BINANCE_STARTING_EQUITY=3100
BINANCE_ALLOW_MAINNET=0
```

## Стратегии

В TUI клавиши `1` / `2` / `3` / `4`. Только лонг. **Momentum rider** покупает самые быстрорастущие USDT-M из «Топ роста». Скальп и тренд — **BTC / ETH / SOL**. **Continuation** (4) — откат ликвидных имён, не догон 24h %. TradFi (XAU, TSLA) в skip-листе.

### 1 — Momentum rider (по умолчанию)

Раз в 60 с (или 120) берёт **топ 24h %** среди торгуемых USDT-M (ASCII-тикер, не TradFi). Сколько **разных** лонгов сразу — `STRATEGY1_MAX_POSITIONS` (в коде по умолчанию 1; в примере `.env` — 3). Корзина на экране = те же имена, что и покупки. Если два уже открытых слота в минусе, третий не открывается.

- в корзине рост 24h **+0.4%…+12%**, не пыль; **уже у хая дня** и уже улетевшие (+26% SPK/MORPHO) не покупаются
- красная 5м или выпал из топа — **закрывает до стопа**, не ждёт полный SL
- 5м свеча красная — эту монету в этом скане не берёт
- за сутки UTC счёт просел на `DAILY_LOSS_USDT` — новых входов нет до 00:00 UTC
- свободный слот + монета из корзины → покупка; TP и SL, стоп только вверх
- выпала из топа — позицию не закрывает, ждёт TP/SL
- входы в часы старта, если нет `STRATEGY1_ALWAYS_ENTER=1`
- после стопа/выхода пауза **только по этой монете**, остальные слоты живут

```bash
cargo run -- --live --strategy 1
STRATEGY1_MAX_POSITIONS=3 cargo run -- --live --strategy 1
```

### 2 — Скальп (откат к VWAP / EMA9)

- EMA9 > EMA21, цена выше VWAP, RSI не перекуплен
- вход — отскок к EMA9/VWAP; стоп ATR, цель 2R
- график 5м, круглосуточно, один слот (BTC → ETH → SOL)

```bash
cargo run -- --live --strategy 2
```

### 3 — Тренд (пробой Donchian 20/10, день)

- закрытие выше 20-дневного максимума, цена выше EMA50
- стоп 2 ATR (с полом под комиссию), цель 8R, ведёт трейл
- выход: закрытие ниже 10-дневного минимума / стоп / трейл
- круглосуточно; сделок мало — так задумано

```bash
cargo run -- --live --strategy 3
```

### 4 — Continuation (откат ликвидных, не догон 24h %)

Long-only, **не только BTC/ETH/SOL**. Книга = топ по **объёму** среди имён с умеренным 24h % (+0.4…+4%), без пыли и без касания хая. Вход **только после отката**: красная 5м, затем зелёная с закрытием в верхней половине. Стоп под минимум этих 5м (0.8–2.5%), цель **2R** после комиссии. Догон зелёной 5м на лидере дня — запрещён: так журнал и сжёг счёт (105 минусов из 120, все «биржа закрыла лонг»).

До `STRATEGY1_MAX_POSITIONS` лонгов сразу, по одному новому за скан. Имена, которые **были в топе и разворачиваются**, бот **продаёт** (шорт не открывает). Минус по стопу — **стол молчит до конца текущего окна UTC** (не покупает следующий альт через минуту). Плюс по TP не закрывает стол. Входы **только** в UTC 00–02 / 07–10 / 13–16 (`STRATEGY4_ENTRY_HOURS`); `STRATEGY1_ALWAYS_ENTER` на это не влияет. Слот в минусе — новый не открывается.

```bash
cargo run -- --live --strategy 4
STRATEGY1_MAX_POSITIONS=3 ORDER_NOTIONAL_USDT=40 cargo run -- --live --strategy 4
STRATEGY4_ALWAYS_ENTER=0 cargo run -- --live --strategy 4
cargo run -- --backtest
```

## Клавиши TUI

| Клавиша | Действие |
| --- | --- |
| `1` `2` `3` `4` | стратегия |
| `r` | обновить; снять паузу после flatten и красную ошибку |
| `x` затем `x` | закрыть всё (дважды). Другая клавиша — отмена |
| `q` | выход |

TP/SL — **reduce-only SELL на размер лонга**, не голый `closePosition`. Иначе TestNet после стопа открывает чужой шорт. В live бот сам снимает сиротские TP/SL после закрытого лонга **и на любом символе без живого лонга** (в том числе после рестарта) и **сразу закрывает чужой шорт** (не ждёт `x` `x`). Снимок и тик стратегии — раз в 5 с, как в Python, не на каждом опросе клавиш.

## Звуки входа и выхода

В интерактивном TUI звуки **включены**. Покупка и продажа — **разные** (два высоких vs два низких). В подвале: «Звуки: покупка — два высоких, продажа — два низких.»

| Событие | Звук |
| --- | --- |
| вход (ордер исполнился; в watch — решение войти) | два высоких тона |
| выход по TP/SL или flatten `x` `x` | два низких тона |
| hold / сдвиг стопа | тишина |

macOS: `afplay`. `--dump-frame` молчит, пока явно не стоит `TRADER_SIGNALS=1`.

```bash
cargo run -- --live
TRADER_SIGNALS=0 cargo run -- --live
```

Сделки: `.state/trades.jsonl`. Ошибки подвала: `.state/errors.jsonl`.

```bash
cargo run -- --report
tail -20 .state/trades.jsonl
tail -20 .state/errors.jsonl
```

## Счёт на экране

- **Сумма счета** = кошелёк + нереализованный PnL
- **Прибыль счета** = эта сумма минус закреплённый старт (`BINANCE_STARTING_EQUITY` или `.state/starting_equity`). Старт не переписывается на каждом обновлении.

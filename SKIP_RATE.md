# Skip-rate / S4 closes (measured)

Generated from `.state/trades.jsonl` + `tests/s4_skip_measure.rs`.
**No threshold tuning** (`near_high_frac` stays **0.02**).

Cutoff for journal: events with `ts >= 2026-08-25`, `strategy_id == 4`.

## Method

1. **Book filter mix** (`tests/s4_skip_measure.rs`): take `liquid_universe` (majors out, top `liquid_n`), then count tape skips vs `near_24h_high` among survivors.
   - **Numerator (near_high):** symbols that pass tape filters but fail `near_24h_high(t, near_high_frac)`.
   - **Denominator A:** `liquid_universe − tape_skip` → `near_high / (uni − tape)`.
   - **Denominator B:** `liquid_universe` → `near_high / uni`.
2. **Session tallies:** `note_s4_skip` → `.state/s4_skip_stats.json` (flush on S4 TUI scan).
3. **Journal closes:** post-Aug25 S4 close reasons (outcome, not entry-skip).

## Fixture universe (synthetic 30 alts + majors, `liquid_n=20`)

Measured `cargo test --offline --test s4_skip_measure -- --nocapture`:

| metric | value |
| --- | ---: |
| liquid_universe | **20** |
| tape_skip | **12** |
| near_high_skip | **4** |
| pass (book) | **4** |
| near_high / (uni − tape) | **50.0%** (4/8) |
| near_high / uni | **20.0%** (4/20) |

Session tally top after `pick_strategy4_book` on same fixture:

| n | reason |
| ---: | --- |
| 16 | улетело за день — не догоняю |
| 11 | слабый рост 24h — не вхожу |
| 5 | у 24h high — не догоняю |

## Live public tape (Binance USDT-M 24hr, measured same run)

| metric | value |
| --- | ---: |
| tickers | **668** |
| liquid_universe | **17** |
| tape_skip | **15** (stretch 8 + weak_24h 7) |
| near_high_skip | **1** |
| pass | **1** |
| near_high / (uni − tape) | **50.0%** (1/2) |
| near_high / uni | **5.9%** (1/17) |

On this snapshot near_high is **rare vs stretch/weak** among the liquid book; do **not** retune `near_high_frac` until a session shows it dominating false skips.

## Post-Aug25 S4 closes (journal)

- closes: **14**
- opens (same window): 13
- amends (same window): 0
- wins: **4/14** (WR 28.6%)
- net PnL: **-9.9192** USDT

### Close reasons

| n | reason |
| ---: | --- |
| 6 | биржа закрыла лонг |
| 4 | биржа закрыла лонг по TP |
| 2 | continuation stop from entry |
| 1 | 5м разворот — закрываю до стопа |
| 1 | continuation stop loss |

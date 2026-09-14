# S1 status — freeze ≠ abandon (2026-09-15 Asia/Jerusalem)

**Branch tip for engineering:** `feat/s1-live-solid` (from `main` @ `75d54ad`).

## Freeze means
- No live soak / no edge claims after held-out kills (MAX S1v2, TSMOM S1v3).
- Do **not** delete/disable Momentum Rider. Key `1` / `--strategy 1` stays end-to-end.

## Live path (shipped tip filters)
- Book: **BTC/ETH/SOL only** (`ranking::LIQUID_MAJORS`), 24h **+0.4%…+12%**.
- Exit: TP ≥ 2R of trail, BE@1R then upward trail only. Red 5m / rank-drop do **not** flatten.
- Entry filters kept: green 5m, 4h > EMA20, late-chase (24h≥2.5% without 1h confirm).
- Dropped (train-mirage): mid-band [2%,4%), 3d impulse, MAX lookback, TSMOM.

## Research
- Optional research knobs only behind explicit flags / off-tree harnesses — never default live.
- Do not re-wire killed MAX/TSMOM as edge.

## Gates
- Live restart: explicit **GO** only.
- Merge to main: explicit **GO** only.

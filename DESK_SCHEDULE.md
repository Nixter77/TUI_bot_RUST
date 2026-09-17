# Desk schedule (research)

**Branch:** `feat/desk-schedule`  
**Flag:** `DESK_SCHEDULE=1` — gates **new EnterLong** only. Manage/exit/trail stay on opening `strategy_id`.  
**Not live multi-strat.** No merge without Chief. Edge? **no claim** (S2/S3 held-out red historically).

## Verified code defaults

| Source | Value |
| --- | --- |
| `sessions::DEFAULT_ENTRY_HOURS` | `0-2,7-10,13-16` |
| `STRATEGY4_ENTRY_HOURS` default | same |
| Half-open windows | `[start, end)` UTC hour |

S4 open gaps (no S4): **02–07**, **10–13**, **16–24** UTC.

## Owner clock (locked)

| UTC hours | Owner | Rule |
| --- | --- | --- |
| 00–02, 07–10, 13–16 | **S4** | `s4_entry_windows` |
| 22–24 | **S3** | arm pre/around 1d close |
| 02–07, 10–13, 16–22 | **S2** | scalp in S4/S3 gaps |

**S4 wins** over S3 on any overlap (0–2 is S4; S3 arm does not wrap into 0).

Carry: positions keep opening sid. Central (future live): no double-book symbol; desk heat cap — **not implemented here**.

## Hour → owner

```
00–01 S4 | 02–06 S2 | 07–09 S4 | 10–12 S2 | 13–15 S4 | 16–21 S2 | 22–23 S3
```

## Usage (paper / BT)

```bash
DESK_SCHEDULE=1 cargo test --locked desk_schedule
DESK_SCHEDULE=1 cargo run -- --backtest --strategy 4   # S4 EnterLong only in S4 hours
```

When flag off (default): behavior unchanged.

## Honesty

User isolation idea is logical for **desk heat / session focus**. Profitability of S2/S3 in their slots is **unproven** (prior held-out kills). This branch is a router + tests, not an edge claim.

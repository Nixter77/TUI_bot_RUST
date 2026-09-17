# NOTES desk schedule

**Date:** 2026-09-17 Asia/Jerusalem  
**Base:** `main` @ failclosed merge tip  
**Tip:** `feat/desk-schedule`

## What landed
- `src/desk_schedule.rs`: `desk_owner(utc_hour)`, hour table, `DESK_SCHEDULE=1` EnterLong gate
- `engine::tick_decisions` filters EnterLong when lens ≠ hour owner; exits/trails untouched
- `DESK_SCHEDULE.md` schedule lock
- Unit tests for windows / owner / flag

## edge?
**NO claim.** Isolation logic only.

## Not done
- Live multi-strat / heat cap / double-book guard
- Merge

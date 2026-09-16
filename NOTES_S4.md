# NOTES S4 SetupScore — feat/s4-setup-score

**Date:** 2026-09-16 Asia/Jerusalem  
**Base (branch start):** `c84ae9b` wip park (off older main `75d54ad`)  
**Tip:** see `git rev-parse HEAD` after commit  
**Main tip at work:** `66301d5` (not rebased — SetupScore unfinished + engine override only)

## What shipped (research)
- Finish parked `src/setup_score.rs` (hard/soft over **live S4 gates only** + soft costR)
- Wire `continuation_override` in `engine::tick_decisions` (was ignored → BT could not A/B)
- `DUMP_S4=1` held-out harness (fees + flat funding 0.01%/8h; funding drag muted while `bars_held` still 0 in sim exits)
- Defaults: `setup_score=false` until held-out edge (live BLOCK)

## Held-out A/B (15m + 4h HTF, train 70/held 30, always_enter=true to isolate score)

| arm | train n/WR/PF_net_f | held n/WR/PF_net_f | ship? |
| --- | --- | --- | --- |
| baseline (off) | 9 / 44.4% / 0.442 | 7 / 71.4% / **1.320** | KILL n&lt;30 |
| enter≥75+costR | 0 | 0 | KILL (zeros book) |
| enter≥65+costR | 0 | 0 | KILL |
| enter≥65 no-costR | 3 / 66.7% / 0.939 | 2 / 50% / **0.171** | KILL (worse + thin) |
| costR-only | 2 / 0% / 0 | 1 / 0% / **0** | KILL |

Full-sample baseline (off): n=16 WR56% PF_net 0.738 — stop/wick heavy.

## Honest edge?
**NO.** No arm with held n≥30 and PF_net_f&gt;1 and &gt;baseline. Combined score+costR **zeros** the book; score-only and costR-only **hurt** vs baseline on thin n.

## Not done / risks
- Not rebased onto current main (`feat/s2-live-solid` merge etc.)
- `bars_held` still 0 on many sim exits → funding drag understated (fee PF already kills)
- Fail-closed soak = separate GO
- Live BLOCK / no merge / no `.env` touch

## Kill rule applied
held PF_net_f≤1 OR ≤baseline OR n&lt;30 → research-only.

# NOTES S4 fail-closed / soak readiness

**Date:** 2026-09-16 Asia/Jerusalem  
**Base:** `origin/main` @ `66301d5`  
**Branch tip:** see `git rev-parse HEAD`  
**Goal:** safety path for S4 soak (−2021 / open_meta) — **not** SetupScore, **not** profit claims.

## Already on main (prior tip chain)

| commit | what |
| --- | --- |
| `0ff9e15` | fail-closed immediate-trigger, retry backoff, open_meta sanitize |
| `0f1239f` | −2021 → immediate fail-closed (no hard backoff storm) + tests |
| `7017362` | rearm session-tracked longs despite open_meta tag races |

### Safety behaviors (before → after that series)

| check | before (incident) | after (main) |
| --- | --- | --- |
| algoOrder −2021 on SL/TP | retry / storm (~25k) | `fail_closed_immediate_trigger` → market flatten, no storm |
| mark already through SL | still hit exchange | local precheck → fail-closed |
| corrupt open_meta | nonsense SL / wrong sid | `sanitize_store` / `meta_is_sane` on load + seed |
| cross-strategy manage | S4 trailed S5 / foreign tags | `continuation_owns` + strategy_id isolation |
| rearm miss budget | keep retrying | `REARM_FAIL_MAX` / `REARM_FAIL_BUDGET_SEC` then flatten |

## This branch

- Journal test flake: `close_tags_*` raced `s5_close_*` on shared `open_meta` STORE + same `AVAXUSDT` → serialize + distinct symbols.
- Docs only for soak checklist; **no live restart**.

## Soak gate (separate GO)

1. `cargo test --locked` green on this tip  
2. CodeReviewer / Chief + user GO  
3. Start **S4 only** (`--strategy 4`), S5 paused  
4. Confirm `.state/open_meta.json` sane after load; watch for −2021 (should flatten once, not storm)

## Explicit non-goals

- SetupScore thresholds / profit A/B  
- Live restart from this push  
- Merge without Chief GO  

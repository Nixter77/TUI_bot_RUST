# Desk orchestrator — paper how-to

**Branch:** `feat/desk-orchestrator`  
**Flag:** `DESK_ORCHESTRATOR=1` (default **off**). Exact name — not `DESK_ORCH`.  
**Clock:** reuses `desk_schedule::desk_owner` (S4 windows / S2 gaps / S3 22–24; S4 wins 0–2).  
**edge?** no claim. **Live multi-process:** BLOCK (do not run three `--live` on one key).

## What it does (one process)

1. UTC hour → active **owner** (S2 / S3 / S4).
2. **EnterLong** only from the owner strategy’s logic.
3. **Manage / exit / trail** by **opening sid** from `open_meta` — hour switch does **not** flatten other sids.
4. **Heat** + **anti double-book** same symbol — fail-closed.

## Env knobs

| Env | Default | Meaning |
| --- | --- | --- |
| `DESK_ORCHESTRATOR` | off | `1` / `true` / `yes` enables |
| `DESK_HEAT_MAX` | `3` | max open desk longs (all sids) |
| `DESK_HEAT_MAX_PER_SID` | `2` | max open longs per opening sid |

## Paper (recommended)

```bash
cd ~/gemini_projekts/TUI_bot_RUST
export DESK_ORCHESTRATOR=1
# optional: export DESK_HEAT_MAX=3 DESK_HEAT_MAX_PER_SID=2

cargo build --release
# paper TUI (no --live): owner rotates; manage keeps opening sid
./target/release/tui-bot --strategy 4

# backtest still takes --strategy as the sim lens; orch tick path is shared
# Prefer paper TUI / unit tests to see owner + skip reasons:
cargo test --locked --lib desk_orchestrator
```

**Alive check:** decisions / last_text contain `desk orch: owner S{N}` (and skip reasons like `heat` / `double-book` when blocked). Footer shows `DESK_ORCHESTRATOR`. If you never see that with the flag set, wrong env name or different shell.

## Do not (yet)

```bash
# DO NOT — live multi still BLOCK even with heat in this tip
DESK_ORCHESTRATOR=1 cargo run --release -- --live --strategy 4 &
DESK_ORCHESTRATOR=1 cargo run --release -- --live --strategy 2 &
```

One-process paper first. Live multi needs CR/Chief GO beyond this tip.

## Related

- `DESK_SCHEDULE=1` — single-lens EnterLong gate (still one `--strategy`).
- `DESK_LAUNCH.md` / `DESK_SCHEDULE.md` — schedule launch notes.

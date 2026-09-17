# Desk schedule — how to launch

**Code tip:** on `main` since merge `16f8540` (`feat/desk-schedule`).  
**Flag:** `DESK_SCHEDULE=1` (default **off**). Gates **new EnterLong** only; manage/exit stay on opening `strategy_id`.  
**edge?** no claim. Live multi-strat + desk heat / double-book gate = **not landed**.

Owner clock (UTC, half-open):

| Hours | Owner |
| --- | --- |
| 00–02, 07–10, 13–16 | **S4** |
| 22–24 | **S3** |
| 02–07, 10–13, 16–22 | **S2** |

Quick check: `./scripts/desk_schedule_check.sh`

---

## Safe A — recommended now (one process)

One strategy lens at a time. Flag only blocks opens when **this** lens is not the hour owner.

```bash
cd ~/gemini_projekts/TUI_bot_RUST
git pull
export DESK_SCHEDULE=1

# build once
cargo build --release

# S4 live (opens only in S4 hours when flag on)
./target/release/tui-bot --live --strategy 4
# or:
# cargo run --release -- --live --strategy 4

# radar in another terminal (never sends orders, no live.lock)
cargo run --release -- --monitor --strategy 4
```

Same pattern for other slots when you want to **exercise** that owner:

```bash
export DESK_SCHEDULE=1
cargo run --release -- --live --strategy 2   # opens only in S2 gap hours
cargo run --release -- --live --strategy 3   # opens only in 22–24 UTC
```

Still **one** live process on the TestNet key at a time until heat lands.

---

## Safe B — paper / backtest

No orders. Good for slot smoke (does not prove edge).

```bash
cd ~/gemini_projekts/TUI_bot_RUST
export DESK_SCHEDULE=1

cargo run --release -- --backtest --strategy 4
cargo run --release -- --backtest --strategy 2
cargo run --release -- --backtest --strategy 3
```

Or sequential single-strat paper TUI (no `--live`):

```bash
export DESK_SCHEDULE=1
cargo run --release -- --strategy 4
```

---

## Unsafe — do **not** (until heat / double-book)

Three `--live` processes on **one** API key / one wallet:

```bash
# DO NOT
DESK_SCHEDULE=1 cargo run --release -- --live --strategy 4 &
DESK_SCHEDULE=1 cargo run --release -- --live --strategy 2 &
DESK_SCHEDULE=1 cargo run --release -- --live --strategy 3 &
```

**Why:** schedule only filters EnterLong per process. There is **no** central pre-trade gate for “symbol already owned by another sid” or desk heat. Concurrent live can double-book the same symbol and stack risk. Kill/flatten is per-process, not desk-wide.

---

## Future C — three processes (blocked)

Only after: desk heat cap + double-book reject on fill lineage + CR/Chief GO. Then separate terminals for S4 / S2 / S3 + one `--monitor` is the intended shape. Not enabled yet.

---

## Monitor (always safe)

```bash
cargo run --release -- --monitor --strategy 4
```

Or with live in another terminal — monitor does not take `live.lock` and does not send orders.

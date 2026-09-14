# NOTES S1v2 — multi-day MAX A/B vs tip (2026-09-14 Asia/Jerusalem)

**Branch/hash:** `feat/s1-momentum-edge` @ **`0658903`**
**Gate:** SHIP only if held-out WR↑ and PF_net≥1 vs tip. Bleed↓ alone = research-only. No live. No merge.

## 0) Diagnose — exit mix + early MFE

### Journal live S1 (`.state/trades.jsonl`)
- S1 closes n=9, WR=0%, sum pnl≈−5.56 (mostly **alt-chase** MORPHO/SPK/… before majors-only book).
- Exit-mix: exchange-close 8 / early red-exit 1. **No MFE fields** on those rows → journal useless for 1–4h MFE.

### Cargo `--backtest` tip @0658903 (short `.state/klines` ~5d majors)
- Exit-mix (DUMP): **SL-dominated** (ETH 2× stop wick, 1× EOD; SOL 1× EOD tiny +). n≈4, WR~25% if counting EOD, **PF≪1**. bars_held often 0 on wick stops (same-bar geometry noise on thin cache).
- README shipped table: baseline n=7 WR0% pnl−2.19 → after filters n=2 WR0% pnl−0.77. **Still unprofitable.**

### Research mirror ~28d (`klines_s1_research`, tip filters, full sample)
| | n | WR | PF_net | exit-mix | avg MFE 1h/2h/4h | note |
| --- | ---: | ---: | ---: | --- | --- | --- |
| raw 24h | 117 | 40.2% | 1.24 | SL71/TP46 | 0.82 / 1.10 / 1.35 | train-regime optimistic |
| tip 0658903 | 101 | 42.6% | 1.40 | SL58/TP43 | 0.80 / 1.05 / 1.34 | trail/SL-heavy; early MFE thin |

Funding drag modeled as **+0.01%/8h** on notional (long). PF_net includes it.

## 1) Held-out table (last 30% of ~28d) — SHIP gate

Universe BTC/ETH/SOL. Arms = **one** lookback (3d XOR 5d XOR 10d MAX high), not stacked. Entry: close within 1% of lookback high AND 24h ≤40% of Nd return. TP2.5/trail2.0. **small_n=YES if n<15**.

| arm | n | WR | PF_net | PF_gr | exp | MFE1h | MFE2h | MFE4h | exit-mix | fees | fund | hold | small_n |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: | ---: | --- | --- |
| baseline tip 0658903 | 11 | 9.1% | 0.20 | 0.20 | −0.1750 | 0.25 | 0.47 | 0.53 | SL10/TP1 | 0.18 | 0.043 | 15.6h | YES |
| raw 24h book | 15 | 6.7% | 0.14 | 0.14 | −0.2007 | 0.31 | 0.52 | 0.58 | SL14/TP1 | 0.24 | 0.068 | 18.0h | no |
| MAX 3d near-high | 0 | — | — | — | — | — | — | — | — | 0 | 0 | — | YES |
| MAX 5d near-high | 2 | 0.0% | 0.00 | 0.00 | −0.3792 | 0.10 | 0.10 | 0.10 | SL2 | 0.03 | 0.004 | 7.2h | YES |
| MAX 10d near-high | 0 | — | — | — | — | — | — | — | — | 0 | 0 | — | YES |

### Train contrast (do **not** ship from train)
MAX **3d** looked good on train (n=15 WR66.7% PF_net3.38) but **n=0 on held-out** → classic train mirage / regime gap. Not an edge.

## 2) Sensitivity (held-out only; still one lookback)

Looser near-high / late-share to get n>0:

| arm | params | n | WR | PF_net | edge? |
| --- | --- | ---: | ---: | ---: | --- |
| MAX 3d | near2% late50% | 5 | 20% | 0.44 | **no (bleed↓ only)** |
| MAX 5d | near2% late40–50% | 5 | 0% | 0.00 | **no** |
| MAX 10d | near2% late40–50% | 2 | 0% | 0.00 | **no (n too small)** |

Windows-only on MAX arms: held-out n=0.

## 3) Verdict

| question | answer |
| --- | --- |
| **edge?** | **no** |
| Beats `0658903` on held-out WR+PF_net≥1? | **no** |
| Ship / new strategy commit? | **no** |
| Ping CR? | **no** (nothing to review for ship; research-only stop) |
| Live / merge main? | **not done** (hard rules) |

Smallest honest action: **stop**. Tip remains `0658903`. Full dump: `.state/s1v2-max-research.txt`.

## 4) Method notes
- Research harness: `/tmp/s1v2_max_research.py` (not in crate; mirror of S1 TP/trail + fees).
- Not cargo-identical book ranking (per-symbol walk); cargo short-cache n too small for MAX arms — held-out call made on 28d research cache.
- Exit A / fail-closed untouched. Working tree kept clean of strategy diffs.

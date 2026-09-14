# NOTES S1v3 — TSMOM sign(r_K)>0 (2026-09-15 Asia/Jerusalem)

**Branch:** `feat/s1-tsmom-v3` @ base `0658903`. Research-only. NOT live.

## Held-out table (PRIMARY = ~28d, vs tip PF_net~0.20)
| arm                          |  n |     WR | PF_net | PF_gr |      exp | MFE1h | MFE2h | MFE4h | exit-mix               | fees   | fund    | hold  | small_n |
| ---------------------------- | --: | -----: | -----: | ----: | -------: | ----: | ----: | ----: | ---------------------- | -----: | ------: | ----: | ------: |
| tip 0658903 mirror           |  18 |  16.7% |   0.42 |  0.42 |  -0.1118 |  0.74 |  0.82 |  0.86 | SL14/TP3/EOD1          |   0.29 |  0.0187 |  11.8h | no  |
| TSMOM K=20 +fundgate         |  10 |   0.0% |   0.00 |  0.00 |  -0.1374 |  0.29 |  0.39 |  0.50 | SL9/SIGNAL1            |   0.12 |  0.0143 |  15.1h | YES |
| TSMOM K=10 +fundgate         |   6 |   0.0% |   0.00 |  0.00 |  -0.1790 |  0.25 |  0.29 |  0.40 | SL6                    |   0.07 |  0.0106 |  19.8h | YES |
| TSMOM K=30 +fundgate         |   0 |      — |      — |     — |        — |     — |     — |     — | —                      |   0.00 |  0.0000 |   0.0h | YES |
| TSMOM K=20 no fundgate       |  10 |   0.0% |   0.00 |  0.00 |  -0.1374 |  0.29 |  0.39 |  0.50 | SL9/SIGNAL1            |   0.12 |  0.0143 |  15.1h | YES |

### Sensitivity held-out (~90d) — not ship gate
| arm                          |  n |     WR | PF_net | PF_gr |      exp | MFE1h | MFE2h | MFE4h | exit-mix               | fees   | fund    | hold  | small_n |
| ---------------------------- | --: | -----: | -----: | ----: | -------: | ----: | ----: | ----: | ---------------------- | -----: | ------: | ----: | ------: |
| tip 0658903 mirror           | 113 |  42.5% |   1.37 |  1.38 |  +0.0551 |  0.79 |  1.00 |  1.26 | SL64/TP48/EOD1         |   1.81 |  0.1597 |   8.9h | no  |
| TSMOM K=20 +fundgate         |  27 |  44.4% |   1.67 |  1.69 |  +0.0655 |  0.21 |  0.31 |  0.41 | SL14/TP10/FUND_CUT2/SIGNAL1 |   0.33 |  0.0437 |  18.4h | no  |
| TSMOM K=10 +fundgate         |  22 |  50.0% |   1.69 |  1.72 |  +0.0633 |  0.24 |  0.32 |  0.45 | SL11/TP8/FUND_CUT2/SIGNAL1 |   0.26 |  0.0396 |  19.1h | no  |
| TSMOM K=30 +fundgate         |  27 |  48.1% |   1.76 |  1.79 |  +0.0747 |  0.22 |  0.33 |  0.43 | SL14/TP11/FUND_CUT2    |   0.32 |  0.0524 |  19.9h | no  |
| TSMOM K=20 no fundgate       |  35 |  51.4% |   2.94 |  2.99 |  +0.1355 |  0.60 |  0.75 |  0.95 | TP18/SL16/SIGNAL1      |   0.42 |  0.0671 |  17.6h | no  |

| question | answer |
| --- | --- |
| **edge?** | **no** |
| vs tip 0658903 PF_net~0.20 | primary K=20 PF_net=0.00 n=10 WR=0.0; tip mirror PF_net=0.42 |
| kill? | **yes** — PF_net=0.00≤1.0 |
| Ping CR? | **no** |
| Live / merge? | **not done** |

## Action
**Stop.** Freeze S1v3 → recommend **S4**. No filter-soup chase. 90d green cell = regime mirage (tip also PF_net>1 there).

Full dump: `.state/s1v3-tsmom-research.txt`
Harness: `/tmp/s1v3_tsmom_research.py` (research-only; no production wire).
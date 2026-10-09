# The bench, night by night

Each number is how many times as long the rucc server took as the gcc one, built from the same pin at the same level and run one after the other on the same runner, by the median of the night's runs. The analytic column is the geometric mean over the twenty queries. Written by `rpg bench-night`.

| night | pin | level | rucc | select-only | tpcb-like | analytic | make check |
|---|---|---|---|---:|---:|---:|---:|
| 2026-10-09 | REL_18_6 | -O2 | rucc 0.24.8 | 1.10 | 1.10 | 1.08 | 1.08 |
| 2026-10-08 | REL_18_6 | -O2 | rucc 0.24.8 | 1.08 | 1.15 | 0.97 | 1.08 |
| 2026-10-07 | REL_18_6 | -O2 | rucc 0.24.8 | 1.12 | 1.05 | 1.08 | 1.08 |
| 2026-10-06 | REL_18_6 | -O2 | rucc 0.24.8 | 1.10 | 1.07 | 1.10 | 1.08 |
| 2026-10-05 | REL_18_6 | -O2 | rucc 0.20.0 | 1.12 | 1.08 | 1.27 | 1.12 |
| 2026-10-04 | REL_18_6 | -O2 | rucc 0.19.0 | 1.16 | 1.16 | 1.17 | 1.12 |

---
type: reference
summary: Same-harness Kaelis measurements, honest claims and chart rules.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-14
title: Kaelis Benchmark Protocol
tags:
  - kaelis
  - benchmarks
  - performance
status: active
updated: 2026-11-14
---

# Kaelis Benchmark Protocol

## Harness

`target/bench-lite.ps1` (local scratch; not committed):

- 3 runs per build.
- `--version` time, process start to `MainWindowHandle`, idle working set after
  2.5 s, idle CPU over 1 s.
- Empty data root per run; process stopped in `finally`.
- Median reported, not mean.

The older `tools/bench.ps1` used a different method and is historical only. Do
not put its rows beside `bench-lite` rows.

## Latest same-harness data

Measured 2026-11-10 on one Windows x64 host:

| Release | Memory | Startup | CLI |
|---|---:|---:|---:|
| 0.3.7 | 86.6 MB | 285.3 ms | 62.5 ms |
| 0.4.5 | 19.4 MB | 288.8 ms | 63.0 ms |
| 0.4.6 | **18.9 MB** | 296.8 ms | 63.1 ms |

**The real result is memory: −78%.** Startup and CLI move by more than the
difference between builds; call them flat/noise.

Binary size: 16.44 / 16.43 / 16.45 MB — within 20 KB.

## Release comparison (`tools/bench.ps1`)

This committed harness is **not comparable** with the older `bench-lite` rows:
3 runs, 5 s settle, 10 s CPU sample, empty and 200-mod fixtures. Do not put both
datasets in one chart.

| Release | CLI | Empty | Heavy | Private | Idle |
|---|---:|---:|---:|---:|---:|
| 0.4.9 | 62 ms | 446 ms | 434 ms | ~91.0 MB | 0.16 / 0.00% |
| 0.4.10 | 62 ms | 549 ms | 424 ms | 90.6 / 91.8 MB | 0.16 / 0.31% |
| 0.4.11 | 67 ms | 733 ms | 705 ms | 90.6 / 91.0 MB | 0.00 / 0.00% |

0.4.9 and 0.4.10 were flat within run-to-run noise. 0.4.11 did not improve
startup, memory or idle CPU; its published run was slower on startup. One empty
sample was 1,299 ms and earlier runs also varied, so this is not yet proof of a
regression. The real 0.4.11 win is binary size: 17,088,000 B, 66,048 B below
0.4.10.

## Rules

- Bars start at zero.
- Print the value at the bar end; do not rely on colour.
- Direct labels over a legend where possible.
- Provide a data table fallback.
- Never call a flat result a speedup.
- Re-measure after any release binary changes; keep both builds under different
  filenames so the older-build sweeper cannot delete the baseline.

## Links

- [[Kaelis Web System]]
- [[Kaelis Current State]]
- [[Kaelis Roadmap]]

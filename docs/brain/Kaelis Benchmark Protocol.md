---
type: reference
summary: Same-harness Kaelis measurements, honest claims and chart rules.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-10
title: Kaelis Benchmark Protocol
tags:
  - kaelis
  - benchmarks
  - performance
status: active
updated: 2026-11-10
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

---
type: index
summary: Index of Kaelis architecture, release, benchmark, web, decision, roadmap and current-state notes.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-15
title: Kaelis Brain
tags:
  - kaelis
  - index
  - project
status: active
updated: 2026-11-15
---

# Kaelis Brain

Shared project memory for humans and agents. This folder is the source of truth;
copy it into Obsidian with
`tools/sync-brain.ps1 -VaultPath "<vault>" -Subfolder "01-Projects/Kaelis/Brain"`.

> [!important] Operating rule
> If a version, benchmark, command or release detail changes, update the matching
> note here in the same change.

## Map

- [[Kaelis Architecture]] — crates, data flow, safety boundaries.
- [[Kaelis Repo Graph]] — crate, runtime, release and web edges.
- [[Kaelis Build And Release]] — build, test, tag, artifact and Pages flow.
- [[Kaelis Benchmark Protocol]] — how to measure and what the numbers mean.
- [[Kaelis Web System]] — page structure, radius scale, aurora, charts.
- [[Kaelis Decisions]] — decisions that should not be re-litigated by accident.
- [[Kaelis Roadmap]] — next measurable improvements.
- [[Kaelis Current State]] — latest release, commit, site and open items.

## Current snapshot

- Latest release: **v0.4.12** (2026-11-15).
- Workspace: **0.4.12** — Windows 11 rounded corners restored on the borderless
  window, with 0.4.11's tray minimize and rustls-only clients intact.
- Product page: https://fuvji.github.io/kaelis/ (live at v0.4.12).
- Historical `bench-lite` memory: **18.9 MB**, down from 86.6 MB on 0.3.7.
- 0.4.11 release comparison (`tools/bench.ps1`): 67 ms CLI, 733 ms empty,
  705 ms heavy, 90.6/91.0 MB private, 0.00% idle. No startup or memory win;
  the published run was slower than 0.4.10 but startup variance is high.
- Binary win: Windows EXE 17,088,000 B, **66,048 B smaller** than 0.4.10;
  0.4.12 leaves it unchanged.
- Rounded check: the published 0.4.12 window reports `corner_preference=2`; the
  desktop capture shows all four clipped corners. Windows 10 stays square.
- Repo: https://github.com/fuvji/kaelis

## Update checklist

1. Change code or content.
2. Run the checks in [[Kaelis Build And Release]].
3. Update [[Kaelis Current State]] and any affected note.
4. Commit and push.
5. Sync the brain to Obsidian.

## Links

- [[Kaelis Current State]]
- [[Kaelis Roadmap]]
- [[Kaelis Decisions]]

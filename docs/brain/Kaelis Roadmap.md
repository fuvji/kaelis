---
type: reference
summary: Measured post-0.4.12 candidates, maintenance and deferred Kaelis work.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-15
title: Kaelis Roadmap
tags:
  - kaelis
  - roadmap
status: active
updated: 2026-11-15
---

# Kaelis Roadmap

## After 0.4.12

0.4.12 restores Windows 11's rounded compositor clip on the borderless window;
the published window reports `corner_preference=2` and the desktop capture shows
the corners. It is visual-only: the Windows EXE is still 17,088,000 B and no
startup, memory or CPU behavior changed. The latest performance comparison
remains 0.4.11's `tools/bench.ps1` run: 733 ms empty and 705 ms heavy versus
0.4.10's 549/424 ms, 90.6/91.0 MB private versus 90.6/91.8 MB, and 0.00% idle
both. One 1,299 ms empty sample makes the startup delta noisy, but there is no
win to claim.

1. **Interleaved startup audit.** Run 0.4.10 and 0.4.11 alternately in one
   session, at least five runs each, and compare medians and p95. Investigate
   the window-creation path only if the regression repeats; the 1,299 ms first
   sample points at host noise or cache state.
2. **First-frame work.** Verify the older-build sweep and settings read stay off
   the paint path; instrument rather than guess.
3. **Further binary size.** The TLS removal proved the approach; look for the
   next real reduction and record exact bytes against the 0.4.12 assets.
4. **Window checks stay manual.** The published binary's DWM preference and
   desktop capture are now verified; keep both in the release checklist, and
   keep a manual tray-restore check because native clicking is blocked here.

Acceptance: a change with a repeatable improvement beyond host noise, or an
honest "no change" and no version bump for performance alone.

## Maintenance

- Keep compatible dependency patches current; CI is the compile gate on this host.
- Regenerate previews from the tagged binary, never from a stale build.
- Run accessibility/Lighthouse review on the benchmark lab and hero.
- Add a check that every screenshot status bar matches the release version.
- Keep `site/downloads` metadata synchronized with the latest release.

## Deferred

- More chart views only if they answer a real question; no decorative dashboards.
- Custom domain and email; not needed for product correctness.
- Auto-update channel for macOS/Linux; archives are deliberate today.

## Links

- [[Kaelis Benchmark Protocol]]
- [[Kaelis Current State]]
- [[Kaelis Decisions]]

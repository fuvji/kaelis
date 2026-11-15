---
type: reference
summary: Measured post-0.4.11 candidates, maintenance and deferred Kaelis work.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-14
title: Kaelis Roadmap
tags:
  - kaelis
  - roadmap
status: active
updated: 2026-11-14
---

# Kaelis Roadmap

## After 0.4.11

0.4.11 ships two concrete changes: Windows header minimize hides to the tray,
and reqwest builds only rustls. The Windows EXE is 17,088,000 B, 66,048 B below
0.4.10. The published `tools/bench.ps1` run did **not** improve startup, private
memory or idle CPU: 733 ms empty and 705 ms heavy versus 0.4.10's 549/424 ms,
90.6/91.0 MB private versus 90.6/91.8 MB, and 0.00% idle both. One 1,299 ms
empty sample makes the startup delta noisy, but there is no win to claim.

1. **Interleaved startup audit.** Run 0.4.10 and 0.4.11 alternately in one
   session, at least five runs each, and compare medians and p95. Investigate
   the window-creation path only if the regression repeats; the 1,299 ms first
   sample points at host noise or cache state.
2. **First-frame work.** Verify the older-build sweep and settings read stay off
   the paint path; instrument rather than guess.
3. **Further binary size.** The TLS removal proved the approach; look for the
   next real reduction and record exact bytes against the 0.4.11 assets.
4. **Tray restore coverage.** The hide path compiled and the fallback is the
   existing tray action, but native clicking was blocked here. Add a manual
   Windows check to the release checklist.

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

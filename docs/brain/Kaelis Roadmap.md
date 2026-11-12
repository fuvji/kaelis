---
type: reference
summary: Measured 0.4.9 candidates, maintenance and deferred Kaelis work.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-12
title: Kaelis Roadmap
tags:
  - kaelis
  - roadmap
status: active
updated: 2026-11-12
---

# Kaelis Roadmap

## Next: 0.4.9

0.4.9 is a correctness release: a clean no-Java Forge instance repairs its
files, JVM and loader in order, and the macOS release build is pinned to Rust
1.89.0. It does not claim a measured speed change. The measured candidates
below remain open for the next pass, in order:

1. **Startup path audit.** Find one allocation, disk read or synchronous call on
   the window-creation path; measure before/after with `bench-lite.ps1`.
2. **First-frame work.** Verify the older-build sweep and settings read stay off
   the paint path; instrument rather than guess.
3. **Binary/bundle size.** Check for duplicate native libraries or unused
   features; record exact bytes.

Acceptance: a change with a repeatable improvement beyond host noise, or an
honest "no change" and no bump.

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

---
type: reference
summary: Current Kaelis release, repo, site, dependency and open-item snapshot.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-11
title: Kaelis Current State
tags:
  - kaelis
  - state
status: active
updated: 2026-11-11
---

# Kaelis Current State

> [!info] Snapshot
> Latest release **v0.4.6**; product page live; workspace version 0.4.6.

## Repo

- Branch `main`.
- Site/repo graph commit: `ca11a2c`; this upkeep follows it.
- Latest tag: `v0.4.6` (2026-11-10T22:02:25Z).
- Repo graph baseline: `f6da23c`; see [[Kaelis Repo Graph]].
- Windows release asset: `Kaelis.exe`, 17,250,816 B.
- SHA-256: `2d5b03bae73066e0fdf0366bdc62cd48a68e506d3f57cc431680538cb1db0370`.
- Release: https://github.com/fuvji/kaelis/releases/tag/v0.4.6

## Site

- https://fuvji.github.io/kaelis/
- Benchmark lab: 0.3.7 / 0.4.5 / 0.4.6, same harness.
- Memory 86.6 → 18.9 MB (−78%); startup and CLI flat/noise.
- Radius 6/10/14/20/28/999 px; static grain; transform-only aurora.
- Seven 1080×700 previews from the 0.4.6 binary.
- Smart download picker: Windows portable ZIP, macOS universal DMG, Linux
  AppImage; every format remains a direct row. No third-party request.
- Eight artifacts have size/hash slots; static fallbacks now match the v0.4.6
  release digests, not the older local `site/downloads` copies.
- Preview tilt and the benchmark trend draw are disabled by reduced motion.
- Superseded preview-card motion rules and the old button easing were removed;
  the later motion block and shared control easing are the only definitions.
- Pages deploy green; direct binary on Pages is 404 by design.

## Dependencies

- 0.4.6 shipped after a 24-crate compatible patch sweep; `webbrowser` 1.2.7
  is now the only compatible patch left. The other 22 newer versions sit
  outside the manifests' current ranges.
- Workflows use current supported majors: `checkout@v6`,
  `upload-pages-artifact@v4`, `softprops/action-gh-release@v3`; artifact
  upload/download remain v4, `configure-pages` v5, `deploy-pages` v4.
- Dependabot checks Cargo and GitHub Actions weekly in
  `.github/dependabot.yml`; minor and patch Cargo updates are grouped.
- Local Rust linking remains blocked by missing `dlltool.exe`; CI is the compile
  and test gate.
- Current Rust: 1.99.1; minimum supported: 1.85.

## Open

- Brain synced into the user's Obsidian vault under `01-Projects/Kaelis/Brain`.
- CI for `ca11a2c` completed: Windows, macOS and Site assets passed; Ubuntu
  stopped in `apt-get` before Rust. The 2026-11-11 upkeep retries apt fetches,
  caps the step at ten minutes and updates the action majors; its run is the
  gate.
- Plan the first measured 0.4.7 optimization.

## Links

- [[Kaelis Brain]]
- [[Kaelis Build And Release]]
- [[Kaelis Roadmap]]

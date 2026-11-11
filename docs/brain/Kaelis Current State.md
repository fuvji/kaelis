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
> The setup, vault-graph, hero-frame and preview-dialog pass is live. Windows
> Setup now has always-visible header and hero actions plus a dedicated row.

## Repo

- Branch `main`.
- Site commit: `583d614`; Pages run 97 green.
- Latest tag: `v0.4.6` (2026-11-10T22:02:25Z).
- Repo graph baseline: `f6da23c`; see [[Kaelis Repo Graph]].
- Windows release asset: `Kaelis.exe`, 17,250,816 B.
- SHA-256: `2d5b03bae73066e0fdf0366bdc62cd48a68e506d3f57cc431680538cb1db0370`.
- Windows setup asset: `Kaelis-Setup.exe`, 6,126,053 B.
- Setup SHA-256: `b1ed1b81f4d5bbd20a7e42d8a4690406dfb48e07101495fd877dd8729012d209`.
- Local `dist/Kaelis-Setup.exe` and `site/downloads/Kaelis-Setup.exe` are synced
  to the v0.4.6 release; `dist/build.json` and both SHA-256 lists match it.
- Release: https://github.com/fuvji/kaelis/releases/tag/v0.4.6

## Site

- https://fuvji.github.io/kaelis/
- Benchmark lab: 0.3.7 / 0.4.5 / 0.4.6, same harness.
- Memory 86.6 → 18.9 MB (−78%); startup and CLI flat/noise.
- Radius 6/10/14/20/28/999 px; static grain; transform-only aurora.
- Seven 1080×700 views from the 0.4.6 binary (hero Overview plus six gallery
  cards), opened in one native dialog with arrow/Esc navigation, focus return
  and scroll lock. The hero capture has its own desktop-status rail.
- Fixed vault graph: 24 nodes, 47 edges, three labels, three highlighted
  traces, one inline SVG. No third-party request or filter; the former dash
  animation and film pulse are removed.
- Smart download picker: Windows portable EXE, macOS universal DMG, Linux
  AppImage; Windows also exposes the optional setup. Direct rows cover Windows
  EXE/setup, macOS DMG/ZIP and Linux `.deb`/AppImage/`tar.gz`. The sticky header
  and hero carry platform-independent Setup actions, the downloads header has a
  text link, and the installer row has an accent rail and `INSTALLER` chip. The
  artifact tile follows the selected platform. No third-party request.
- The sticky header carries a scroll-driven progress hairline with no scroll
  listener; reduced motion and unsupported timelines hide it.
- The Windows ZIP is retired from current links and future releases. The
  optional per-user setup is restored as `Kaelis-Setup.exe` and hydrated from
  the release; 0.4.6 keeps its old artifacts as history.
- Seven artifacts have size/hash slots; static fallbacks match the v0.4.6
  release digests, not the older local `site/downloads` copies.
- The Windows native frame is cleared through DWM attributes and the first clear
  is dark, so the undecorated window does not flash a pale edge.
- The film poster is deferred until the film is within 300 px. In a 520×944
  Chromium check, its 147,409 B was absent from the initial resource list and
  loaded after scrolling. The play control is a real MP4 link before JavaScript.
- Preview tilt, graph drift, hero entrance, benchmark value fade and the
  benchmark trend draw are disabled by reduced motion.
- The picker tints its recommendation per OS; checksum rows copy in place.
- Mobile has a native section menu; the preview dialog uses native modal focus,
  `aria-describedby`, pointer-safe backdrop dismissal and stable scrollbar
  geometry. Metadata contrast was lifted from `#6e7580` to `#808996`.
- `content-visibility: auto` remains only on below-fold sections; the graph
  removed its continuous dash paint loop and the aurora dropped `will-change`.
- Pages deploy run 97 green. Live page is 200 with the header, hero, picker and
  row Setup links. Local Chromium checks at 320, 390 and 1,440 px, plus the live
  page at 520 px, show no horizontal overflow. Direct binary on Pages is 404 by
  design.

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
- CI at `21e9007`: Site assets and macOS passed; Windows and Ubuntu were still
  in their release build at the last check.
- The setup, vault-graph, hero-frame and preview-dialog pass is live; a
  full-page visual review beyond the measured layout checks remains.
- Dependabot opened #1–#6 for major dependency updates; review them instead of
  merging by default.
- Plan the first measured 0.4.7 optimization.

## Links

- [[Kaelis Brain]]
- [[Kaelis Build And Release]]
- [[Kaelis Roadmap]]

---
type: reference
summary: Durable Kaelis choices about versioning, performance claims, distribution and web.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-15
title: Kaelis Decisions
tags:
  - kaelis
  - decisions
status: active
updated: 2026-11-15
---

# Kaelis Decisions

## Version line

- 0.4.6 resumed the requested **0.4.x** numbering; 0.4.12 is the current release.
- 0.5.0 and 0.6.0 tags/releases remain published as history; do not delete them.
- Next bump is **0.4.13** only when launcher code changes. A web-only pass does
  not invent a version.
- 0.4.10 removed UI chrome and dead source captures, but the Windows EXE grew
  one 4 KiB page. Treat that as a cleanup release, not a size or speed win.
- 0.4.11 is a real size win: the Windows EXE is 17,088,000 B, 66,048 B below
  0.4.10. It is **not** a speed win; startup and memory showed no repeatable
  improvement, and the measured startup was slower in the published run.
- 0.4.12 is visual-only: restore `DWMWCP_ROUND` while keeping the invisible
  border and borderless edge. Do not describe it as a performance or size win;
  the EXE is unchanged at 17,088,000 B.

## Performance claims

- Same host, fixture, harness and median-of-three or no comparison.
- `tools/bench.ps1` release rows and older `bench-lite.ps1` rows are separate
  datasets; do not put them in one chart.
- Startup variation on this host is large. Interleave builds in one session
  before calling a regression; one 1,299 ms sample did not prove one.
- Never zero retry, timeout, process-wait or safety timers to claim zero
  latency. Measure real work instead.
- Memory is the measured win; start-up and CLI are flat within noise.
- No overclocking, no impossible latency claim, no synthetic competitor.

## Distribution

- GitHub Releases host binaries.
- Pages serves page, previews, checksum and links text only; binaries are deleted
  from the artifact before upload.
- Release metadata (`KAELIS-LINKS.txt`, `SHA256SUMS.txt`) is copied into
  `site/downloads` so repo and live copies agree.

## Window and TLS

- On Windows, the header minimize control hides to the tray when a tray icon is
  available. If tray installation failed, it uses ordinary taskbar minimize so
  the window cannot become unrecoverable.
- Non-Windows keeps native minimize; hiding a viewport there can block the
  restore event.
- Build one TLS stack, rustls. Do not re-enable reqwest defaults or native-tls;
  keep HTTP/2, charset handling and macOS system-proxy support.

## Packaging

- Windows ships a portable EXE and an optional per-user NSIS setup; no ZIP.
  The setup installs under `%LOCALAPPDATA%\Programs\Kaelis` with a Start menu
  shortcut and uninstaller, and carries the same binary as the portable EXE.
- All packagers read root `Cargo.toml`; crate manifests use
  `version.workspace = true`.
- One version feeds About, tray, update checks and package metadata.
- CI and release pin Rust **1.89.0**. The GUI reaches `objc2` 0.4.1 through
  `winit` 0.29, which no longer builds on current stable, and `uuid` 1.29 needs
  1.89. Do not float the pin without upgrading that window stack.

## Web

- No ad network; GitHub Sponsors only, labelled.
- Single inline document; no font/framework/CDN dependency.
- Rounded web geometry, transform-only aurora, static grain, reduced motion.
  The native Windows border and visible frame are removed, but the Windows 11
  rounded-corner preference stays; Windows 10 keeps square corners.
- Sections paint without a reveal observer; the film poster is lazy.
- Charts are honest: zero baseline, direct values, data table, flat labelled flat.

## Brain

- `docs/brain` is the source of truth for humans and agents.
- Obsidian is a synced view, not a second divergent copy.
- Update the brain in the same commit as the change it describes.

## Links

- [[Kaelis Brain]]
- [[Kaelis Current State]]
- [[Kaelis Roadmap]]

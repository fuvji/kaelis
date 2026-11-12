---
type: reference
summary: Durable Kaelis choices about versioning, performance claims, distribution and web.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-11
title: Kaelis Decisions
tags:
  - kaelis
  - decisions
status: active
updated: 2026-11-11
---

# Kaelis Decisions

## Version line

- 0.4.6 resumed the requested **0.4.x** numbering; 0.4.7 is the current release.
- 0.5.0 and 0.6.0 tags/releases remain published as history; do not delete them.
- Next bump is **0.4.8** only when launcher code changes. A web-only pass does not
  invent a version.

## Performance claims

- Same host, fixture, harness and median-of-three or no comparison.
- Old `tools/bench.ps1` rows are not comparable with `bench-lite.ps1`.
- Memory is the measured win; start-up and CLI are flat within noise.
- No overclocking, no impossible latency claim, no synthetic competitor.

## Distribution

- GitHub Releases host binaries.
- Pages serves page, previews, checksum and links text only; binaries are deleted
  from the artifact before upload.
- Release metadata (`KAELIS-LINKS.txt`, `SHA256SUMS.txt`) is copied into
  `site/downloads` so repo and live copies agree.

## Packaging

- Windows ships a portable EXE and an optional per-user NSIS setup; no ZIP.
  The setup installs under `%LOCALAPPDATA%\Programs\Kaelis` with a Start menu
  shortcut and uninstaller, and carries the same binary as the portable EXE.
- All packagers read root `Cargo.toml`; crate manifests use
  `version.workspace = true`.
- One version feeds About, tray, update checks and package metadata.

## Web

- No ad network; GitHub Sponsors only, labelled.
- Single inline document; no font/framework/CDN dependency.
- Rounded web geometry, transform-only aurora, static grain, reduced motion.
  The native Windows frame is removed instead of rounded.
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

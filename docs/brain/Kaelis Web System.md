---
type: reference
summary: Kaelis product page layout, radius scale, aurora, grain and benchmark lab.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-11
title: Kaelis Web System
tags:
  - kaelis
  - web
  - design
status: active
updated: 2026-11-11
---

# Kaelis Web System

One inline HTML document in `site/index.html`. No framework, webfont, analytics,
cookie or third-party request. The download picker is local JavaScript; every
artifact row is a direct link when scripting is off.

## Shape scale

| Token | Value | Use |
|---|---:|---|
| `--r-sm` | 10 px | skip link, small controls |
| `--r-md` | 14 px | commands, disclosures |
| `--r-lg` | 20 px | previews, stats, support |
| `--r-xl` | 28 px | hero, film, benchmark lab |
| pill | 999 px | buttons and filter chips |

`corner-shape: squircle` is progressive enhancement only. Chart bars keep a flat
zero edge and a small 4 px value edge so length stays readable.

## Background

- Base: near-black `#06070a` with three radial-gradient blooms.
- Motion: only `transform` (`translate3d` + scale); no animated blur or
  background-position.
- Texture: static `site/assets/grain.svg` (3 octaves, 180 px tile), opacity ~16%,
  `mix-blend-mode: soft-light`.
- `prefers-reduced-motion` freezes the blooms, preview tilt and trend-line draw.
- Text always sits on near-solid surfaces, never directly on the moving bloom.

## Smart downloads

- The picker detects Windows, macOS or Linux from `userAgentData` / `platform`
  and offers the native artifact. iPadOS (a touch-capable `MacIntel`) and Linux
  ARM are deliberately not mapped to the x64-only Linux packages.
- Windows recommends the single EXE; macOS the universal DMG; Linux the
  AppImage. Every platform row remains available below.
- No new request: the picker only rewrites local links.
- macOS ZIP and Linux `tar.gz` have their own hydrated size/hash slots. The
  static fallbacks match the v0.4.6 release digests, not the older files that may
  sit in a developer's `site/downloads` folder.

## Benchmark lab

- Tabs: Memory / Startup / CLI.
- Three releases share one harness: 0.3.7, 0.4.5, 0.4.6.
- Zero-baseline horizontal bars, direct values, one shared scale per metric.
- Memory trend SVG + data table fallback.
- Delta colour is reserved for a real win; flat metrics use muted text and say
  "within run-to-run noise".
- Memory: 86.6 → 19.4 → 18.9 MB (−67.7 MB, −78%).
- Startup: 285.3 → 288.8 → 296.8 ms (flat/noise).
- CLI: 62.5 → 63.0 → 63.1 ms (flat/noise).

## Performance rules

- Hero screenshot is the only preloaded image; previews are lazy.
- Film has `preload="none"`. Its 147,409 B poster is not requested until the
  film is within 300 px; a 520×944 Chromium check saw it absent at the top and
  present after scroll. The play control links to the MP4 before JavaScript.
- Sections paint immediately; there is no reveal observer or `js` marker.
- `content-visibility: auto` on below-fold sections.
- No third-party embeds; no ad network. Sponsorship is a hand-reviewed link.
- Preview cards use a small perspective tilt on fine pointers only; the
  benchmark trend line draws once. Both are transform/opacity work.
- Static site can deploy at a project subpath or domain root.

## Links

- [[Kaelis Benchmark Protocol]]
- [[Kaelis Build And Release]]
- [[Kaelis Brain]]

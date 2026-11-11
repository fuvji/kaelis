---
type: reference
summary: Kaelis product page layout, vault graph, preview dialog, motion and benchmark lab.
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

## Hero viewport

- The Overview capture is framed as a desktop window: a 38 px rail, the Kaelis
  mark, a `Desktop` label and a teal `Native · local` state. It keeps the same
  1080×700 image and adds no request.
- The capture is the first of seven lightbox triggers. It opens from the image
  itself, reuses the already-loaded overview and returns focus to that image.

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
- Vault graph: one inline 1,600×1,000 SVG with 24 nodes, 47 edges, three
  labels, five halos and three highlighted traces. It is `position: fixed`,
  behind the content, `mix-blend-mode: screen`, and makes no extra request.
- Motion: blooms and the graph use `transform`; the graph drift is a 72 s,
  8 px total move. The highlighted traces are static. No animated dash offset,
  animated blur, filter or background-position.
- Texture: static `site/assets/grain.svg` (3 octaves, 180 px tile), opacity ~16%,
  `mix-blend-mode: soft-light`.
- The hero enters once with a short transform/opacity sequence; there is no
  scroll-reveal observer.
- Staged graph entrance is one first-load opacity sequence; reduced motion
  freezes the blooms, graph, hero entrance, preview scale, benchmark value fade
  and trend-line draw.
- Graph labels hide below 700 px; opacity drops so the page stays quiet. The
  radial mask fades the graph away from the copy side.
- Text always sits on near-solid surfaces, never directly on the moving bloom.
  Metadata uses `--dim: #808996`, lifted from `#6e7580` for AA contrast.

## Smart downloads

- The picker detects Windows, macOS or Linux from `userAgentData` / `platform`
  and offers the native artifact. iPadOS (a touch-capable `MacIntel`) and Linux
  ARM are deliberately not mapped to the x64-only Linux packages.
- Windows recommends the portable EXE with a secondary Setup link; macOS the
  universal DMG; Linux the AppImage. Every platform row remains available below.
- The downloads header also carries a platform-independent `Download Windows
  setup` link, so the installer remains discoverable if detection selects macOS
  or Linux.
- No new request: the picker only rewrites local links. The selected platform
  tints the recommendation control blue, violet or mint through one CSS
  variable; artifact URLs and labels do not depend on the tint.
- The artifact tile shows `EXE`, `DMG` or `APPIMAGE` and follows the same
  platform accent. Without JavaScript it stays on the static Windows fallback.
- Windows setup, macOS ZIP and Linux `tar.gz` have their own hydrated size/hash
  slots. The static fallbacks match the v0.4.6 release digests, not the older
  files that may sit in a developer's `site/downloads` folder.
- Each checksum row has a local Copy control with a transient Copied state.

## Preview viewer

- Six 1080×700 gallery cards plus the hero Overview are ordinary image links;
  JavaScript upgrades all seven to one native `<dialog>` capped at 1,180 px.
- The dialog is named by its changing title and described by its one-line meta;
  initial focus lands on that title so the image context is announced first.
- Left/right arrows, Escape, the close control and a pointer-safe backdrop click
  dismiss it. Focus returns to the originating card and body scroll unlocks
  immediately.
- `scrollbar-gutter: stable` on the root keeps the modal-open scrollbar change
  from shifting the page; `overscroll-behavior: contain` limits scroll chaining.
- The dialog reuses the preview image already on the page; it does not preload a
  second copy. The count is a polite live region.
- Hover and keyboard focus raise an `Expand` control. Reduced motion removes the
  image scale but keeps the affordance.

## Benchmark lab

- Tabs: Memory / Startup / CLI.
- Three releases share one harness: 0.3.7, 0.4.5, 0.4.6.
- Zero-baseline horizontal bars, direct values, one shared scale per metric.
- Values fade/translate once when the metric changes; reduced motion prints them
  directly.
- Memory trend SVG + data table fallback.
- Delta colour is reserved for a real win; flat metrics use muted text and say
  "within run-to-run noise".
- Memory: 86.6 → 19.4 → 18.9 MB (−67.7 MB, −78%).
- Startup: 285.3 → 288.8 → 296.8 ms (flat/noise).
- CLI: 62.5 → 63.0 → 63.1 ms (flat/noise).

## Surfaces

- Core panels share a 1 px inset top highlight and a deep, low-opacity shadow.
  There is no continuous shadow animation.
- Section labels carry a short gradient rule. The benchmark scale has a static
  tick ruler for scale only; it is not a second dataset.

## Performance rules

- Hero screenshot is the only preloaded image; previews are lazy.
- Film has `preload="none"`. Its 147,409 B poster is not requested until the
  film is within 300 px; a 520×944 Chromium check saw it absent at the top and
  present after scroll. The play control links to the MP4 before JavaScript.
- Sections paint immediately; there is no reveal observer or `js` marker. The
  hero entrance is a one-shot CSS animation.
- `content-visibility: auto` on below-fold sections.
- No third-party embeds; no ad network. Sponsorship is a hand-reviewed link.
- The graph is one inline SVG with no filter; its only ambient motion is a
  slow group transform. Preview cards use a small perspective tilt on fine
  pointers; the benchmark trend line draws once.
- Mobile section navigation is a native `<details>` menu: no custom focus trap,
  Escape closes it, and section links close it before navigating.
- The preview dialog reuses the existing image and clears its `src` on close, so
  opening it does not create another asset request.
- The sticky-header progress hairline is a CSS scroll-driven animation, not a
  scroll listener. It is absent when reduced motion is requested or the
  timeline API is unavailable.
- Static site can deploy at a project subpath or domain root.

## Links

- [[Kaelis Benchmark Protocol]]
- [[Kaelis Build And Release]]
- [[Kaelis Brain]]

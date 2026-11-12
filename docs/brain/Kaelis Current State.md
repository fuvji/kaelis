---
type: reference
summary: Current Kaelis release, repo, site, dependency and open-item snapshot.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-12
title: Kaelis Current State
tags:
  - kaelis
  - state
status: active
updated: 2026-11-12
---

# Kaelis Current State

> [!info] Snapshot
> Latest release **v0.4.7**; product page live; workspace version **0.4.7**.
> 0.4.7 makes Forge and NeoForge install themselves instead of ending every
> Play with “needs its own installer”. The setup, vault-graph, hero-frame and
> preview-dialog pass is live. Windows Setup now has always-visible header and
> hero actions plus a dedicated row.

## Repo

- Branch `main`.
- Site commit: `2806924`; Pages run 43272613159 green.
- Latest tag: `v0.4.7` (2026-11-12).
- Repo graph baseline: `f6da23c`; see [[Kaelis Repo Graph]].
- Windows release asset: `Kaelis.exe`, 16,586,752 B.
- SHA-256: `c3fcd6a90650be697e52807245c68c5366e650d4fc3a31c2d5de20f83c15f445`.
- Windows setup asset: `Kaelis-Setup.exe`, 5,913,793 B.
- Setup SHA-256: `8ed07cd48ab2c4f01a416a91dcc58cf1315974fdebee675a4bbd2fc060367aea`.
- macOS DMG: 17,138,961 B, `421d4cef0654983066a4508f9b95dae1d9bf5ae61e517e7643cad4f412fff059`.
- macOS ZIP: 15,077,271 B, `e9c68fc0579208e895a7da05f9a3ef8e23ccdd890583b0766d29613d386afc95`.
- Linux: `.deb` 8,158,460 B, AppImage 10,848,760 B, tar.gz 10,786,202 B.
- `site/downloads/SHA256SUMS.txt` and the page fallbacks are synced to v0.4.7;
  the release assets remain authoritative.
- Release: https://github.com/fuvji/kaelis/releases/tag/v0.4.7

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
- Seven artifacts have size/hash slots; static fallbacks match the v0.4.7
  release digests, not older local copies.
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
- Pages deploy 43272613159 green. Live page is 200 with the header, hero,
  picker and row Setup links. Local Chromium checks at 320, 390 and 1,440 px, plus the live
  page at 520 px, show no horizontal overflow. Direct binary on Pages is 404 by
  design.

## Loaders

- Fabric and Quilt install from their meta profiles (`loader.rs`).
- Forge and NeoForge install through their own official installers
  (`forge.rs`): build resolved from Forge’s promotion table or Maven metadata,
  installer SHA-1 verified against the Maven `.sha1`, then
  `java -jar <installer> --installClient <instance root>` with a minimal
  `launcher_profiles.json` when the instance has none.
- The generated `versions/<profile>/<profile>.json` supplies the main class and
  JVM and game arguments; `prepare_launch_plan` expands `${library_directory}`
  and `${classpath_separator}`, so Forge’s `-p` module path resolves. The merged
  classpath keeps the profile’s order (vanilla libraries, then the loader’s)
  instead of sorting, so Forge’s chosen ASM stays first.
- A profile counts as installed only when `inheritsFrom` matches the instance
  version, every profile library exists, and the patched client jar is present:
  `net/minecraftforge/forge/<full>/forge-<full>-client.jar` for Forge,
  `net/neoforged/neoforge/<version>/neoforge-<version>-client.jar` for modern
  NeoForge, or the 1.20.1 legacy
  `net/neoforged/forge/<full>/forge-<full>-client.jar`. The jar location, not
  the profile id, decides the family: NeoForge 1.20.1 reuses
  `1.20.1-forge-<build>` and the same `net.neoforged:forge` coordinate.
  Anything less is repaired before launch.
- This host executed the real installers with a scratch Temurin 17: Forge
  1.20.1-47.4.0 and NeoForge 1.20.1-47.1.106 both booted through ModLauncher
  and reached Minecraft’s title-screen render path; NeoForge’s legacy profile
  and jar were confirmed. The scratch had only the asset index, so texture
  loading failed there. That was a launch-plan mimic, not a Kaelis build; CI
  still compiles and tests.

## Dependencies

- 0.4.7 picked up the last compatible patch, `errno` 0.3.15. `webbrowser`
  1.2.7 is already current; 22 newer versions sit outside the manifests'
  current ranges and remain major-version Dependabot work.
- `kaelis-api` now asks `zip` for Deflated content only. `cargo tree` confirms
  `lzma-sys`, `bzip2-sys` and `zstd-sys` are out of its graph; `ring` remains
  only through rustls. No binary-size number is claimed without a build.
- Workflows use current supported majors: `checkout@v6`,
  `upload-pages-artifact@v4`, `softprops/action-gh-release@v3`; artifact
  upload/download remain v4, `configure-pages` v5, `deploy-pages` v4.
- Dependabot checks Cargo and GitHub Actions weekly in
  `.github/dependabot.yml`; minor and patch Cargo updates are grouped.
- Local Rust linking is still blocked: `dlltool` can be bridged to the bundled
  `rust-lld`, but `ring`’s C build needs a C toolchain this host lacks. CI is
  the compile and test gate.
- Current Rust: 1.99.1; minimum supported: 1.85.

## Open

- Brain sync to the user's Obsidian vault (`01-Projects/Kaelis/Brain`) is
  pending for this pass; the vault path was not provided.
- v0.4.7 is published. CI run 43245768021 is green on Windows, Ubuntu and macOS;
  the release checksums and the live Pages checksum file match.
- The setup, vault-graph, hero-frame and preview-dialog pass is live; a
  full-page visual review beyond the measured layout checks remains.
- Dependabot opened #1–#6 for major dependency updates; review them instead of
  merging by default.
- The Forge/NeoForge installers were exercised against real scratch installs;
  CI compiles and tests the pass. Re-test a user-created Forge instance on
  0.4.7 before closing the loader report.
- Plan the first measured post-0.4.7 optimization.

## Links

- [[Kaelis Brain]]
- [[Kaelis Build And Release]]
- [[Kaelis Roadmap]]

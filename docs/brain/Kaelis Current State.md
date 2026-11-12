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
> Latest release **v0.4.10** (2026-11-12); product page live; workspace version
> **0.4.10**. This is a visual and dead-weight cleanup: the Instances
> `RUNNING` badge and root-path row are gone, the startup label is clearer,
> and unreferenced source captures were removed. It makes no speed claim; the
> Windows binary is one 4 KiB page larger than 0.4.9.

## Repo

- Branch `main`; site commit `408263e`.
- Latest tag: `v0.4.10` at `f6f60be` (2026-11-12).
- Release run 43301510867 green: Windows, macOS universal, Linux and Publish.
- CI run 43298311378 green on Windows, Ubuntu, macOS and site assets; the
  site-only follow-up also passed its available jobs before deployment.
- `v0.4.8` is a historical tag only: its release run failed before publishing,
  and the tag must not be moved.
- Release: https://github.com/fuvji/kaelis/releases/tag/v0.4.10
- Windows release asset: `Kaelis.exe`, 17,154,048 B.
- SHA-256: `a281ce87f71672201595362d9fe5555ac53e0bfeea1b5f177867da411d5c40df`.
- Windows setup asset: `Kaelis-Setup.exe`, 6,023,423 B.
- Setup SHA-256: `4d0f2b04a517ddac117eb98af3fa73183da039e3e1b1fddc33d16c952a00a5c6`.
- macOS DMG: 17,571,949 B,
  `5ff9a86c8880aa22115cf6ebbd9cde88dab6d202adb2c91f1db197374eabd8d7`.
- macOS ZIP: 15,455,941 B,
  `0c5756e6c1284397b1c7e829bee41134d215ff69b870e1f2d3ec093242c8250a`.
- Linux `.deb` 8,438,386 B,
  `41fa1ebc183b89b787b8faf5aa3f32fc42ce915e9b60775f8c5372cd8ecd2033`;
  AppImage 11,151,864 B,
  `608021f438c5f577fc5fb5546c3d1bd49adcc5b445badd28705ebf7ca6341237`;
  `tar.gz` 11,131,387 B,
  `e7aa8bb34f2df1af9ef8ca779e1cc458e7aa6d7b21d2b99f257d8d96f921cb1e`.
- `site/downloads/SHA256SUMS.txt` and the page fallbacks are synced to v0.4.10;
  all seven downloaded binaries verified against the release checksum sheet.
- Removed the unreferenced root `screenshots/` and `media/` copies; `site/`
  keeps the optimized images and film it actually serves. The Instances card
  also lost its unused root-path string.

## Site

- https://fuvji.github.io/kaelis/
- Pages run 43306427741 green from site commit `408263e`. The first two uploads
  succeeded but `deploy-pages` saw no artifact; a fresh dispatch fixed it.
- Live page returns 200 with `softwareVersion` and the footer at **v0.4.10**; the
  EXE slot carries `a281ce87…`.
- Live `downloads/SHA256SUMS.txt` is byte-identical to the release copy;
  `downloads/Kaelis.exe` is 404 by design.
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
- Seven artifacts have size/hash slots; static fallbacks match the v0.4.10
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
- Local Chromium checks at 320, 390 and 1,440 px, plus the live page at 520 px,
  showed no horizontal overflow before this release; the 0.4.10 site change only
  replaced version and checksum text.

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
- Repair is explicitly ordered: restore game files and version metadata, install
  the JVM that metadata names, then install the loader. This is what makes an
  empty no-Java Forge root work; it used to fail on a missing
  `versions/1.20.1/1.20.1.json`.
- A fast regression asserts the stage order. The ignored real-network test
  installs Java, vanilla 1.20.1 and Forge on a clean root, then requires
  `health::check` to be ready. It passed in 169 s, and a 0.4.8 CLI run on a
  fresh root installed `java-runtime-gamma/17.0.15` and Forge 1.20.1-47.4.28
  and reached the game process. The repair code is unchanged in 0.4.9.
- This host previously executed the real installers with a scratch Temurin 17:
  Forge 1.20.1-47.4.0 and NeoForge 1.20.1-47.1.106 both booted through
  ModLauncher and reached Minecraft’s title-screen render path; NeoForge’s
  legacy profile and jar were confirmed.

## Dependencies

- CI and release pin Rust **1.89.0**. `uuid` 1.29.2 requires 1.89, while
  `objc2` 0.4.1 (through `winit` 0.29) still builds on 1.89 and 1.90 but fails
  on the current stable 1.99. The workspace minimum remains 1.85.
- 0.4.10 took the compatible `rustls-webpki` 0.103.16 → 0.103.17 patch.
  0.4.7 picked up `errno` 0.3.15; `webbrowser` 1.2.7 is already current. Newer
  major versions remain Dependabot work.
- `kaelis-api` asks `zip` for Deflated content only. `cargo tree` confirms
  `lzma-sys`, `bzip2-sys` and `zstd-sys` are out of its graph; `ring` remains
  only through rustls.
- Workflows use `checkout@v6`, `upload-pages-artifact@v4`,
  `softprops/action-gh-release@v3`; artifact upload/download remain v4,
  `configure-pages` v5, `deploy-pages` v4.
- Dependabot checks Cargo and GitHub Actions weekly in
  `.github/dependabot.yml`; minor and patch Cargo updates are grouped.
- Local tests and clippy passed with the bundled Winlibs PATH; CI remains the
  release compile and test gate.

## Open

- Brain sync to the user's Obsidian vault (`01-Projects/Kaelis/Brain`) is
  pending; no vault path or `KAELIS_VAULT` is set.
- v0.4.10 is published and its checksum, Pages hydration and live links are
  verified.
- Review Dependabot major updates instead of merging them by default. Moving
  past Rust 1.89 requires upgrading `winit`/`eframe` away from `objc2` 0.4.1.
- The clean-root no-Java Forge flow is covered by the stage-order test and the
  ignored network test; re-run the ignored test against any future loader
  change.
- No post-0.4.10 benchmark was run; the 0.4.6 numbers remain the reference.
  The Windows EXE grew one 4 KiB page, so do not claim a binary-size or speed
  win for this cleanup. Root capture removal reduces the checked-out tree only;
  old blobs remain in Git history.
- Plan the first measured post-0.4.10 optimization; do not zero safety, retry
  or process-wait timers just to make a latency claim.

## Links

- [[Kaelis Brain]]
- [[Kaelis Build And Release]]
- [[Kaelis Roadmap]]

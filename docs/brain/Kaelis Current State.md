---
type: reference
summary: Current Kaelis release, repo, site, dependency and open-item snapshot.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-15
title: Kaelis Current State
tags:
  - kaelis
  - state
status: active
updated: 2026-11-15
---

# Kaelis Current State

> [!info] Snapshot
> Latest release **v0.4.12** (2026-11-15); product page live; workspace version
> **0.4.12**. Windows 11's compositor rounds the borderless launcher again: the
> published window reports `corner_preference=2` and a desktop capture shows the
> clipped corners. This is a visual restoration only; the 0.4.11 tray minimize
> and rustls-only client remain, with no new performance or size claim.

## Repo

- Branch `main`; release commit `8d24eda`; the site fallback sync follows it.
- Latest tag: `v0.4.12` at `8d24eda` (2026-11-15).
- Release run 43704414850 green: Windows, macOS universal, Linux and Publish.
- CI run 43703348291 green on Windows, Ubuntu, macOS and site assets.
- `v0.4.8` is a historical tag only: its release run failed before publishing,
  and no published tag may be moved.
- Release: https://github.com/fuvji/kaelis/releases/tag/v0.4.12
- Windows release asset: `Kaelis.exe`, 17,088,000 B, unchanged from 0.4.11.
- SHA-256: `cc78b769ee3f3552754fe968f9a6fb10a99108f8ae7c9046613dfa85e92abe81`.
- Windows setup asset: `Kaelis-Setup.exe`, 6,001,676 B.
- Setup SHA-256: `44e394049a8c5af118e9e4a5f1c72fc4a6da858dbdbc4bc2bbab75f5bc00b256`.
- macOS DMG: 17,522,794 B,
  `c7a1c33aeb53f4b59b52fe8189166cc133d237e5b0ec60cd25d301c2435c7e70`.
- macOS ZIP: 15,412,012 B,
  `ddcb3e91f110f4f57d2a6db151bce1edc29ded793141fc392ef3b9805dd32ea9`.
- Linux `.deb` 8,413,862 B,
  `6b129ba6b83f16e9a796450c91e8993877b25e6d0db7b18fd1484085e08b3a72`;
  AppImage 11,094,520 B,
  `0bd6cfc9efab9eb46442c9cf8d496e58a5ea110d151356733f365696a2bc3ce2`;
  `tar.gz` 11,099,849 B,
  `b3e34acc58cc50358ce1ee35a52612bedb91ad5c8c5fc4272683282dbe92354c`.
- `site/downloads/SHA256SUMS.txt` and all seven page hash slots are synced to
  v0.4.12; every downloaded asset passed `sha256sum -c`.
- Rounded corners are restored through `DWMWCP_ROUND` (2), while the border
  stays `DWMWA_COLOR_NONE` and the visible frame request stays zero. The published
  1080×700 window reports `corner_preference=2`; a desktop capture at
  `C:\temp\kaelis-ui\corners-0412.png` shows all four clipped corners. Windows
  10 ignores the attribute and keeps square corners.
- The header minimize control calls `hide_to_tray` when a tray exists and falls
  back to ordinary taskbar minimize when it does not. Non-Windows keeps the
  native minimized state. CI compiled the path, but this host blocked synthetic
  native cursor injection, so the control was not clicked in the published
  build; the existing tray restore path is the tested fallback.
- reqwest now builds one TLS stack: `default-features=false` with
  `json,rustls-tls,stream,http2,charset,macos-system-configuration` in the API,
  GUI and launcher crates. `native-tls`, OpenSSL and `schannel` left the graph.
  `socket2` moved 0.4.11 → 0.4.10 because 0.4.11 is yanked.
- 0.4.11 shipped the tray minimize and rustls-only client; its local GNU build
  measured 17,637,888 B, but CI remains the release gate. 0.4.12 changes only
  the DWM corner preference, so the published EXE size is unchanged.
- 0.4.12 was checked visually on the published EXE. No new startup, memory or
  idle-CPU benchmark was run; 0.4.11's `tools/bench.ps1` figures remain the
  latest comparison.

## Site

- https://fuvji.github.io/kaelis/
- Pages run 43705936328 green from release commit `8d24eda`; the release
  Publish job dispatched it after the 0.4.12 assets were uploaded.
- Live page returns 200 with `softwareVersion` and the footer at **v0.4.12**; the
  EXE slot carries `cc78b769…`.
- Live `downloads/SHA256SUMS.txt` is byte-identical to the release copy;
  `downloads/Kaelis.exe` is absent by design. `releases/latest` resolves v0.4.12.
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
- Seven artifacts have size/hash slots; static fallbacks match the v0.4.12
  release digests, not older local copies. The EXE fallback is 16.3 MB and the
  DMG fallback is 16.7 MB.
- The Windows native border and visible frame are cleared through DWM
  attributes, while the Windows 11 rounded-corner preference is requested; the
  first clear is dark, so the undecorated window does not flash a pale edge.
  The published 0.4.12 window reports preference 2 and the desktop capture
  confirms the rounded clip. Windows 10 keeps square corners.
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
  showed no horizontal overflow before this release; the 0.4.12 site change only
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
- 0.4.11 removed `native-tls`, OpenSSL and `schannel` by using reqwest with
  rustls only; HTTP/2, charset handling and macOS system-proxy support remain.
  `socket2` is pinned at 0.4.10 because 0.4.11 is yanked. 0.4.10 took the
  compatible `rustls-webpki` 0.103.16 → 0.103.17 patch. `webbrowser` 1.2.7 is
  current; newer major versions remain Dependabot work.
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
  pending. `KAELIS_VAULT` is unset, and the `obsidian-vault` skill path
  `/mnt/d/Obsidian Vault/AI Research` does not exist on this host.
- `/loops` is absent and no Node/npx is installed, so the find-skills CLI could
  not run; the available `find-skills` and `obsidian-vault` skills were read.
- v0.4.12 is published; all seven assets, the checksum sheet, Pages hydration,
  live version `0.4.12`, rounded-window preference and the 404 for the Pages
  binary path are verified.
- Review Dependabot major updates instead of merging them by default. Moving
  past Rust 1.89 requires upgrading `winit`/`eframe` away from `objc2` 0.4.1.
- The clean-root no-Java Forge flow is covered by the stage-order test and the
  ignored network test; re-run the ignored test against any future loader
  change.
- **No startup, memory or idle-CPU win in 0.4.11.** `tools/bench.ps1` medians:
  67 ms CLI, 733 ms empty, 705 ms heavy, 31.9/32.0 MB working set,
  90.6/91.0 MB private, 0.00% idle. 0.4.10 measured 62 ms, 549 ms, 424 ms,
  90.6/91.8 MB private. The 0.4.11 startup is slower in this run, but one empty
  sample was 1,299 ms and earlier runs varied enough that this is not proof of a
  code regression. The real 0.4.11 win is binary size: −66,048 B.
- **0.4.12 is visual-only and has no benchmark.** The EXE remains 17,088,000 B;
  no startup, memory or CPU behavior changed. The rounded-corner check is a DWM
  attribute read plus a desktop capture, not a performance claim.
- Plan an interleaved, same-session 0.4.10/0.4.11 startup audit before making a
  regression or speed claim. Do not zero safety, retry, timeout or process-wait
  timers to manufacture a latency result.

## Links

- [[Kaelis Brain]]
- [[Kaelis Build And Release]]
- [[Kaelis Roadmap]]

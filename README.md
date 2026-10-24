# Kaelis - Fastest & Lightest Minecraft Launcher
<img width="1080" height="700" alt="accounts" src="https://github.com/user-attachments/assets/48041394-24b6-4a1c-8885-ad04ca1db913" />

**Minecraft, composed.** A native Rust launcher for Windows, macOS, and Linux
with one calm library for instances, modpacks, mods, resource packs, and
shaders.

[Website](site/index.html) · [Releases](https://github.com/fuvji/kaelis/releases) · [Issues](https://github.com/fuvji/kaelis/issues) · [Community](https://discord.gg/fvrdYzrQjq)

## Performance

Measured on the shipped Windows build with `tools/bench.ps1` (3 runs, warm disk
cache, window detected through UI Automation) on 2026-10-24:

| Metric | Result |
| --- | --- |
| `kaelis --version` | 70 ms median, 89 ms p95 |
| Window, empty data root | 162 ms median |
| Window, 200-jar instance | 160 ms median |
| Working set (idle) | 80.0 MB empty, 80.6 MB with 200 jars |
| Private bytes (idle) | 90 MB |
| Idle CPU | 0.00% (event-driven, no repaint loop) |
| Handles | 553 |
| Release binary | 17,872,896 B |

The window does not wait for mod metadata: jar scanning and artwork decoding
run on workers, so a 200-mod instance opens in the same time as an empty one.
Startup no longer walks the temp folder before the window: the background
sweeper's first tick does it on the runtime. Pack downloads use 8 parallel
transfers, and an idle launcher schedules no frames at all. The binary carries
both renderers, so a session without OpenGL 2.0 still opens a window through
Direct3D 12; that fallback is the largest part of the size above. Run the same benchmark after a change with
`powershell -NoProfile -ExecutionPolicy Bypass -File tools/bench.ps1`; it writes
`target/bench-report.json`. These are single-machine numbers, not a ranking
against other launchers, and the absolute window time moves with the host: the
same machine recorded 362 ms for 0.1.3 on a different day without any change to
the window path, so a number that looks slower here is the machine, not the
launcher. The working set and handle count are the figures that stay stable.

## Quick start

Requirements: Rust 1.85 or newer and a C/C++ linker for the selected target.

| Platform | Toolchain | Extra packages |
| --- | --- | --- |
| Windows 10/11 | `stable-x86_64-pc-windows-msvc` | Visual Studio Build Tools with the C++ workload |
| macOS 11+ | `stable-aarch64-apple-darwin` or `stable-x86_64-apple-darwin` | Xcode command line tools |
| Linux | `stable-x86_64-unknown-linux-gnu` | `libgtk-3-dev libayatana-appindicator3-dev libxkbcommon-dev libwayland-dev libx11-dev` |

```bash
git clone https://github.com/fuvji/kaelis.git
cd kaelis
cargo build --release -p kaelis-launcher
```

Run `target/release/kaelis` (`kaelis.exe` on Windows), or launch a saved
instance directly:

```bash
kaelis --play fabric-1-21
```

Install it like a desktop app:

```bash
# macOS: Kaelis.app in dist/, ad-hoc signed and ready to drag to Applications
./packaging/macos/bundle.sh              # add --universal for Intel + Apple silicon

# Linux: per-user install, no root
./packaging/linux/install.sh
```

The Linux build needs `libgtk-3-dev` and `libayatana-appindicator3-dev` because
the tray icon talks to the desktop's app-indicator service; without a tray the
launcher still runs and reports it in the status line. Java is found in this
order: the instance's configured path, the runtime Kaelis provisioned, `PATH`,
then the standard JDK install folders (`/usr/lib/jvm`, `/Library/Java/…`,
`C:\Program Files\Java\…`), so a Dock- or menu-launched app that inherits no
shell environment still starts the game.

Portable instance archives work from the CLI too:

```bash
kaelis --duplicate fabric-1-21 --with-worlds
kaelis --export-instance fabric-1-21 ~/fabric-1-21.kaelis.zip
kaelis --import-instance ~/fabric-1-21.kaelis.zip
```
<img width="1080" height="700" alt="modpacks" src="https://github.com/user-attachments/assets/161668d6-006e-41b5-951f-d4c98abb049c" />

Both Modrinth and CurseForge are supported, and neither needs a key or account.
Type a CurseForge project name, paste its link, or give its project id and it
resolves straight to the project, then installs from the official CDN. Nothing
to configure, and nothing stored.

CurseForge's own catalogue search endpoint is key-only, so Kaelis never scrapes
it: the keyless path resolves exact projects instead of pretending to browse a
catalogue it cannot legally read. Modrinth remains the full-text catalogue. If
you hold an approved CurseForge key, exporting `CF_API_KEY` upgrades search to
the official API without changing anything else.

Sites without an API — SpigotMC, Planet Minecraft, 9Minecraft, GitHub release
pages, private packs — are covered by the link and local-file paths instead of
scrapers, so traffic stays predictable and within each site's terms.

## Accounts

Offline profiles need only a name. Microsoft sign-in needs no setup: **Account →
Sign in with Microsoft** shows a short code, copies it, opens the Microsoft page,
and finishes by itself once the page is approved. That is the public Xbox Live
client the Xbox app signs in with, so there is no Azure registration, no client
ID to paste, and no client secret anywhere in the app.

Every sign-in is exchanged through Xbox Live → XSTS → Minecraft Services, so an
account without Java Edition is refused instead of failing later at launch. The
refresh token lives in the OS credential store (Credential Manager, Keychain,
Secret Service) and restores the session on the next start without a code; the
Minecraft access token stays in zeroizing memory and is never written to disk.

## Keyboard

| Keys | Does |
| --- | --- |
| `Ctrl+1` … `Ctrl+5` | Jump to Overview, Instances, Content, Account, or Settings |
| `Tab` / `Shift+Tab` | Move focus; a focused navigation row draws a visible ring |
| `Ctrl+F` | Focus the search field on Instances or Content |
| `Ctrl+S` | Save the open instance editor |
| `Esc` | Close the open dialog, or clear the content search |
| `Ctrl+V` | Paste a copied picture as instance artwork, or text into the focused search field |

On macOS, use `Cmd` wherever `Ctrl` is listed; `Esc` and `Tab` are unchanged.

## Command line

Every operation the interface performs is also a headless command. Add `--json`
to a report and the output is one machine-readable document on stdout, with
diagnostics on stderr, so scripts and MCP servers can parse it without a GUI.

| Command | Does |
| --- | --- |
| `kaelis` | Opens the desktop launcher |
| `kaelis --version [--json]` | Build identity, platform, site and checksum URLs |
| `kaelis --status [--json]` | Data root, instance and account counts, active identity, resolved Java |
| `kaelis --list-instances [--json]` | Every instance: id, name, version, loader, mod count |
| `kaelis --list-versions [--show-alpha] [--show-beta] [--hide-snapshots] [--json]` | Newest Mojang versions |
| `kaelis --accounts [--json]` | Every saved identity, its kind, profile id, and the active one |
| `kaelis --use-account <name> [--json]` | Switches the saved identity the next launch uses |
| `kaelis --offline <name> [--json]` | Offline profile and its skin head URL |
| `kaelis --play <instance> [--offline <name>]` | Launches with the managed runtime and the saved offline identity |
| `kaelis --check-updates <instance> [--json]` | Newest compatible Modrinth version for every installed mod |
| `kaelis --update-mods <instance> [--json]` | Downloads, verifies, and installs every available mod update |
| `kaelis --install-mod <instance> <provider:project>` | Installs a mod and its required dependencies; a bare slug means Modrinth |
| `kaelis --repair <instance>` | Reinstalls missing required dependencies |
| `kaelis --import-modpack <instance> <pack file>` | Imports an `.mrpack` or `.zip` pack into an instance |
| `kaelis --export-instance <instance> <out.kaelis.zip>` | Writes one portable archive |
| `kaelis --import-instance <archive.kaelis.zip>` | Imports it under a free id |
| `kaelis --duplicate <instance> [--with-worlds] [--json]` | Copies an instance under a free id; worlds are opt-in |

```bash
kaelis --list-instances --json
kaelis --status --json
kaelis --accounts --json
kaelis --use-account Builder
kaelis --check-updates fabric-1-21 --json
kaelis --play fabric-1-21 --offline Steve
```

Exit codes are stable: `0` success, `2` bad arguments, other non-zero values are
runtime failures. A second GUI launch hands its arguments to the running window
instead of opening a duplicate.

## Workspace

```text
crates/
  kaelis-core/       brand, instance metadata, JVM flags, user automation
                     rules and their runner, junk cleaner, RAM-only log,
                     single-instance mutex, memory sweeper
  kaelis-api/        Mojang versions, Modrinth, CurseForge, downloads,
                     Fabric/Quilt provisioning, mrpack import, portable
                     instance archives, repair, hashing, auth, launch
  kaelis-gui/        egui interface, content studio, instance editor, tray,
                     brand mark (one geometry for window, tray, and site)
  kaelis-launcher/   CLI and desktop entry point
site/                static product site and brand assets
```
<img width="1080" height="700" alt="settings" src="https://github.com/user-attachments/assets/0c718cb2-0401-4c60-b75a-4a9dde368fab" />

Instances are plain JSON under the platform data directory:
`%APPDATA%\kaelis\Kaelis\data\instances` on Windows,
`~/Library/Application Support/gg.kaelis.Kaelis/instances` on macOS, and
`~/.local/share/kaelis/instances` on Linux. Set `KAELIS_DATA_DIR` to relocate
all launcher state; each data root also gets its own single-instance guard, so a
portable copy never blocks the installed one.

## Design rules

- Native first: every hot path stays in Rust; the UI is immediate-mode and
  paints only when needed — forced repaints run at 60 FPS only while an install
  or progress bar is on screen, and an idle launcher is fully event-driven.
- Bounded everything: image decodes, log lines, dependency walks, download
  concurrency, archive extraction, and retry loops all have hard limits.
- One pass per download: SHA-1 is computed from the stream while bytes are
  written, so a verified install never re-reads the file it just saved.
- Explicit automation: repair, duplicate resolution, the junk cleaner, and
  every user command are visible settings with conservative safety proofs,
  never silent mutations of user data.
- No hidden state: no cache daemon, no persistent event log, no telemetry endpoint.
- Small releases: fat LTO, one codegen unit, stripped symbols, abort on panic.

## Verification

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The release path also generates `dist/KAELIS-LINKS.txt`, publishes the same
bytes as [`site/downloads/KAELIS-LINKS.txt`](site/downloads/KAELIS-LINKS.txt),
and builds a fresh per-user Windows installer. The index lists every runtime endpoint,
download, installation path, and project link; `tools/links.ps1` regenerates it,
prunes legacy link files, and `-Verify` probes the static links. `tools/release.ps1`
runs it automatically after hashing the new artifacts.

## Website

The product site is a single dependency-free HTML document in
[`site/`](site/index.html). The Pages workflow at
[`.github/workflows/pages.yml`](.github/workflows/pages.yml) deploys that
directory on a push to `main` and on a published release; enable GitHub Pages →
GitHub Actions once in the repository settings. Because the EXE, ZIP, and Setup
are release assets rather than git objects, the workflow fetches them into
`site/downloads` and runs [`tools/hydrate-site.ps1`](tools/hydrate-site.ps1) so
the visible sizes and hashes always describe the files actually being served.
Brand artwork lives in [`site/assets/kaelis-mark.svg`](site/assets/kaelis-mark.svg); every raster icon and the social card is regenerated from the same geometry with `tools/make-brand-assets.ps1` (1024 px master, 16–256 px ICO, 192/512 PWA icons, 180 px touch icon, 1200x630 card). The mark is a plate-free two-plane dart (`#70a5ff` / `#2e70e8`), so the SVG, ICO, PWA icons, window icon, and site all share one transparent glyph. The film is rebuilt with `tools/build-film.ps1`: six painted stills, a slow push with editorial cuts, a scored 31.7-second 1080p60 MP4, a fresh poster, and captions generated from the same timing table; `tools/narrate.ps1` holds the narration lines and voice direction.

Preview it locally with the same MIME types the host sends:

```bash
powershell -NoProfile -File tools/serve-site.ps1 -Port 8088
```

The directory also carries `robots.txt`, `sitemap.xml` (page, screenshots, and
the intro video), `manifest.webmanifest`, and `.well-known/security.txt`. The CI
workflow fails when a `href`/`src` in the page does not resolve, when
`SHA256SUMS.txt` stops matching the binaries beside it, or when the manifest
references a missing icon.

Releases are built by [`.github/workflows/release.yml`](.github/workflows/release.yml):
a `v*` tag builds Windows, macOS (universal), and Linux, publishes one release
with every artifact, the generated `KAELIS-LINKS.txt`, and a combined
`SHA256SUMS.txt`. The Windows job installs NSIS, so `Kaelis-Setup.exe` is rebuilt
from the same fresh binary as the portable download rather than reusing an old
local setup. `tools/release.ps1` is the same Windows path used locally.

<img width="1240" height="430" alt="account-skin" src="https://github.com/user-attachments/assets/9d54e89e-e081-4bc1-a7d4-005c93bb83b6" />

## License

MIT · Kaelis Labs · [@epufk](https://github.com/fuvji)

Kaelis is not affiliated with Mojang Studios, Microsoft, Modrinth, or
CurseForge. Minecraft is a trademark of Mojang Studios.

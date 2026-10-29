# Developing Kaelis

Internal build and maintenance notes. Kaelis is proprietary software: this
document is not a license to reuse the source. See [LICENSE](LICENSE).

## Build

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
# macOS: Kaelis.app in dist/, ad-hoc signed, then a drag-to-Applications DMG
./packaging/macos/bundle.sh              # add --universal for Intel + Apple silicon
./packaging/macos/dmg.sh

# Linux: per-user install, or tarball + .deb + AppImage in dist/
./packaging/linux/install.sh
./packaging/linux/package.sh --no-build
```

The Linux build needs `libgtk-3-dev` and `libayatana-appindicator3-dev` because
the tray icon talks to the desktop's app-indicator service; without a tray the
launcher still runs and reports it in the status line. Java is found in this
order: the instance's configured path, the runtime Kaelis provisioned, `PATH`,
then the standard JDK install folders (`/usr/lib/jvm`, `/Library/Java/…`,
`C:\Program Files\Java\…`), so a Dock- or menu-launched app that inherits no

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

The workspace currently passes 244 tests (126 api, 49 core, 57 gui, 12
launcher), and Clippy is clean with warnings denied on Windows, macOS, and
Linux.

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
local setup. The macOS job produces both the ZIP and the DMG; the Linux job
smoke-tests the tarball, the `.deb`, and the AppImage before publishing. The
publish job dispatches the site deploy so the page can never serve the previous
release under a new version number. `tools/release.ps1` is the same Windows path
used locally.

<img width="1080" height="700" alt="Overview" src="site/assets/app-overview.png" />

<img width="1240" height="430" alt="account-skin" src="https://github.com/user-attachments/assets/9d54e89e-e081-4bc1-a7d4-005c93bb83b6" />


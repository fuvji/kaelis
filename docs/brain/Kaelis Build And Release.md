---
type: reference
summary: How to verify, version, tag, package, publish and sync Kaelis.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-14
title: Kaelis Build And Release
tags:
  - kaelis
  - release
  - build
status: active
updated: 2026-11-14
---

# Kaelis Build And Release

## Local checks

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p kaelis-launcher
```

Linux also needs `libgtk-3-dev`, `libayatana-appindicator3-dev`, `libxdo-dev`,
`libxkbcommon-dev`, `libwayland-dev`, `libx11-dev`.

> [!warning] This Windows host
> Local linking needs the Winlibs PATH at
> `/c/temp/kaelis-toolchain/mingw64/bin` and a `CARGO_TARGET_DIR` outside the
> space-containing project path. With both, the 0.4.11 GNU release build
> succeeded at 17,637,888 B. CI (Windows/macOS/Linux) remains the release
> compile and test gate; do not call a local build green when only `cargo fmt`
> ran.

## Version contract

- One source: `[workspace.package].version` in `Cargo.toml`.
- Packaging scripts read that root manifest.
- `README.md` must carry `## What's new in <version>`; the release job lifts that
  exact section into the GitHub release body.
- `CHANGELOG.md` carries the durable history.
- Current line: **0.4.x**. Current: **0.4.11**. Next bump only when launcher
  code changes.

## Release flow

1. Commit and push `main`; wait for CI.
2. `git tag v<version>` and push the tag.
3. Release workflow builds Windows, macOS universal and Linux; publishes:
   `Kaelis.exe`, `Kaelis-Setup.exe`, macOS ZIP/DMG, Linux tar/deb/AppImage,
   `KAELIS-LINKS.txt`, `SHA256SUMS.txt`. Windows setup is optional, per-user
   NSIS; the portable ZIP stays retired.
4. Publish job dispatches the Pages deploy. macOS uses `macos-26-intel`, the
   current Intel image, because it acquires a runner more reliably than the
   legacy `macos-15-intel` pool and still builds both Rust targets.
5. If `deploy-pages` reports “Found 0 artifact(s)” after a successful
   `upload-pages-artifact` step, re-run once; if it repeats, dispatch
   `pages.yml` again. A fresh run creates the missing artifact and succeeded
   for 0.4.10 after two failed attempts on the original run.
6. 0.4.11 published all nine release assets from run 43601731015. The Windows
   EXE is 17,088,000 B, 66,048 B below 0.4.10. Pages run 43702756319 deployed
   site commit `4f8130f`; the live page and checksum file carry 0.4.11.

## Toolchain pin

CI and release pin Rust **1.89.0** (`toolchain: 1.89.0` in both workflows).
`objc2` 0.4.1 is reached through `winit` 0.29 and stopped compiling on newer
stable; `uuid` 1.29 requires 1.89. Do not float to `stable` without upgrading
`winit`/`eframe`, and keep the Windows packaging `-Toolchain` argument on the
same version.
## Verify

- release `latest` resolves the new tag (v0.4.11 verified);
- downloaded `Kaelis.exe` and all six other artifacts match `SHA256SUMS.txt`;
- live page shows the version and hydrated hashes (0.4.11 verified);
- `/downloads/Kaelis.exe` is 404 on Pages (binaries live on Releases);
- no `.rs`, `.toml`, `.lock` or sourcemap is present in `site/` or assets.

## Pages

Static site lives in `site/`. The workflow fetches release binaries only to hash
them, then deletes the binaries before upload. `tools/hydrate-site.ps1` rewrites
sizes/hashes. `site/downloads/KAELIS-LINKS.txt` is generated and ignored; do not
force-add it. Keep the page dependency-free.

## Brain sync

```powershell
powershell -NoProfile -File tools/sync-brain.ps1 -VaultPath "<vault>" -Subfolder "01-Projects/Kaelis/Brain"
```

## Links

- [[Kaelis Benchmark Protocol]]
- [[Kaelis Web System]]
- [[Kaelis Current State]]

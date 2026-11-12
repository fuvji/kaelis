---
type: reference
summary: How to verify, version, tag, package, publish and sync Kaelis.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-12
title: Kaelis Build And Release
tags:
  - kaelis
  - release
  - build
status: active
updated: 2026-11-12
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
> Local Rust linking is blocked because `dlltool.exe` is absent from the
> installed GNU toolchain. CI (Windows/macOS/Linux) is the compile and test
> evidence. Do not call a local build green when only `cargo fmt` ran.

## Version contract

- One source: `[workspace.package].version` in `Cargo.toml`.
- Packaging scripts read that root manifest.
- `README.md` must carry `## What's new in <version>`; the release job lifts that
  exact section into the GitHub release body.
- `CHANGELOG.md` carries the durable history.
- Current line: **0.4.x**. Current: **0.4.9**. Next bump only when launcher
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

## Toolchain pin

CI and release pin Rust **1.89.0** (`toolchain: 1.89.0` in both workflows).
`objc2` 0.4.1 is reached through `winit` 0.29 and stopped compiling on newer
stable; `uuid` 1.29 requires 1.89. Do not float to `stable` without upgrading
`winit`/`eframe`, and keep the Windows packaging `-Toolchain` argument on the
same version.
5. Verify:
   - release `latest` resolves the new tag;
   - downloaded `Kaelis.exe` matches `SHA256SUMS.txt`;
   - live page shows the version and hydrated hashes;
   - `/downloads/Kaelis.exe` is 404 on Pages (binaries live on Releases);
   - no `.rs`, `.toml`, `.lock` or sourcemap is present in `site/` or assets.

## Pages

Static site lives in `site/`. The workflow fetches release binaries only to hash
them, then deletes the binaries before upload. `tools/hydrate-site.ps1` rewrites
sizes/hashes. Keep the page dependency-free.

## Brain sync

```powershell
powershell -NoProfile -File tools/sync-brain.ps1 -VaultPath "<vault>" -Subfolder "01-Projects/Kaelis/Brain"
```

## Links

- [[Kaelis Benchmark Protocol]]
- [[Kaelis Web System]]
- [[Kaelis Current State]]

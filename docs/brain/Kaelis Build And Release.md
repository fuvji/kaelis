---
type: reference
summary: How to verify, version, tag, package, publish and sync Kaelis.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-10
title: Kaelis Build And Release
tags:
  - kaelis
  - release
  - build
status: active
updated: 2026-11-10
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
- Current line: **0.4.x**. Next: **0.4.7**.

## Release flow

1. Commit and push `main`; wait for CI.
2. `git tag v<version>` and push the tag.
3. Release workflow builds Windows, macOS universal and Linux; publishes:
   `Kaelis.exe`, macOS ZIP/DMG, Linux tar/deb/AppImage, `KAELIS-LINKS.txt`,
   `SHA256SUMS.txt`. Windows has no installer or archive.
4. Publish job dispatches the Pages deploy.
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

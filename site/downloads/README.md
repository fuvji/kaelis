<div align="center">

<img src="site/assets/kaelis-mark.svg" width="88" alt="Kaelis mark">

# Kaelis

**Minecraft, composed.** A native Rust launcher with one calm library for
instances, mods, resource packs, shaders, and worlds.

[Website](site/index.html) · [Releases](https://github.com/epufk/kaelis/releases) · [Issues](https://github.com/epufk/kaelis/issues) · [Community](https://discord.gg/fvrdYzrQjq)

</div>

<p align="center">
  <img src="screenshots/overview.png" alt="Kaelis overview: active instance, content counts, account, and release status" width="960">
</p>

Kaelis is a local-first desktop launcher for Windows 10 and 11. It pairs a
Rust workspace with a compact egui interface: no bundled browser, no JavaScript
runtime, no telemetry, and no background analytics.

## What it does

- **One content studio.** Browse and install mods, resource packs, shader
  packs, and worlds from Modrinth and CurseForge without leaving the app.
- **Loader-aware installs.** Mod dependency graphs are resolved breadth-first
  with cycle detection, strict game-version/loader validation, and a bounded
  request budget.
- **Duplicate control.** After an install, the optional repair pass disables
  older jars that claim the same primary mod id, but only when every other
  provided id remains satisfied.
- **Instance workspace.** Per-instance Java runtime, heap sizing, JVM/game
  arguments, environment, window rules, quick join, artwork, notes, groups, and
  explicit pre-launch/post-exit hooks. Cards show a cached notes preview so
  each instance carries its own description at a glance.
- **Accounts that stay put.** Microsoft device-code sign-in with refresh tokens
  in the OS credential store, plus a clearly labelled offline profile. Profile
  heads and access tokens are held in memory only.
- **Managed runtime.** The launcher can provision a matching Java runtime for
  the selected Minecraft version instead of relying on a hand-built install.
- **Honest logging.** The instance log is a bounded in-memory ring buffer. It
  is discarded with the process and exported only when the user asks.

## Quick start

Requirements: Windows 10/11, Rust 1.85 or newer, and a C/C++ linker for the
selected Rust target.

```bash
git clone https://github.com/epufk/kaelis.git
cd kaelis
cargo build --release -p kaelis-launcher
```

Run `target/release/kaelis.exe`, or launch a saved instance directly:

```bash
kaelis --play fabric-1-21
```

Both Modrinth and CurseForge are supported. CurseForge searches require an API
key; add it in **Settings → Integrations**. It is stored in the operating-system
credential store, never in instance JSON or the repository.

## Workspace

```text
crates/
  kaelis-core/       brand, instance metadata, JVM flags, RAM-only log,
                     single-instance mutex, fileless memory sweeper
  kaelis-api/        Mojang versions, Modrinth, CurseForge, downloads,
                     mrpack import, repair, hashing, auth, launch pipeline
  kaelis-gui/        egui interface, content studio, instance editor, tray
  kaelis-launcher/   CLI and desktop entry point
site/                static product site and brand assets
```

Instances are plain JSON under the platform data directory
(`%APPDATA%\kaelis\Kaelis\data\instances` on Windows by default). Set
`KAELIS_DATA_DIR` to relocate all launcher state.

## Design rules

- Native first: every hot path stays in Rust; the UI is immediate-mode and
  paints only when needed.
- Bounded everything: image decodes, log lines, dependency walks, download
  concurrency, archive extraction, and retry loops all have hard limits.
- Explicit automation: repair and duplicate resolution are visible settings
  with conservative safety proofs, never silent mutations.
- No hidden state: no cache daemon, no persistent event log, no telemetry endpoint.
- Small releases: fat LTO, one codegen unit, stripped symbols, abort on panic.

## Verification

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Website

The product site is a single dependency-free HTML document in
[`site/`](site/index.html). The Pages workflow at
[`.github/workflows/pages.yml`](.github/workflows/pages.yml) deploys that
directory on a push to `main`; enable GitHub Pages → GitHub Actions once in the
repository settings. Brand artwork lives in
[`site/assets/kaelis-mark.svg`](site/assets/kaelis-mark.svg).

## License

MIT · Kaelis Labs · Founder [@epufk](https://github.com/epufk)

Kaelis is not affiliated with Mojang Studios, Microsoft, Modrinth, or
CurseForge. Minecraft is a trademark of Mojang Studios.

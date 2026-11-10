---
type: reference
summary: Kaelis Rust workspace, data flow, storage, safety boundaries and native UI geometry.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-10
title: Kaelis Architecture
tags:
  - kaelis
  - architecture
status: active
updated: 2026-11-10
---

# Kaelis Architecture

Kaelis is a native Rust workspace. The UI is immediate-mode egui; no web runtime
is embedded in the launcher.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `kaelis-core` | instance metadata, JVM flags, automation, junk cleaner, RAM-only logs, handoff, memory sweeper |
| `kaelis-api` | Mojang versions, Modrinth/CurseForge, downloads, Fabric/Quilt, mrpack, portable archives, repair, hashing, auth, launch |
| `kaelis-gui` | egui shell, content studio, editor, accounts, settings, tray, brand mark |
| `kaelis-launcher` | CLI and desktop entry point |

## Data flow

```
settings.json / instances/*.json
        │
        ├── kaelis-gui ──> user actions
        │
        └── kaelis-api ──> Mojang / Modrinth / CurseForge / CDN
                                  │
                                  └── verified files ──> content-addressed store
                                                          └── hard links per instance
```

Instances are plain JSON under the platform data root. `KAELIS_DATA_DIR`
relocates all state; each root has its own single-instance guard.

## Safety boundaries

- Downloads are hash-verified while streaming; a failed check never installs.
- Archive and image decodes are size-bounded.
- Junk cleanup removes only regenerable files and refuses a symlinked root.
- Microsoft refresh tokens use the OS keychain; access tokens stay in zeroising
  memory.
- No telemetry, no daemon, no persistent event log.
- A running game is registered with pid + creation time so a reopened launcher
  can adopt it without guessing.

## UI geometry

Native shape tokens live in `crates/kaelis-gui/src/lib.rs`:

- `CORNER_SM = 10`, `CORNER_MD = 13`, `CORNER_LG = 15`, `CORNER_XL = 16`.
- `CONTROL_RADIUS = 11`, `CONTROL_HEIGHT = 26`.
- No hard 90° panel corners; separators are flat surfaces and hairlines.

The web uses a separate 6/10/14/20/28/999 px scale. See [[Kaelis Web System]].

## Links

- [[Kaelis Build And Release]]
- [[Kaelis Decisions]]
- [[Kaelis Brain]]

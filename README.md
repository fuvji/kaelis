<p align="center">
  <img width="96" height="96" src="site/assets/kaelis-mark.svg" alt="Kaelis">
</p>

<h1 align="center">Kaelis</h1>

<p align="center"><b>The fastest, lightest Minecraft launcher.</b><br>
Native Rust for Windows, macOS, and Linux.</p>

<p align="center">
  <a href="https://fuvji.github.io/kaelis/">Website</a> ·
  <a href="https://github.com/fuvji/kaelis/releases">Releases</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="https://discord.gg/fvrdYzrQjq">Discord</a>
</p>

<p align="center">
  <img alt="Version" src="https://img.shields.io/badge/version-0.4.4-70a5ff">
  <img alt="Platforms" src="https://img.shields.io/badge/platforms-Windows%20%7C%20macOS%20%7C%20Linux-2e70e8">
  <img alt="Idle CPU" src="https://img.shields.io/badge/idle%20CPU-0.00%25-3fb950">
  <img alt="License" src="https://img.shields.io/badge/license-proprietary-8b949e">
</p>

<img width="1080" height="700" alt="Accounts tab: the active identity and every saved offline or Microsoft account" src="site/assets/app-accounts.png" />

Instances, mods, modpacks, resource packs, shaders, and accounts in one window.
Files stay local, nothing runs in the background, and the library is one screen
away.

## Features

| | |
| --- | --- |
| **Auto Erase Junk** | Logs, caches, and stale launcher state go at every start. Saves, mods, and configs never do. |
| **One copy of the game** | Verified files are hard-linked once, so a second instance costs MB, not 600 MB. |
| **Auto-repair** | A missing JVM, jar, library, asset, or loader is refilled before the game starts. |
| **Required dependencies** | Every install resolves and downloads its dependency graph, on both catalogues. |
| **Verified mod updates** | Newest compatible release per mod, installed only after its hash matches. |
| **Keyless catalogue** | All of Modrinth and CurseForge. No API key, no account. |
| **Managed Java** | The runtime the version names, installed on first play. |
| **Portable instances** | `.kaelis.zip` round trip, plus Prism/MultiMC, CurseForge, and `.mrpack` imports. |
| **High-priority games** | Every JVM is promoted at spawn and stays high for its whole life. |
| **Runs in parallel** | Per-instance guards let different instances run at once. |
| **Reopens, never doubles** | One process, one tray icon. A second launch restores the window in ~120 ms. |
| **Hibernates while you play** | Hides and empties its working set; one tray click brings it back. |
| **Taskbar-friendly** | Keeps its taskbar button and Alt+Tab entry; the tray's Hide is opt-in. |
| **Starts with Windows** | Optional per-user entry, tray-hidden, no service or scheduled task. |
| **Survives a restart** | A running game still shows as running after the launcher reopens. |
| **Remembers your setup** | Window size and every remembered switch reopen as they were left. |
| **Updates in one click** | SHA-256 verified against the release, backed up, rolled back on failure. |
| **Private by default** | No telemetry, no daemon; tokens live in the OS keychain. |

**Auto Erase Junk never guesses.** It removes only what Minecraft or Kaelis
regenerates on the next launch: logs, crash dumps, loader and download caches,
extracted natives, staging files, OS thumbnail noise, and another launcher's
leftovers. Saves, mods, configuration, resource packs, shaders, screenshots,
options, and every folder it does not recognise stay. A symlinked instance root
is refused rather than followed, and a hostile folder cannot slow startup past
50,000 inspected entries. The first pass runs off the UI thread and after every
game; `kaelis --clean [--json]` runs it headless.

**The same bytes are stored once.** A verified file is hard-linked into
`<data>/content` by SHA-1 before any later instance asks for it, so a second
instance of the same version links 443 MB of assets, 66 MB of libraries, the
client jar, and the 100 MB Java runtime instead of downloading them. Deleting an
instance never damages another, and unreferenced entries are pruned.

## Content

Both catalogues are keyless. Modrinth is the official API; CurseForge is read
through the public mirror of its v1 API, with the same paths, JSON, pagination,
and file metadata as the official endpoint. Files come from the official CDN
with hash and required-dependency checks. An approved key in `CF_API_KEY` aims
the same calls at the official endpoint. Sites without an API are covered by
link and local-file import, never scrapers.

## Install

| Platform | Downloads |
| --- | --- |
| **Windows 10 / 11 · x64** | [Portable ZIP](https://fuvji.github.io/kaelis/downloads/Kaelis-Portable.zip) · [Kaelis.exe](https://fuvji.github.io/kaelis/downloads/Kaelis.exe) · [Installer](https://fuvji.github.io/kaelis/downloads/Kaelis-Setup.exe) |
| **macOS 11+** | [Universal DMG](https://fuvji.github.io/kaelis/downloads/Kaelis-macos-universal.dmg) · [ZIP](https://fuvji.github.io/kaelis/downloads/Kaelis-macos-universal.zip) |
| **Linux · x64** | [.deb](https://fuvji.github.io/kaelis/downloads/Kaelis-linux-x64.deb) · [AppImage](https://fuvji.github.io/kaelis/downloads/Kaelis-linux-x64.AppImage) · [tar.gz](https://fuvji.github.io/kaelis/downloads/Kaelis-linux-x64.tar.gz) |

No admin rights, no runtime to install, no account to create. The Windows build
is unsigned, so SmartScreen may ask once; the macOS build is ad-hoc signed, so
the first launch is right-click → Open. Verify before running:

```bash
shasum -a 256 -c SHA256SUMS.txt     # certutil -hashfile Kaelis.exe SHA256
```

Every artifact is listed in [SHA256SUMS.txt](https://fuvji.github.io/kaelis/downloads/SHA256SUMS.txt)
and the [URL & install index](https://fuvji.github.io/kaelis/downloads/KAELIS-LINKS.txt).

## Performance

Measured on one machine on 2026-10-28, 0.3.7 against 0.4.0, with the same
fixtures and `tools/bench.ps1`.

| Metric | 0.3.7 | 0.4.0 |
| --- | --- | --- |
| `kaelis --version` | 79 ms median | 78 ms median |
| Window, empty data root | 313 ms | 311 ms |
| Window, 200-jar instance | 309 ms | 315 ms |
| Working set, idle | 80.8 MB | 31.9 MB |
| Handles | 562 | 560 |
| Idle CPU, window shown | 0.00% | 0.00% |
| Idle CPU, hidden 60 s | 5.13% of a core | 0.57%, 0 ms after start-up |
| Working set, hidden | 79.4 MB | 32.0 MB |

An idle launcher schedules no frames, and a hidden one is off the schedule
entirely. Window time moves with host load; working set, handles, and idle CPU
are the figures that stay put.

## Command line

Every screen has a headless equivalent. `--json` prints one machine-readable
document on stdout, so scripts and agents can parse it without a GUI.

| Command | Does |
| --- | --- |
| `kaelis` | Open the launcher |
| `kaelis --version [--json]` | Build identity, platform, site and checksum URLs |
| `kaelis --status [--json]` | Data root, instance and account counts, resolved Java |
| `kaelis --clean [--json]` | Erase junk, share legacy files, prune the cache |
| `kaelis --list-instances [--json]` | Every instance: id, name, version, loader, mods |
| `kaelis --list-versions [--show-alpha] [--show-beta] [--hide-snapshots]` | Newest Mojang versions |
| `kaelis --accounts [--json]` | Every saved identity and the active one |
| `kaelis --use-account <name>` | Switch the identity the next launch uses |
| `kaelis --offline <name> [--json]` | Offline profile and its skin head URL |
| `kaelis --play <instance> [--offline <name>]` | Launch with the managed runtime |
| `kaelis --check-updates <instance> [--json]` | Newest compatible mod version for every jar |
| `kaelis --update-mods <instance> [--json]` | Download, verify, and install those updates |
| `kaelis --install-mod <instance> <provider:project>` | Install a mod and its dependencies |
| `kaelis --repair <instance>` | Reinstall missing required dependencies |
| `kaelis --import-modpack <instance> <pack>` | Import an `.mrpack` or `.zip` pack |
| `kaelis --export-instance <instance> <out.kaelis.zip>` | Write one portable archive |
| `kaelis --import-instance <archive>` | Import it under a free id |
| `kaelis --duplicate <instance> [--with-worlds]` | Copy an instance; worlds are opt-in |

Exit codes are stable: `0` success, `2` bad arguments, anything else a runtime
failure. A second GUI launch hands its arguments to the running window.

## Accounts

Offline profiles need only a name. Microsoft sign-in needs no setup: it shows a
short code, opens the Microsoft page, and finishes when the page is approved —
no Azure registration, no client id, no secret. Every sign-in is exchanged
through Xbox Live → XSTS → Minecraft Services, so an account without Java
Edition is refused at sign-in rather than at launch. The refresh token lives in
the OS credential store; the access token stays in zeroising memory.

## Latest release · 0.4.4

- **Proprietary licence.** Free to download and use; redistribution and
  modification are no longer permitted. Earlier builds keep their terms.
- **The packages carry it.** The portable ZIP, installer, and portable guide all
  ship the Kaelis licence instead of the MIT text.
- **A product README.** One line per feature; full history in
  [CHANGELOG.md](CHANGELOG.md).
- **Compact site copy.** Shorter cards, FAQ answers, and metadata.

Full history in [CHANGELOG.md](CHANGELOG.md).

## Licence

Proprietary · © 2026 Kaelis Labs · [@epufk](https://github.com/fuvji)

Free to download and use. Not free to redistribute, modify, or repackage. See
[LICENSE](LICENSE).

Kaelis is not affiliated with Mojang Studios, Microsoft, Modrinth, or
CurseForge. Minecraft is a trademark of Mojang Studios.

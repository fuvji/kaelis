# Kaelis — portable

**Minecraft, composed.** A native Rust launcher for instances, modpacks, mods,
resource packs, and shaders.

## Run it

1. Keep `Kaelis.exe` in any folder you can write to (a USB stick works).
2. Double-click it. There is no installer, no runtime to add, and no first-run
   download of the launcher itself.

This download is the Windows x64 build. The same Rust workspace builds on
macOS 11+ and Linux (`./packaging/macos/bundle.sh`, `./packaging/linux/install.sh`).
The launcher stores instances, settings, and the managed Java runtime under
`%APPDATA%\kaelis\Kaelis\data`; set `KAELIS_DATA_DIR` to keep everything beside
the executable instead.

## What is inside

- One Content tab for Modrinth and CurseForge: mods, resource packs, shaders,
  and `.mrpack` modpacks, with `Install` and one-click `Erase`.
- `Add files…` imports jars and packs from the file manager in one batch, and
  `Folders` opens mods, resource packs, shaders, or saves directly.
- Modpacks become their own instances, including the Fabric or Quilt loader
  they ask for.
- Java is provisioned automatically the first time you press Play, matched to
  the Minecraft version.
- One account manager for every identity: saved Microsoft and offline
  accounts sit in one list, each with its skin head, and switch in a single
  click. Offline names wear the premium skin that owns them (Steve when the
  name is unclaimed); Microsoft accounts wear their own.
- `Updates` checks every installed mod against Modrinth in one pass, shows the
  file and version each update would replace, and installs them with SHA-1
  verification. Stable builds follow stable releases; disabled jars stay
  disabled. Only each jar's hash plus the loader and game version leave the
  machine.
- Several different instances can run at once. Each launch claims a
  per-instance OS guard, so the same instance never starts twice; the window
  slips into the tray until the last game closes.
- Instances carry per-instance heap, JVM arguments, window rules, quick join,
  artwork, and notes. `Duplicate` branches mods and settings under a free id
  (`Duplicate with worlds` adds saves); `Export instance…` writes a portable
  `.kaelis.zip`, and `Import archive…` restores one on another machine or
  operating system.

## Script it

Every screen has a headless equivalent. `--json` prints one document on stdout,
so a script or an agent can read it without a GUI:

```bash
kaelis --status --json
kaelis --list-instances --json
kaelis --accounts --json
kaelis --use-account Builder
kaelis --check-updates fabric-1-21 --json
kaelis --play fabric-1-21 --offline Steve
```

## Verify the download

```powershell
certutil -hashfile Kaelis.exe SHA256
```

Compare it with `SHA256SUMS.txt` beside the download. A copy that does not match
is not the file this page published.

## Links

- Every URL, endpoint, and installation path: <https://fuvji.github.io/kaelis/downloads/KAELIS-LINKS.txt>
- Source and releases: <https://github.com/fuvji/kaelis>
- Issues: <https://github.com/fuvji/kaelis/issues>
- Community: <https://discord.gg/fvrdYzrQjq>

MIT · Kaelis Labs · [@epufk](https://github.com/fuvji)

Kaelis is not affiliated with Mojang Studios, Microsoft, Modrinth, or
CurseForge. Minecraft is a trademark of Mojang Studios.

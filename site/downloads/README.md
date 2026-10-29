# Kaelis — portable

The fastest, lightest Minecraft launcher. Native Rust for Windows, macOS, and
Linux.

## Run it

1. Keep `Kaelis.exe` in any folder you can write to (a USB stick works).
2. Double-click it. No installer, no runtime, no first-run download of the
   launcher itself.

This download is the Windows x64 build. Instances, settings, and the managed Java
runtime live under `%APPDATA%\kaelis\Kaelis\data`; set `KAELIS_DATA_DIR` to keep
everything beside the executable instead.

## What is inside

- One Content tab for Modrinth and CurseForge: mods, resource packs, shaders, and
  `.mrpack` modpacks, with `Install` and one-click `Erase`.
- `Add files…` imports jars and packs in one batch; `Folders` opens mods, resource
  packs, shaders, or saves directly.
- Modpacks become their own instances, including the Fabric or Quilt loader they
  ask for.
- Java is provisioned automatically on first play, matched to the game version.
- One account manager: Microsoft and offline identities side by side, each with
  its skin head, one click to switch.
- `Updates` checks every installed mod in one pass and installs with SHA-1
  verification. Stable follows stable; disabled jars stay disabled.
- Different instances can run at once; each launch claims a per-instance OS guard.
- Per-instance heap, JVM arguments, window rules, quick join, artwork, and notes.
  `Duplicate` branches an instance, `Export instance…` writes one `.kaelis.zip`,
  and `Import archive…` restores it on another machine or operating system.

## Script it

Every screen has a headless equivalent. `--json` prints one document on stdout:

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
- Releases and issues: <https://github.com/fuvji/kaelis>
- Community: <https://discord.gg/fvrdYzrQjq>

Proprietary · © Kaelis Labs · [@epufk](https://github.com/fuvji)

Kaelis is not affiliated with Mojang Studios, Microsoft, Modrinth, or
CurseForge. Minecraft is a trademark of Mojang Studios.

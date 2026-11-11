# Kaelis — downloads

The fastest, lightest Minecraft launcher. Native Rust for Windows, macOS, and
Linux. Every artifact is the same launcher; pick the form that fits your OS.

## Windows 10 / 11 · x64

1. Put `Kaelis.exe` in a folder you can write to (a USB stick works).
2. Double-click it. No installer, no archive, no runtime, and no first-run
   download of the launcher itself.

The executable keeps instances, settings, and the managed Java runtime in the
platform data root shown by `kaelis --status --json`; set `KAELIS_DATA_DIR` to
keep everything beside the executable instead.

## macOS 11+ · Apple silicon and Intel

The DMG is universal and ad-hoc signed:

1. Open `Kaelis-macos-universal.dmg`.
2. Drag `Kaelis.app` to Applications.
3. On the first launch, right-click the app and choose **Open**. macOS may warn
   because the build has no commercial notarisation certificate.

`Kaelis-macos-universal.zip` contains the same `.app` for scripted or portable
installs. The data root is reported by `kaelis --status --json`.

## Linux · x64

### Debian / Ubuntu

```bash
sudo apt install ./Kaelis-linux-x64.deb
kaelis
```

The package includes the desktop entry. Uninstall it with the system package
manager when you no longer want it.

### AppImage

```bash
chmod +x Kaelis-linux-x64.AppImage
./Kaelis-linux-x64.AppImage
```

If the host has no FUSE, run it as
`./Kaelis-linux-x64.AppImage --appimage-extract-and-run`.

### Tarball

```bash
tar -xzf Kaelis-linux-x64.tar.gz
./kaelis-linux-x64/kaelis
```

The tarball is the simplest choice for a portable folder; it has no package
manager and no FUSE requirement.

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

Windows:

```powershell
certutil -hashfile Kaelis.exe SHA256
```

macOS and Linux:

```bash
shasum -a 256 -c SHA256SUMS.txt
```

Compare the result with `SHA256SUMS.txt` beside the download. A copy that does
not match is not the file this page published.

## Links

- Every URL, endpoint, and installation path: <https://fuvji.github.io/kaelis/downloads/KAELIS-LINKS.txt>
- Releases and issues: <https://github.com/fuvji/kaelis>
- Community: <https://discord.gg/fvrdYzrQjq>

Proprietary · © Kaelis Labs · [@epufk](https://github.com/fuvji)

Kaelis is not affiliated with Mojang Studios, Microsoft, Modrinth, or
CurseForge. Minecraft is a trademark of Mojang Studios.

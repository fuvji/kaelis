# Kaelis — portable

**Minecraft, composed.** A native Rust launcher for instances, modpacks, mods,
resource packs, and shaders.

## Run it

1. Keep `Kaelis.exe` in any folder you can write to (a USB stick works).
2. Double-click it. There is no installer, no runtime to add, and no first-run
   download of the launcher itself.

Windows 10/11 x64. The launcher stores instances, settings, and the managed
Java runtime under `%APPDATA%\kaelis\Kaelis\data`; set `KAELIS_DATA_DIR` to keep
everything beside the executable instead.

## What is inside

- One Content tab for Modrinth and CurseForge: mods, resource packs, shaders,
  and `.mrpack` modpacks, with `Install` and one-click `Erase`.
- Modpacks become their own instances, including the loader they ask for.
- Java is provisioned automatically the first time you press Play, matched to
  the Minecraft version.
- Offline profiles show the real Steve head; Microsoft sign-in shows your own
  skin.
- Instances carry per-instance heap, JVM arguments, window rules, quick join,
  artwork, and notes.

## Links

- Source and releases: <https://github.com/epufk/kaelis>
- Issues: <https://github.com/epufk/kaelis/issues>
- Community: <https://discord.gg/fvrdYzrQjq>

MIT · Kaelis Labs · Founder [@epufk](https://github.com/epufk)

Kaelis is not affiliated with Mojang Studios, Microsoft, Modrinth, or
CurseForge. Minecraft is a trademark of Mojang Studios.

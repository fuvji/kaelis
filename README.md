# Kaelis

A lightweight Minecraft launcher for Windows, macOS, and Linux.

[Website](https://fuvji.github.io/kaelis/) · [Download](https://github.com/fuvji/kaelis/releases/latest) · [Report an issue](https://github.com/fuvji/kaelis/issues) · [Discord](https://discord.gg/fvrdYzrQjq)

[![CI](https://github.com/fuvji/kaelis/actions/workflows/ci.yml/badge.svg)](https://github.com/fuvji/kaelis/actions/workflows/ci.yml)

## Features

- Create and manage Minecraft instances.
- Browse and install modpacks, mods, resource packs, and shaders.
- Import and export portable instances.
- Sign in with Microsoft or use an offline profile.
- Configure Java, memory, and launch options.
- Automatically clean temporary files and logs.

## Download

Get the latest version from [GitHub Releases](https://github.com/fuvji/kaelis/releases/latest). Choose the installer or portable download for your platform.

## Build from source

Install [Rust](https://www.rust-lang.org/tools/install) and the build tools for your platform, then run:

```bash
git clone https://github.com/fuvji/kaelis.git
cd kaelis
cargo build --release -p kaelis-launcher
```

## License

Kaelis is licensed under the MIT License. It is not affiliated with Mojang Studios, Microsoft, Modrinth, or CurseForge. Minecraft is a trademark of Mojang Studios.

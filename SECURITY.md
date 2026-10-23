# Security policy

Kaelis stores credentials in the operating system keychain and contacts only the
services it needs: Mojang, Modrinth, and CurseForge. There is no telemetry and no
background service.

## Supported versions

| Version | Supported |
| ------- | --------- |
| 0.1.x   | Yes       |

Only builds downloaded from the [project site](https://fuvji.github.io/kaelis/)
or the [GitHub releases](https://github.com/fuvji/kaelis/releases) are supported.
Verify the published `SHA256SUMS.txt` before running a binary from anywhere else.

## Reporting a vulnerability

Use a [private security advisory](https://github.com/fuvji/kaelis/security/advisories/new).
Please do not open a public issue for an unpatched vulnerability.

Include the version from `kaelis --version`, the platform, and the smallest set
of steps that reproduces the problem. A proof of concept helps; a live exploit
against someone else's account does not.

Expect an acknowledgement within three days. Fixes ship as a patch release, and
the advisory is published once a build is available.

## Scope

In scope: the launcher, the update and download paths, the instance archive
format, token handling, and the static site.

Out of scope: Minecraft itself, third-party mods and modpacks, and reports that
require an already-compromised machine.

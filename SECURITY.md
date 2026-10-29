# Security policy

Kaelis stores credentials in the operating system keychain and contacts only
Mojang, Modrinth, and CurseForge. No telemetry, no background service. Every
launch rewrites the instance's `options.txt` with `telemetryOptIn:false`.

## Supported versions

| Version | Supported |
| --- | --- |
| 0.4.x | Yes |

Only builds from the [project site](https://fuvji.github.io/kaelis/) or
[GitHub releases](https://github.com/fuvji/kaelis/releases) are supported.
Verify `SHA256SUMS.txt` before running a binary from anywhere else.

## Reporting a vulnerability

Use a [private security advisory](https://github.com/fuvji/kaelis/security/advisories/new).
Do not open a public issue for an unpatched vulnerability.

Include the version from `kaelis --version`, the platform, and the smallest
reproduction. A proof of concept helps; a live exploit against someone else's
account does not.

Expect an acknowledgement within three days. Fixes ship as a patch release, and
the advisory is published once a build is available.

## Scope

In scope: the launcher, the update and download paths, the instance archive
format, token handling, and the static site.

Out of scope: Minecraft itself, third-party mods and modpacks, and reports that
require an already-compromised machine.

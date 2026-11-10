# Kaelis · agent contract

Read [docs/brain/Kaelis Brain.md](docs/brain/Kaelis%20Brain.md) before non-trivial work.
It is the shared source of truth for humans and agents; sync it to Obsidian with
`tools/sync-brain.ps1`.

## What this is

Kaelis is a proprietary, native Rust Minecraft launcher for Windows, macOS and
Linux. Instances, content, accounts and the library live in one immediate-mode
window. No telemetry, no background daemon, no admin rights, no third-party
requests on the product page.

## Invariants

- Native first. Hot paths stay in Rust; the launcher is event-driven when idle.
- One version: `[workspace.package].version` in `Cargo.toml`. Never duplicate it
  in a crate manifest.
- No unmeasured claims. Benchmark numbers must name the host, harness, fixture,
  run count and median. Never compare rows from different harnesses.
- No hidden state, telemetry endpoint, cookie or third-party page request.
- No source leaks into `site/`, release assets or public artifacts.
- Preserve the licence: proprietary freeware, not open source.
- Web: one inline document; rounded radius tokens; transform-only aurora;
  static grain; lazy previews; reduced-motion fallbacks.

## Local commands

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p kaelis-launcher
powershell -NoProfile -File tools/serve-site.ps1 -Port 8088
```

This Windows host cannot link locally because `dlltool.exe` is missing from the
installed GNU toolchain. Use GitHub CI as the compile/test gate and say so
plainly; do not claim a local build passed.

## Release contract

1. Bump the workspace version (current line: `0.4.x`).
2. Add `## What's new in <version>` to `README.md`; release notes are lifted from
   that exact heading.
3. Add a `CHANGELOG.md` section.
4. Run formatting; push; wait for CI green.
5. Tag `v<version>` and push the tag. Never move a published tag.
6. Verify the release checksum, Pages hydration and live links.
7. Update the brain's [Current State](docs/brain/Kaelis%20Current%20State.md) note.

## Change discipline

- Make the smallest change that fully solves the request.
- Update the brain when architecture, commands, versions, benchmarks or release
  state change.
- Prefer measured results over optimistic copy. Flat results are labelled flat.
- Inspect the final diff and test the affected surface before finishing.

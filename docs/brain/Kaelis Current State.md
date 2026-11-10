---
type: reference
summary: Current Kaelis release, repo, site, dependency and open-item snapshot.
project: Kaelis
created: 2026-11-10
last_touched: 2026-11-10
title: Kaelis Current State
tags:
  - kaelis
  - state
status: active
updated: 2026-11-10
---

# Kaelis Current State

> [!info] Snapshot
> Latest release **v0.4.6**; product page live; workspace version 0.4.6.

## Repo

- Branch `main`.
- Site/benchmark commit: `7224358` (before this brain/dependency commit).
- Latest tag: `v0.4.6` (2026-11-10T22:02:25Z).
- Windows release asset: `Kaelis.exe`, 17,250,816 B.
- SHA-256: `2d5b03bae73066e0fdf0366bdc62cd48a68e506d3f57cc431680538cb1db0370`.
- Release: https://github.com/fuvji/kaelis/releases/tag/v0.4.6

## Site

- https://fuvji.github.io/kaelis/
- Benchmark lab: 0.3.7 / 0.4.5 / 0.4.6, same harness.
- Memory 86.6 → 18.9 MB (−78%); startup and CLI flat/noise.
- Radius 6/10/14/20/28/999 px; static grain; transform-only aurora.
- Seven 1080×700 previews from the 0.4.6 binary.
- Pages deploy green; direct binary on Pages is 404 by design.

## Dependencies

- `cargo update` applied 24 compatible patches (async, cc, flate2, openssl,
  rustls-pki-types, toml, uuid, zstd and related).
- Local Rust linking remains blocked by missing `dlltool.exe`; CI is the compile
  and test gate.
- Current Rust: 1.99.1; minimum supported: 1.85.

## Open

- Brain synced into the user's Obsidian vault under `01-Projects/Kaelis/Brain`.
- Final CI for the dependency/brain commit.
- Plan the first measured 0.4.7 optimization.

## Links

- [[Kaelis Brain]]
- [[Kaelis Build And Release]]
- [[Kaelis Roadmap]]

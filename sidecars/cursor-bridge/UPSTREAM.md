# Upstream provenance

This directory contains a modified copy of the server component of **cursor-byok**.

| Field | Value |
|---|---|
| Repository | https://github.com/leookun/cursor-byok |
| Baseline commit | `2068ab20513288e6a0289d33febe872a30608610` |
| Baseline commit date | 2026-09-20 11:32:53 +0800 |
| Baseline commit subject | `chore(release): bump desktop to 1.0.1` |
| Upstream desktop version | 1.0.1 |
| Upstream server version | 0.1.0 |
| License | MIT (Copyright (c) 2026 leookun) |

## What was taken

| Upstream path | Path here |
|---|---|
| `server/` | `server/` |
| `protocols/cursor/` | `protocols/cursor/` |
| `crates/semble-core/` | `crates/semble-core/` |

The relative layout between `server/` and `protocols/` is preserved deliberately.
`server/build.rs` resolves the protobuf sources as `manifest.join("../protocols/cursor")`.

Not taken: `apps/desktop/` (CodeRelay supplies its own frontend), `images/`, `support/`,
`.agents/`, `.github/`.

## Nested upstream

`crates/semble-core/` is itself derived from a separate upstream, recorded in that
directory's own `UPSTREAM.md`:

- Repository: https://github.com/MinishLab/semble
- Baseline commit: `921849164e2632dd4f0e1c1370f82cfe15ed6d6c`
- License: MIT
- Model: `minishlab/potion-code-16M-v2` (MIT), downloaded at runtime, not bundled

## Why the library name is unchanged

`server/Cargo.toml` keeps `[lib] name = "cursor_server"`. Only the `[package]` and
`[[bin]]` names were changed. This keeps `use cursor_server::` paths intact throughout
the source and makes future rebases against upstream tractable.

## Maintenance note

The upstream works by speaking Cursor's private protobuf protocol, which Cursor may
change at any time. Upgrading the baseline commit is a deliberate, tested operation,
not a routine dependency bump.

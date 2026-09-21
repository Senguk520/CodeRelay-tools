# Upstream provenance

`crates/semble-core/` is itself derived from a separate upstream, distinct from the
`cursor-byok` upstream recorded in the parent `../UPSTREAM.md`. The facts below are
carried over from that parent record.

| Field | Value |
|---|---|
| Repository | https://github.com/MinishLab/semble |
| Baseline commit | `921849164e2632dd4f0e1c1370f82cfe15ed6d6c` |
| License | MIT |

## What was taken

The transport-independent indexing and hybrid-search engine, as vendored by
`cursor-byok`. It is kept because the bridge's search tooling depends on it.

## Runtime asset, not bundled

The embedding model is **downloaded at runtime**, not compiled into the binary or
shipped in the installer. `src/embedding/assets.rs` holds the URL and SHA-256
integrity check:

| Field | Value |
|---|---|
| Model | `minishlab/potion-code-16M-v2` |
| License | MIT |
| Source | https://huggingface.co/minishlab/potion-code-16M-v2 |
| Integrity | SHA-256 `75cf7a6c2171b230ad19b1e7d8e0b1aee86da5a02af8e7cacedd9921d227623c` |

Because the model is fetched lazily on first use, a bridge that never runs a search
never downloads it. This is why the search subsystem was retained during Phase 1
rather than deleted alongside the plugin system; see the parent `../README.md`.

## Local changes

The only CodeRelay-specific change in this crate is the on-disk cache location, which
was moved off the upstream `.cursor-byok-v3/cache/semble` path to
`.coderelay-cursor-bridge/cache/semble` so it cannot collide with an upstream
installation's data directory (`src/config.rs`).

# Upstream provenance

`crates/semble-core/` has **two** upstreams that must not be conflated: the Rust
code comes from one project; the search algorithm and embedding model from another.

| Layer | Repository | Baseline commit | License |
|---|---|---|---|
| Code provenance | https://github.com/leookun/cursor-byok | `2068ab20513288e6a0289d33febe872a30608610` | MIT |
| Algorithm / model provenance | https://github.com/MinishLab/semble | `921849164e2632dd4f0e1c1370f82cfe15ed6d6c` | MIT |

**Code provenance.** Every `.rs` file and the `Cargo.toml` in this crate are a
vendored copy from `cursor-byok`.

**Algorithm / model provenance.** `MinishLab/semble` is a **pure Python** project:
at the baseline commit above it holds no `.rs` files and no `Cargo.toml`, and its
`pyproject.toml` declares `Programming Language :: Python :: 3 :: Only`. It
supplies the *design* of the transport-independent indexing and hybrid-search
engine, and the embedding model fetched at runtime — it contributes **no Rust
code** to this crate.

Earlier revisions recorded `MinishLab/semble` as the *code* source of this crate.
That was an error, corrected here (2026-10-21).

## What was taken

The transport-independent indexing and hybrid-search engine, kept because the
bridge's search tooling depends on it. The implementation was taken from
`cursor-byok`; the algorithm it implements and the embedding model it uses were
taken from `MinishLab/semble`.

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

Three CodeRelay-specific changes, all contained in this crate:

- **Cache location.** Moved off the upstream `.cursor-byok-v3/cache/semble` path to
  `.coderelay-cursor-bridge/cache/semble`, so it cannot collide with an upstream
  installation's data directory (`src/config.rs`).
- **`Default` impl removed; `cache_dir` is now required.** `SembleConfig` no longer
  implements `Default` (`src/config.rs:26`). The removed impl fell back to
  `~/.coderelay-cursor-bridge/cache/semble` (overridable via
  `SEMBLE_CACHE_LOCATION`), contradicting the host contract that bridge state lives
  under `CODERELAY_CURSOR_DATA_DIR` — and it did so on a path reachable with no
  user action: one tool call from a model would create a cache in the user's home
  directory, download tens of megabytes of model assets from huggingface.co, and
  `git clone` whatever URL the model supplied. `cache_dir` is now a required
  constructor argument that the host fills with its managed data directory. This is
  a deliberate breaking public-API change made for safety.
- **Remote Git sources are HTTPS-only.** `RemoteRepository::acquire` rejects any URL
  that is not `https` with a host (`src/source/remote_git.rs:25`): the clone output
  becomes the text the index is built from, so a plaintext `http://` URL would let
  anyone on the network path choose that text. Covered by the regression test
  `remote_sources_reject_non_https_urls_before_git_runs`
  (`src/source/remote_git.rs:80`).

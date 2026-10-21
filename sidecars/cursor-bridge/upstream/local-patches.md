# Local patches against upstream `cursor-byok`

This document records a per-file comparison between the local `sidecars/cursor-bridge`
tree and the upstream repository it was derived from.

## Provenance

| Field | Value |
|---|---|
| Upstream repository | https://github.com/leookun/cursor-byok |
| Baseline commit | `2068ab20513288e6a0289d33febe872a30608610` |
| Baseline commit date | 2026-09-20 11:32:53 +0800 |
| Baseline commit subject | `chore(release): bump desktop to 1.0.1` |
| Comparison date | 2026-10-21 |
| Local tree compared | `sidecars/cursor-bridge` (git-tracked files only, 272 files) |
| Local repository | `H:\CodeRelay\Independent_new_program` |

## Method

### Retrieval

The upstream tree was fetched over an explicit HTTP proxy at `http://127.0.0.1:7890`.
The machine has **no system proxy and no TUN mode**, so the proxy was passed
explicitly to every network operation (`-Proxy` for `Invoke-WebRequest`,
`-x` for `curl`, `-c http.proxy=... -c https.proxy=...` for `git`). No request
relied on automatic proxy detection.

The clone was made into a temporary directory outside the repository with
`--filter=blob:none --no-checkout`, then the pinned commit was fetched with
`--depth 1` and checked out. `git rev-parse HEAD` was verified to return
`2068ab20513288e6a0289d33febe872a30608610`. No git write command was run inside
the project repository.

### Comparison basis

* **Upstream side**: `git ls-tree -r --name-only HEAD -- <scope>`, restricted to the
  three in-scope directories. Only files tracked at the pinned commit are considered.
* **Local side**: `git -C H:\CodeRelay\Independent_new_program ls-files -- sidecars/cursor-bridge`,
  restricted to the same three scopes. Untracked working-tree files are ignored.
* **Path normalization**: the local `sidecars/cursor-bridge/` prefix is stripped so
  paths line up with the upstream relative paths.

### Content hashing and CRLF normalization

For every file present on both sides, a SHA-256 digest is computed after normalizing
line endings:

* **Binary extensions** are hashed from **raw bytes**, unmodified. The binary
  whitelist is:
  `.png .jpg .jpeg .gif .ico .dll .lib .zip .exe .woff .woff2 .onnx .bin`
* **Everything else is treated as text** and hashed after every `\r\n` pair is
  collapsed to `\n`. The normalization is applied at the byte level, so it is safe
  for UTF-8 content and does not depend on any particular decoding.

The decision rule is deliberately stated as **"binary whitelist, negation"** rather
than as a text-extension whitelist. Using a text whitelist would silently skip
normalization for `.rs`, `.lock`, `.toml`, `.md`, `.json`, `.sql` and similar files
and would manufacture a large number of false `modified` results purely from
line-ending differences.

**Measured outcome in this run**: the normalization changed nothing. All files in the
compared set are stored with CRLF on *both* sides, so 0 of the 178 `identical`
files in `server/` differ under raw-byte hashing. The normalization is retained
because it makes the comparison robust against a future checkout that rewrites
line endings, but no result below depends on it.

### State definitions

| State | Meaning |
|---|---|
| `identical` | Present on both sides, digests equal. |
| `modified` | Present on both sides, digests differ. |
| `local_added` | Present locally, absent upstream. |
| `upstream_only` | Present upstream, absent locally (not taken, or dropped). |

## Scope

Only three upstream directories are compared:

* `server/`
* `protocols/cursor/`
* `crates/semble-core/`

Other upstream top-level paths are out of scope and are not counted in any of the
four states. See *Upstream paths not taken* below.

## Per-directory counts

### `server/`

| State | Count |
|---|---:|
| `identical` | 178 |
| `modified` | 46 |
| `local_added` | 3 |
| `upstream_only` | 62 |
| **Upstream files** | **286** |
| **Local files** | **227** |

178 + 46 + 62 = 286 upstream files accounted for; 178 + 46 + 3 = 227 local files.

### `protocols/cursor/`

| State | Count |
|---|---:|
| `identical` | 6 |
| `modified` | 0 |
| `local_added` | 0 |
| `upstream_only` | 0 |
| **Upstream files** | **6** |
| **Local files** | **6** |

### `crates/semble-core/`

| State | Count |
|---|---:|
| `identical` | 28 |
| `modified` | 3 |
| `local_added` | 1 |
| `upstream_only` | 0 |
| **Upstream files** | **31** |
| **Local files** | **32** |

## Directory-level summary

| State | `server/` | `protocols/cursor/` | `crates/semble-core/` | Total |
|---|---:|---:|---:|---:|
| `identical` | 178 | 6 | 28 | **212** |
| `modified` | 46 | 0 | 3 | **49** |
| `local_added` | 3 | 0 | 1 | **4** |
| `upstream_only` | 62 | 0 | 0 | **62** |
| **Total rows** | 289 | 6 | 32 | **327** |

Reconciliation: 323 upstream files (286 + 6 + 31) and 265 local files in scope
(227 + 6 + 32). The 7 remaining local tracked files sit outside the three scopes
(`.cargo/`, `.gitignore`, `Cargo.lock`, `Cargo.toml`, `LICENSE`, `README.md`,
`UPSTREAM.md`), giving the expected 272 tracked files in total.

## `modified` files (49)

Sizes are raw byte counts; upstream size first, local size second. Paths are relative
to the directory named in the heading.

### `server/` (46)

| Path | Upstream bytes | Local bytes |
|---|---:|---:|
| `Cargo.toml` | 2195 | 2240 |
| `src/api/cursor/handlers.rs` | 13525 | 14281 |
| `src/api/cursor/proxy.rs` | 6882 | 6506 |
| `src/app.rs` | 6769 | 8065 |
| `src/bin/cursor-server.rs` | 503 | 914 |
| `src/config.rs` | 6024 | 22813 |
| `src/control/harness.rs` | 790 | 1398 |
| `src/control/mod.rs` | 9738 | 15541 |
| `src/control/models.rs` | 2688 | 2793 |
| `src/control/service.rs` | 38580 | 28932 |
| `src/control/settings.rs` | 5846 | 5040 |
| `src/cursor/checkpoint/messages/decode.rs` | 9523 | 9579 |
| `src/cursor/checkpoint/messages/mod.rs` | 306 | 311 |
| `src/cursor/services/account.rs` | 17071 | 17098 |
| `src/cursor/services/commit_message.rs` | 14584 | 14239 |
| `src/cursor/services/compatibility.rs` | 3477 | 3504 |
| `src/cursor/services/knowledge/mod.rs` | 12507 | 12534 |
| `src/cursor/services/mod.rs` | 336 | 322 |
| `src/cursor/services/model_catalog.rs` | 29813 | 24835 |
| `src/cursor/services/server_config.rs` | 1475 | 1487 |
| `src/cursor/services/usage.rs` | 7479 | 7522 |
| `src/cursor/tools/compat.rs` | 4162 | 4191 |
| `src/cursor/tools/tool_call_result/interaction.rs` | 15970 | 15988 |
| `src/cursor/transport/registry.rs` | 8368 | 7661 |
| `src/error.rs` | 2713 | 2725 |
| `src/lib.rs` | 414 | 456 |
| `src/local_app/account.rs` | 6310 | 13048 |
| `src/local_app/ca/mod.rs` | 8614 | 14933 |
| `src/local_app/ca/windows.rs` | 2366 | 3692 |
| `src/local_app/mod.rs` | 7494 | 11874 |
| `src/local_app/process.rs` | 2672 | 6923 |
| `src/local_app/proxy.rs` | 8207 | 7786 |
| `src/local_app/settings.rs` | 4003 | 19435 |
| `src/model/configuration.rs` | 16264 | 17697 |
| `src/model/observability.rs` | 9088 | 7477 |
| `src/network.rs` | 7542 | 7554 |
| `src/provider/mod.rs` | 6692 | 6704 |
| `src/provider/router.rs` | 17002 | 14342 |
| `src/search/engine.rs` | 9936 | 9927 |
| `src/search/fetch.rs` | 10962 | 10953 |
| `src/search/search_provider.rs` | 4502 | 5041 |
| `src/store/llm_calls.rs` | 19974 | 18259 |
| `src/store/migrations.rs` | 16918 | 16922 |
| `src/store/mod.rs` | 599 | 564 |
| `src/store/models.rs` | 16451 | 19916 |
| `src/store/settings.rs` | 23123 | 19717 |

### `crates/semble-core/` (3)

| Path | Upstream bytes | Local bytes |
|---|---:|---:|
| `Cargo.toml` | 935 | 923 |
| `src/config.rs` | 1107 | 1435 |
| `src/source/remote_git.rs` | 3036 | 3677 |

The largest divergences are concentrated in the local-app and settings areas
(`src/config.rs` +16789 bytes, `src/local_app/settings.rs` +15432 bytes), which is
consistent with CodeRelay's own local-application and settings surface being layered
onto the upstream server.

## `local_added` files (4)

| Scope | Path | Local bytes |
|---|---|---:|
| `server/` | `src/limits.rs` | 1758 |
| `server/` | `src/parent_monitor.rs` | 5856 |
| `server/` | `src/secret.rs` | 9696 |
| `crates/semble-core/` | `UPSTREAM.md` | 1606 |

`crates/semble-core/UPSTREAM.md` is absent upstream and was added locally to record
the nested provenance described in *Nested upstream* below.

## `upstream_only` files (62)

All 62 are in `server/`. They are upstream files with no local counterpart, i.e. not
taken or since dropped. No `upstream_only` files exist under `protocols/cursor/` or
`crates/semble-core/`.

| Group | Count | Paths |
|---|---:|---|
| `plugins/build-in/` | 30 | `antigravity-auth/` (11), `codex-auth/` (10), `grok-auth/` (9) |
| `src/plugin/` | 26 | see list below |
| `src/control/` | 3 | `ads.rs`, `overview.rs`, `plugins.rs` |
| `src/store/` | 2 | `legacy_config.rs`, `overview.rs` |
| `src/cursor/` | 1 | `services/tab.rs` |
| **Total** | **62** | |

The bulk is the built-in plugin subsystem (`src/plugin/` plus `plugins/build-in/`),
which CodeRelay does not take. Full paths:

```
plugins/build-in/antigravity-auth/assets/antigravity.svg
plugins/build-in/antigravity-auth/deno.json
plugins/build-in/antigravity-auth/google_oauth.ts
plugins/build-in/antigravity-auth/main.ts
plugins/build-in/antigravity-auth/models.ts
plugins/build-in/antigravity-auth/oauth.ts
plugins/build-in/antigravity-auth/oauth_test.ts
plugins/build-in/antigravity-auth/plugin.json
plugins/build-in/antigravity-auth/provider.ts
plugins/build-in/antigravity-auth/provider_test.ts
plugins/build-in/antigravity-auth/resources.ts
plugins/build-in/antigravity-auth/resources_test.ts
plugins/build-in/codex-auth/assets/codex.svg
plugins/build-in/codex-auth/codex_test.ts
plugins/build-in/codex-auth/deno.json
plugins/build-in/codex-auth/main.ts
plugins/build-in/codex-auth/models.ts
plugins/build-in/codex-auth/oauth.ts
plugins/build-in/codex-auth/plugin.json
plugins/build-in/codex-auth/provider.ts
plugins/build-in/codex-auth/resources.ts
plugins/build-in/grok-auth/assets/grok.svg
plugins/build-in/grok-auth/deno.json
plugins/build-in/grok-auth/grok_test.ts
plugins/build-in/grok-auth/main.ts
plugins/build-in/grok-auth/models.ts
plugins/build-in/grok-auth/oauth.ts
plugins/build-in/grok-auth/plugin.json
plugins/build-in/grok-auth/provider.ts
plugins/build-in/grok-auth/resources.ts
src/control/ads.rs
src/control/overview.rs
src/control/plugins.rs
src/cursor/services/tab.rs
src/plugin/asset.rs
src/plugin/builtin.rs
src/plugin/catalog.rs
src/plugin/data.rs
src/plugin/definition.rs
src/plugin/descriptor.rs
src/plugin/installation.rs
src/plugin/manifest.rs
src/plugin/mod.rs
src/plugin/oauth_callback.rs
src/plugin/protocol.rs
src/plugin/registry.rs
src/plugin/runtime.rs
src/plugin/sdk/collect.ts
src/plugin/sdk/deno.json
src/plugin/sdk/import-map.json
src/plugin/sdk/model.ts
src/plugin/sdk/plugin.ts
src/plugin/sdk/protocol/openai_chat.ts
src/plugin/sdk/protocol/openai_responses.ts
src/plugin/sdk/provider.ts
src/plugin/sdk/resource.ts
src/plugin/sdk/worker.ts
src/plugin/state.rs
src/plugin/wire.rs
src/plugin/worker.rs
src/store/legacy_config.rs
src/store/overview.rs
```

## Protobuf files

All six protobuf definitions under `protocols/cursor/` are byte-for-byte identical to
upstream after normalization. Sizes are raw local byte counts, and each was
independently confirmed against the on-disk file and matched the upstream size.

| Proto | State | Bytes |
|---|---|---:|
| `agent_v1.proto` | `identical` | 262563 |
| `aiserver_v1.proto` | `identical` | 1522950 |
| `anyrun_v1.proto` | `identical` | 48696 |
| `git_forge_v1.proto` | `identical` | 25108 |
| `internapi_v1.proto` | `identical` | 1039 |
| `origin_v1.proto` | `identical` | 141399 |

This matters for two reasons:

* `agent_v1.proto` is the **sole compilation input** for `server/build.rs`, so any
  drift there would change generated code for the whole server.
* `aiserver_v1.proto` is the basis for **14 wire assertions**, so an unnoticed edit
  would silently break those checks.

Neither file has drifted. The local copies are unmodified upstream copies.

## `server/tests/` integration tests

The 16 files under `server/tests/` are **all `identical` to upstream**. There are no
local changes in this directory.

| File | State |
|---|---|
| `checkpoint_recovery.rs` | `identical` |
| `commit_message.rs` | `identical` |
| `compaction.rs` | `identical` |
| `connect_wire.rs` | `identical` |
| `conversation_delivery.rs` | `identical` |
| `cursor_trace_queue.rs` | `identical` |
| `cursor_transport_lifecycle.rs` | `identical` |
| `error_lifecycle.rs` | `identical` |
| `interrupt.rs` | `identical` |
| `knowledge_rules.rs` | `identical` |
| `local_rules_context.rs` | `identical` |
| `prefix_stability.rs` | `identical` |
| `support/fake_cursor.rs` | `identical` |
| `support/fake_provider.rs` | `identical` |
| `support/fixtures.rs` | `identical` |
| `tool_round.rs` | `identical` |

Because the tests are untouched but 49 source files diverged, the existing test suite
still exercises upstream behaviour against locally modified code. Any behaviour the
local patches rely on is therefore **not** covered by a local regression test that
would have been updated alongside it.

## Nested upstream: `crates/semble-core/`

`crates/semble-core/` is itself derived from a separate upstream, distinct from
`cursor-byok`, and recorded in that directory's own `UPSTREAM.md` (added locally,
absent upstream):

| Field | Value |
|---|---|
| Repository | https://github.com/MinishLab/semble |
| Baseline commit | `921849164e2632dd4f0e1c1370f82cfe15ed6d6c` |
| License | MIT |

The counts for `crates/semble-core/` in this document compare it against the
**`cursor-byok` vendored copy at `2068ab2`**, not against `MinishLab/semble` at
`9218491`. Diffing against the nested upstream is **out of scope here** and was not
performed. The 3 `modified` files in that directory (`Cargo.toml`, `src/config.rs`,
`src/source/remote_git.rs`) are therefore local deltas relative to the vendored copy,
and it is not established whether they also differ from MinishLab/semble itself.

## Upstream paths not taken

These upstream top-level paths were not taken into the local tree and are excluded
from all four states. File counts are from `git ls-tree -r HEAD`:

| Upstream path | Files | Note |
|---|---:|---|
| `apps/desktop/` | 193 | CodeRelay supplies its own frontend |
| `support/` | 50 | not taken |
| `.agents/` | 10 | not taken |
| `.github/` | 6 | not taken |
| `images/` | 3 | not taken |

Upstream root files (`AGENTS.md`, `Cargo.lock`, `Cargo.toml`, `Dockerfile`,
`Makefile`, `README.md`, `README-CN.md`, `UPSTREAM.md`, `cursor.md`, `LICENSE`,
`.gitignore`, `.dockerignore`) are workspace-level and likewise out of scope.

## How to reproduce

The commands below re-run this comparison from scratch. All network access goes
through the explicit proxy; the clone lands in a temporary directory and nothing is
written inside the project repository.

```powershell
# 0) Proxy. Required explicitly: no system proxy, no TUN mode on this machine.
$p = "http://127.0.0.1:7890"
$env:HTTP_PROXY = $p; $env:HTTPS_PROXY = $p

# 1) Fetch upstream into a temp dir outside the repo.
$t = "$env:TEMP\cb-$([guid]::NewGuid().ToString('N').Substring(0,6))"
git -c http.proxy=$p -c https.proxy=$p clone --filter=blob:none --no-checkout `
    https://github.com/leookun/cursor-byok "$t\src"
git -C "$t\src" -c http.proxy=$p -c https.proxy=$p fetch --depth 1 origin `
    2068ab20513288e6a0289d33febe872a30608610
git -C "$t\src" checkout 2068ab20513288e6a0289d33febe872a30608610
git -C "$t\src" rev-parse HEAD   # must print the commit above

# 2) Build the two path lists.
$scopes = @('server','protocols/cursor','crates/semble-core')
git -C "$t\src" ls-tree -r --name-only HEAD -- $scopes
git -C H:\CodeRelay\Independent_new_program ls-files -- sidecars/cursor-bridge

# 3) Hash with CRLF -> LF normalization, binary whitelist by raw bytes.
#    Compare with the rule: text = NOT in the binary whitelist.
#    Binary whitelist:
#    .png .jpg .jpeg .gif .ico .dll .lib .zip .exe .woff .woff2 .onnx .bin
#
#    For each path present on both sides:
#      - binary extension -> SHA-256 of raw bytes
#      - anything else    -> drop every 0x0D that precedes 0x0A, then SHA-256
#    Equal digests -> identical; otherwise -> modified.
#    Present only upstream -> upstream_only; only locally -> local_added.
```

Line endings are CRLF on both sides in this run, so normalization is a no-op here.
It is kept in the method because a future checkout with different line-ending
handling would otherwise produce a large number of false `modified` results.


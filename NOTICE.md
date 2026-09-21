# CodeRelay third-party notices

## cursor_byok (Cursor bridge sidecar)

`sidecars/cursor-bridge` is a modified copy of the server component of
**cursor_byok**, baseline commit `2068ab20513288e6a0289d33febe872a30608610`,
distributed under the MIT License (Copyright (c) 2026 leookun). The original
copyright and permission notice is preserved verbatim in
`sidecars/cursor-bridge/LICENSE`.

The CodeRelay bridge removes the plugin system, ad placements, legacy config
import, home statistics and token pricing, and the Tab completion subsystem; it
also renames the data directory, environment variables, and certificate
authority to CodeRelay-specific values. Those changes are maintained separately
from the upstream snapshot. See `sidecars/cursor-bridge/UPSTREAM.md` for the full
provenance record and `sidecars/cursor-bridge/README.md` for the change list.

`sidecars/cursor-bridge/crates/semble-core` is in turn derived from
[MinishLab/semble](https://github.com/MinishLab/semble) (MIT), baseline commit
`921849164e2632dd4f0e1c1370f82cfe15ed6d6c`; see that directory's `UPSTREAM.md`.
Its embedding model (`minishlab/potion-code-16M-v2`, MIT) is downloaded at
runtime, not bundled.

## CLIProxyAPI

`sidecars/coderelay-proxy/third_party/CLIProxyAPI` is based on CLIProxyAPI
v7.2.140, baseline commit `a7e3596b`, distributed under the MIT License.
The original copyright and permission notice is retained in
`third_party/CLIProxyAPI/LICENSE`.

The outer CodeRelay sidecar adds CodeBuddy CN request conversion, account-pool
routing, usage accounting, and host integration. Those changes are maintained
separately from the upstream snapshot.

## CodeRelay application

The application-specific code in this repository is distributed under the
MIT License with Commons Clause. The Commons Clause License Condition v1.0
restricts the right to Sell the Software: you may use, copy, modify, merge,
publish, and distribute it freely for non-commercial and internal purposes,
but you may not provide it to third parties for a fee or other consideration.

No Cockpit application branding, metadata, screenshots, tokens, or local
configuration files are included in the new application.

The full text of the MIT License with Commons Clause applicable to this
repository's own code is available in the root-level [LICENSE](./LICENSE) file.
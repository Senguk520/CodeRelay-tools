# Acknowledgments

## Upstream & Inspiration

This project's technology stack and dependency selection were informed by
[cockpit-tools](https://github.com/jlcodes99/cockpit-tools). CodeRelay is an
independent project with its own implementation; we acknowledge the upstream
project for the reference it provided during early development.

The Cursor bridge sidecar is a modified copy of
[cursor-byok](https://github.com/leookun/cursor-byok) (MIT, Copyright (c) 2026
leookun), vendored at commit `2068ab20513288e6a0289d33febe872a30608610`. See
`sidecars/cursor-bridge/UPSTREAM.md` for the full provenance record and
`sidecars/cursor-bridge/README.md` for the list of changes made here.

## Third-Party Dependencies

CodeRelay is built on the following open-source projects. We are grateful to
their authors and maintainers.

### Frontend (Node.js)

| Project | License |
|---|---|
| [React](https://react.dev/) | MIT |
| [zustand](https://github.com/pmndrs/zustand) | MIT |
| [lucide-react](https://lucide.dev/) | ISC |
| [Vite](https://vite.dev/) | MIT |
| [TypeScript](https://www.typescriptlang.org/) | Apache-2.0 |
| [@vitejs/plugin-react](https://github.com/vitejs/vite-plugin-react) | MIT |

### Desktop Shell (Rust)

| Project | License |
|---|---|
| [Tauri](https://tauri.app/) | MIT / Apache-2.0 |
| [serde](https://serde.rs/) | MIT / Apache-2.0 |
| [reqwest](https://github.com/seanmonstar/reqwest) | MIT / Apache-2.0 |
| [tokio](https://tokio.rs/) | MIT |
| [chrono](https://github.com/chronotope/chrono) | MIT / Apache-2.0 |

### Reverse Proxy Sidecar (Go)

| Project | License |
|---|---|
| [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) | MIT |
| [gin-gonic/gin](https://github.com/gin-gonic/gin) | MIT |
| [logrus](https://github.com/sirupsen/logrus) | MIT |
| [gjson / sjson](https://github.com/tidwall/gjson) | MIT |
| [klauspost/compress](https://github.com/klauspost/compress) | Apache-2.0 / BSD-3-Clause / MIT |
| [pion/webrtc](https://github.com/pion/webrtc) | MIT |
| [golang.org/x/*](https://pkg.go.dev/golang.org/x) | BSD-3-Clause |

### Cursor Bridge Sidecar (Rust)

`sidecars/cursor-bridge/` is a modified copy of cursor-byok (see above). It adds
the following dependencies on top of CodeRelay's own Rust stack:

| Project | License |
|---|---|
| [axum](https://github.com/tokio-rs/axum) | MIT |
| [hudsucker](https://github.com/omjadas/hudsucker) | Apache-2.0 / MIT |
| [rcgen](https://github.com/rustls/rcgen) | MIT / Apache-2.0 |
| [sqlx](https://github.com/launchbadge/sqlx) | MIT / Apache-2.0 |
| [prost](https://github.com/tokio-rs/prost) | Apache-2.0 |
| [tower-http](https://github.com/tower-rs/tower-http) | MIT |
| [tracing](https://github.com/tokio-rs/tracing) | MIT |
| [semble-core](https://github.com/MinishLab/semble) | MIT |

`semble-core` downloads its embedding model (`minishlab/potion-code-16M-v2`,
MIT) at runtime rather than bundling it. See
`sidecars/cursor-bridge/crates/semble-core/UPSTREAM.md`.

For the full third-party license texts, see the `LICENSE` files distributed
alongside each dependency and the [`NOTICE.md`](./NOTICE.md) file in this
repository.

## Contributors

<!-- Add contributor names and thanks here. -->

_To be filled in._

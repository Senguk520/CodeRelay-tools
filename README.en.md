# CodeRelay

A **Windows desktop management tool** for power users: centrally manage CodeBuddy China account pools and run a local **OpenAI-compatible reverse proxy service**, letting clients such as Cursor and CodeBuddy IDE connect to multiple accounts through a single local address with policy-based load balancing, cooldown, and quota scheduling.

![License](https://img.shields.io/badge/license-MIT%20with%20Commons%20Clause-blue)
![Version](https://img.shields.io/badge/version-0.3.0-blue)
![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-lightgrey)

**English** | [中文](./README.md)

> Visual design follows a "macOS flavor, Windows behavior" philosophy: black/white/gray hierarchy with semantic status colors, sidebar navigation, custom frameless title bar, and a persistent bottom service status bar.

---

## Features

- **Account Pool Management**: Add accounts via OAuth/web login, manual token paste, or config file import. Account health, cooldown, quota, and binding status are visible at a glance; long-press drag to reorder accounts, and export accounts for backup or migration.
- **Daily Check-in**: Check in a single account or all accounts with one click.
- **API Key Management**: `sk-*` prefixed keys with binding scope, model restrictions, aliases, and enable/disable.
- **Model Management**: Sync model lists from the online API with local caching that survives restarts; supports aliases and disabling, with per-account fallback and visible error reporting when a sync fails.
- **Local Reverse Proxy Service**: Listens on `127.0.0.1:11435` by default, manual start/stop, multi-account scheduling strategies with session affinity.
- **LAN Access**: Switch the access scope to "Local + LAN" and the app automatically detects and displays a LAN address (one-click copy, plus a Windows Firewall rule command); the address refreshes automatically when the network changes.
- **Request Statistics & Logs**: Total requests, tokens, cache hit rate, credit consumption, hourly bar chart and per-day aggregation; request logs are kept for the current day with filtering, details, and JSON export.
- **System Tray & Notifications**: Minimize to tray, start/stop proxy, quit; Windows system notifications on startup failure or service errors.
- **Update Check**: Queries the latest GitHub Release and compares it with the running version, surfacing new versions in the sidebar and on the overview page; shows release notes and opens the release page for download (CodeRelay does **not** download or install updates automatically).

---

## Screenshots

| Overview Home | Service Configuration | API Key Management |
|:---:|:---:|:---:|
| ![Overview Home](docs/image/home.png) | ![Service Configuration](docs/image/Service_configuration.png) | ![API Key Management](docs/image/API_Key.png) |

---

## Quick Start

### Prerequisites

- Node.js >= 20 (20+ recommended)
- Rust / cargo >= 1.8x
- Go >= 1.22
- Tauri CLI (provided as npm devDependency, no global install needed)

### Development

```powershell
npm install
npm run tauri:dev
```

### Production Build

```powershell
npm run tauri:build
```

Build artifacts (NSIS installer and MSI) are output to the Cargo target directory's `release/bundle/` subdirectory.

> Note: This project's Cargo target directory is configured separately (via `CARGO_TARGET_DIR` or `.cargo/config`) rather than the default `src-tauri/target`; the exact location depends on your build environment.

---

## Usage

1. **Add Accounts**: Go to "Account Pool" and add CodeBuddy China accounts via browser authentication, manual token, or config file import.
2. **Create an API Key**: On the "API Key" page, create a `sk-` prefixed key and bind it to the available account scope.
3. **Start the Service**: Manually start the reverse proxy from "Service Config" or the bottom status bar (the service does **not** auto-start with the app).
4. **Connect Your Client**: Point your client to `http://127.0.0.1:11435` and authenticate with the API key you created. To let phones, tablets, or other LAN devices connect, switch "Service Config -> Network -> Access Scope" to "Local + LAN" and copy the LAN address shown on the page (a Windows Firewall rule command is provided there as well).

### Connection Examples

**OpenAI-compatible base_url**:

```
http://127.0.0.1:11435/v1
```

**curl**:

```bash
curl http://127.0.0.1:11435/v1/chat/completions \
  -H "Authorization: Bearer sk-xxxx" \
  -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"Hello"}]}'
```

---

## Architecture

```
+----------------------------------------------+
|  Frontend   React 19 + TypeScript + Vite     |
|        (zustand state / lucide-react icons)   |
+----------------------+-----------------------+
                       | Tauri invoke / events
+----------------------+-----------------------+
|  Desktop Shell   Tauri 2 + Rust             |
|        (gateway.rs: sidecar process mgmt /    |
|         state machine)                        |
+----------------------+-----------------------+
                       | Launch sidecar + stdout events
+----------------------+-----------------------+
|  Reverse Proxy Sidecar   Go                  |
|        (embedded CLIProxyAPI v7.2.140 +       |
|         CodeBuddy CN patch,                  |
|         listens on 127.0.0.1:11435)           |
+----------------------------------------------+
```

**Data Flow**: Rust writes account credentials to a temporary `auths/` directory and generates `config.json`/`manifest.json` -> launches the sidecar -> the sidecar sends structured lifecycle and request events via **stdout (JSON lines)** -> Rust parses and updates state -> notifies the frontend to refresh via events.

---

## Developer Guide

### Directory Structure

| Path | Responsibility |
|---|---|
| `src/` | React frontend; `App.tsx` hosts main pages; `services.ts` wraps invoke/HTTP; `types.ts` types |
| `src-tauri/src/lib.rs` | Tauri entry point (plugins, single instance, tray, window) |
| `src-tauri/src/gateway.rs` | Core: sidecar process management, state machine, event parsing, credential hot-reload |
| `src-tauri/src/codebuddy_oauth.rs` | Account OAuth, token/quota refresh, check-in |
| `src-tauri/src/update.rs` | GitHub Releases update check (version comparison, installer URL parsing) |
| `src-tauri/src/models.rs` | Request log / statistics structures and app state model |
| `sidecars/coderelay-proxy/` | Go sidecar main program (relay server, model sync, account pool scheduling) |
| `scripts/` | `build-sidecar.ps1`, `sync-version.mjs` |

### Common Commands

```powershell
npm run typecheck          # Frontend TS type check
npm run build              # Frontend production build
npm run build:sidecar      # Build Go sidecar for Rust target triple
npm run sync-version       # Sync package.json version to tauri.conf.json/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
go build ./...             # under sidecars/coderelay-proxy
go test ./...
```

### Version Convention

**Single version source = `package.json` `version`**. To bump the version, modify only this file, then run `npm run sync-version` (or just `tauri:build`, which includes it in beforeBuild). The frontend displays the version via `APP_VERSION` (injected by Vite) — do not hardcode it. The Rust side reads the same version through `env!("CARGO_PKG_VERSION")` for update comparison, so nothing else needs maintaining.

### Release Convention

The update check reads the latest GitHub Release. Upload installers to the release **Assets**, named like `CodeRelay_<version>_x64-setup.zip`:

- **Assets first**: gives a stable direct URL, file name, and size, so the UI can show installer details.
- **Release body fallback**: if Assets is empty, the app parses `user-attachments` zip links from the release notes (compatible with v0.3.0 and earlier releases). Size is unavailable this way and shows as "—".
- Tags must parse as a version (e.g. `v0.3.0`). Unparsable tags such as `Bate_Version` are treated as "no update" rather than a false positive.

Update results are session state and are not written to `state.json`. "Check for updates on startup" is off by default; enable it under Settings → General.

---

## Acknowledgments

This project takes inspiration from the technology choices of [cockpit-tools](https://github.com/jlcodes99/cockpit-tools) and is built on many excellent open-source projects. See [ACKNOWLEDGMENTS.md](./ACKNOWLEDGMENTS.md) for the full upstream attribution and third-party dependency list.

The local bridge sidecar behind the "Cursor service" page is a modified copy of the server component of [cursor_byok](https://github.com/leookun/cursor-byok) (MIT License, Copyright (c) 2026 leookun), pinned to upstream commit `2068ab20513288e6a0289d33febe872a30608610`. Thanks to the author leookun and to the cursor_byok project: without that upstream implementation there would be no "Cursor service" page. The upstream copyright and license notices are preserved verbatim; see [sidecars/cursor-bridge/README.md](./sidecars/cursor-bridge/README.md) for the change list and [sidecars/cursor-bridge/UPSTREAM.md](./sidecars/cursor-bridge/UPSTREAM.md) for the provenance record.

> **Troubleshooting note**: this bridge works by speaking Cursor's private protobuf protocol, which Cursor may change at any time. After upgrading Cursor, fully quit and restart it, then re-enable injection.
>
> **Tab completion note**: this bridge no longer provides inline Tab completion. Those requests are forwarded to Cursor's official backend unchanged, so their availability depends on your own Cursor account quota. This is expected behavior, not a defect.

## Third-Party Components & Licenses

The reverse proxy sidecar is built on [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) (MIT License); the Cursor bridge sidecar is built on [cursor_byok](https://github.com/leookun/cursor-byok) (MIT License). For third-party license and attribution information, see:

- [NOTICE.md](./NOTICE.md)
- [ACKNOWLEDGMENTS.md](./ACKNOWLEDGMENTS.md)
- `sidecars/coderelay-proxy/third_party/CLIProxyAPI/LICENSE`
- `sidecars/cursor-bridge/LICENSE`, `sidecars/cursor-bridge/UPSTREAM.md`

---

## License

This project's own code is licensed under the [MIT License with Commons Clause](./LICENSE): the MIT License with the additional **Commons Clause License Condition v1.0**, which permits free use, copying, modification, merging, publishing, and distribution, but prohibits **selling the software or providing it to third parties for a fee or other consideration**. The third-party [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) and [cursor_byok](https://github.com/leookun/cursor-byok) components remain under their own original MIT Licenses, unaffected by the additional Commons Clause condition, which applies only to this project's own code.

---

## Disclaimer

CodeRelay is an independent third-party tool and is not affiliated with Tencent or CodeBuddy. Please comply with the terms of service of CodeBuddy and related upstream services, and use it only for lawful, compliant learning and personal purposes. Account credentials, tokens, and API keys are stored locally only — this project does not collect or upload any credentials.

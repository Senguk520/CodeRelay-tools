# cursor-bridge

A local model gateway that connects Cursor to the model channels configured in CodeRelay.

This is a **modified copy** of the server component of
[cursor-byok](https://github.com/leookun/cursor-byok), vendored into CodeRelay.
See `UPSTREAM.md` for the exact baseline commit and the full provenance record.
See `LICENSE` for the upstream MIT license, which is preserved verbatim.

## What it does

It runs a local service that Cursor talks to. A local certificate authority and a
loopback HTTP proxy intercept Cursor's traffic to `*.cursor.sh`. Agent requests are
answered locally by adapting them to the OpenAI-compatible endpoint that CodeRelay's
own relay exposes, so model requests flow to the CodeBuddy channel instead of Cursor.
Everything else is forwarded to Cursor's real backend unchanged.

## Build

From the repository root:

```powershell
npm run build:cursor-bridge
```

Or directly:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-cursor-bridge.ps1
```

The build uses its own Cargo target directory (`F:/target-cursor-bridge` by default)
so it never contends with the Tauri build's target directory. Override it with the
`CODERELAY_CURSOR_BRIDGE_TARGET_DIR` environment variable if needed.

The output lands in `bin/cursor-bridge-<target-triple>.exe`.

## Differences from upstream

This copy is not a drop-in replacement for the upstream server. Changes made here:

### Renamed for CodeRelay

- Package name is `coderelay-cursor-bridge`; the produced binary is `cursor-bridge`.
  The library name is still `cursor_server`.
- Data directory is CodeRelay-specific and overridable by environment variable, so it
  can never share a SQLite file with an upstream installation.
- Environment variables use the `CODERELAY_CURSOR_*` prefix.
- The generated certificate authority identifies itself as CodeRelay, not as upstream.

### Removed

- **Plugin system.** CodeRelay already has its own account pool and upstream routing;
  the plugin channel was redundant and was the only source of long-lived child processes.
- **Ad placements.** These contacted a third-party server operated by the upstream author.
- **Legacy config import.** Migration support for the upstream author's pre-1.0 format.
- **Home statistics and token pricing.** CodeRelay has its own request log and statistics.
- **Tab completion subsystem, in full.** Tab completion was never implemented locally
  upstream; that code only forwarded requests to either Cursor's official backend or to a
  third-party service. This copy forwards those requests to Cursor's official backend.
  Tab completion therefore depends on the user's own Cursor account. Implementing local
  Tab completion is a separate project.

## Licensing

Upstream is MIT licensed. `LICENSE` is the upstream text, unmodified and preserved in
full. CodeRelay's own license does not apply to this directory; see the repository root
`NOTICE.md` and `ACKNOWLEDGMENTS.md` for how CodeRelay records third-party components.

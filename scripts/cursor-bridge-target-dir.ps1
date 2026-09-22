# Resolves the Cargo target directory used by the cursor-bridge build.
#
# Prints the path and nothing else, so callers can capture it:
#
#   $targetDir = & (Join-Path $PSScriptRoot 'cursor-bridge-target-dir.ps1')
#
# Resolution order:
#   1. `CODERELAY_CURSOR_TARGET_DIR`, when set — an explicit choice always wins.
#   2. `F:/target-cursor-bridge`, when an F: drive exists. This is where this
#      checkout builds, and it is kept as the default so an existing setup does
#      not silently move and force a full rebuild.
#   3. `sidecars/cursor-bridge/target`, when there is no F: drive. A hardcoded
#      `F:/` would make the build fail outright on such a machine, and Cargo
#      cannot read an environment variable from `.cargo/config.toml` to redirect
#      it, so the fallback has to be decided here. The location is inside the
#      checkout on purpose: `sidecars/cursor-bridge/.gitignore` already ignores
#      an anchored `/target/`, so the artifacts never reach the repository.
#
# The build script and every `verify-*.ps1` script resolve the directory through
# this file, so the binary that is built and the binary that is verified are
# always the same one.

$ErrorActionPreference = 'Stop'

if ($env:CODERELAY_CURSOR_TARGET_DIR) {
  $env:CODERELAY_CURSOR_TARGET_DIR
  return
}

if (Test-Path 'F:/') {
  'F:/target-cursor-bridge'
  return
}

$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Join-Path $root 'sidecars\cursor-bridge\target'

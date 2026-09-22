$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$bridge = Join-Path $root 'sidecars\cursor-bridge'
$hostLine = & rustc -vV | Select-String '^host:'
$targetTriple = $hostLine.ToString().Split(':', 2)[1].Trim()
if ([string]::IsNullOrWhiteSpace($targetTriple)) {
  throw 'Unable to determine the Rust target triple.'
}
$extension = ''
if ($targetTriple -match 'windows') {
  $extension = '.exe'
}

# The bridge has its own target directory on purpose: src-tauri/.cargo/config.toml
# pins the Tauri build to F:/target, and that setting is directory-scoped, so it
# does not apply here. Sharing one target directory between the two builds would
# make each one evict the other's artifacts.
#
# The location is resolved by scripts/cursor-bridge-target-dir.ps1 so the build
# and every verify script that looks for its output agree on it: an explicit
# CODERELAY_CURSOR_TARGET_DIR wins, F:/target-cursor-bridge is the default, and a
# machine without an F: drive falls back to the checkout-local
# sidecars/cursor-bridge/target (git-ignored).
$targetDir = & (Join-Path $PSScriptRoot 'cursor-bridge-target-dir.ps1')

Push-Location $bridge
try {
  cargo build --release --target-dir $targetDir -p coderelay-cursor-bridge
} finally {
  Pop-Location
}

$built = Join-Path $targetDir "release\cursor-bridge$extension"
if (-not (Test-Path $built)) {
  throw "cargo did not produce $built"
}

$bin = Join-Path $bridge "bin\cursor-bridge-$targetTriple$extension"
New-Item -ItemType Directory -Force (Split-Path $bin) | Out-Null
Copy-Item -Force $built $bin
Write-Host "Built $bin"

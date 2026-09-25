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

# `Copy-Item -Force` cannot be used here: Windows lets a running process keep its
# mapped image while the file is renamed, but it refuses to overwrite that image
# in place, so the copy fails with a sharing violation whenever the bridge is
# already running -- which is the normal case during `tauri:build`. Stage the new
# binary beside the target instead, then rename it into place; only when the
# target name is truly in use is that name moved aside first.
#
# The staging file has to sit in the target directory: F: -> H: is a different
# volume, and a cross-volume rename is a copy, which the running process blocks.
$staged = "$bin.new"
Remove-Item -Force -LiteralPath $staged -ErrorAction SilentlyContinue
Copy-Item -Force $built $staged
try {
  Move-Item -Force -LiteralPath $staged -Destination $bin -ErrorAction Stop
} catch {
  if (-not (Test-Path -LiteralPath $staged)) { throw }
  # The aside name carries a timestamp on purpose: a fixed name such as `~` is
  # taken by the running process's image, so from the second conflict onwards the
  # move-aside has nowhere to put the name and fails.
  $aside = "$bin.old-$(Get-Date -Format yyyyMMddHHmmss)"
  Move-Item -Force -LiteralPath $bin -Destination $aside -ErrorAction Stop
  Move-Item -Force -LiteralPath $staged -Destination $bin -ErrorAction Stop
}

# Each conflicting replacement above leaves one `<name>.old-<timestamp>` behind
# (an interrupted run can leave `<name>.new`, and older hand-rolled workflows
# left `<name>~`). Sweep them up best-effort: debris that an old process still
# maps cannot be deleted at all, and that must never fail the build. None of the
# patterns match the canonical binary, so it is never a candidate.
$binDir = Split-Path $bin
$binName = Split-Path $bin -Leaf
Get-ChildItem -LiteralPath $binDir -File -Force -ErrorAction SilentlyContinue |
  Where-Object { $_.Name -like "$binName.old-*" -or $_.Name -like "$binName.new" -or $_.Name -like "$binName~" } |
  Remove-Item -Force -ErrorAction SilentlyContinue

Write-Host "Built $bin"

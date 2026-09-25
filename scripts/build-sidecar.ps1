$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$sidecar = Join-Path $root 'sidecars\coderelay-proxy'
$hostLine = & rustc -vV | Select-String '^host:'
$targetTriple = $hostLine.ToString().Split(':', 2)[1].Trim()
if ([string]::IsNullOrWhiteSpace($targetTriple)) {
  throw 'Unable to determine the Rust target triple.'
}
$extension = ''
if ($targetTriple -match 'windows') {
  $extension = '.exe'
}
$bin = Join-Path $sidecar "bin\coderelay-proxy-$targetTriple$extension"
New-Item -ItemType Directory -Force (Split-Path $bin) | Out-Null
Push-Location $sidecar
try {
  go mod download
  go build -trimpath -ldflags "-s -w" -o $bin .
} finally {
  Pop-Location
}

# The Go linker replaces a running binary by renaming it aside to a single fixed
# `<name>~`, so at most one such leftover exists per binary. It reuses that name
# on the next conflicting build but leaves a stale one alone when the target is
# free, so sweep it here best-effort: a leftover whose old process is still
# running cannot be deleted at all, and that must never fail the build. The
# pattern cannot match the canonical binary, which is never a candidate.
$binDir = Split-Path $bin
$binName = Split-Path $bin -Leaf
Get-ChildItem -LiteralPath $binDir -File -Force -ErrorAction SilentlyContinue |
  Where-Object { $_.Name -like "$binName~" } |
  Remove-Item -Force -ErrorAction SilentlyContinue

Write-Host "Built $bin"
# Group 2 runtime verification: the bridge must self-terminate when the process
# that spawned it dies uncooperatively.
#
# Method: spawn a throwaway "fake parent" process, start the bridge with
# `--parent-pid <fake-parent-pid>`, force-kill the fake parent, then poll for the
# bridge to exit on its own. A control run repeats the same steps *without*
# `--parent-pid` and asserts the bridge stays alive, which is what proves the
# watchdog (and not something incidental) is doing the terminating.
#
# Note on process names: the deployed sidecar is named
# `cursor-bridge-x86_64-pc-windows-msvc.exe`, so matching processes by
# "cursor-bridge" does not find it. This script tracks the bridge by the PID
# Start-Process returns, never by name.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-parent-monitor.ps1
$ErrorActionPreference = 'Continue'

$targetDir = if ($env:CODERELAY_CURSOR_TARGET_DIR) { $env:CODERELAY_CURSOR_TARGET_DIR } else { 'F:/target-cursor-bridge' }
$exe = Join-Path $targetDir 'debug\cursor-bridge.exe'
if (-not (Test-Path $exe)) { throw "bridge debug binary not found: $exe" }

$root = Join-Path $env:TEMP ('cb-verify-parent-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $root | Out-Null

function Start-FakeParent([string]$tag) {
  # A powershell that just sleeps: it stays alive until force-killed, and has no
  # children that would outlive it.
  $script = Join-Path $root "parent-$tag.ps1"
  'Start-Sleep -Seconds 300' | Set-Content -Path $script -Encoding ASCII
  $p = Start-Process -FilePath 'powershell' -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $script) -PassThru -WindowStyle Hidden
  Start-Sleep -Milliseconds 1200
  return $p
}

function Wait-ForExit($proc, [int]$seconds) {
  $deadline = (Get-Date).AddSeconds($seconds)
  while ((Get-Date) -lt $deadline) {
    if ($proc.HasExited) { return $true }
    Start-Sleep -Milliseconds 500
  }
  return $false
}

function Run-Case([string]$tag, [bool]$withParentPid) {
  Write-Host "`n================ CASE: $tag (parent-pid=$withParentPid) ================"

  $dataDir = Join-Path $root "data-$tag"
  New-Item -ItemType Directory -Force $dataDir | Out-Null

  $parent = Start-FakeParent $tag
  Write-Host "FAKE_PARENT_PID=$($parent.Id)"

  $stdout = Join-Path $root "$tag.out.log"
  $stderr = Join-Path $root "$tag.err.log"

  # The bridge inherits APPDATA from this process; redirect it so a stray
  # settings write can never reach the developer's real Cursor configuration.
  $appData = Join-Path $root "appdata-$tag"
  New-Item -ItemType Directory -Force (Join-Path $appData 'Cursor\User') | Out-Null

  $previousAppData = $env:APPDATA
  $previousDataDir = $env:CODERELAY_CURSOR_DATA_DIR
  $previousListen = $env:CODERELAY_CURSOR_LISTEN_ADDR
  $env:APPDATA = $appData
  $env:CODERELAY_CURSOR_DATA_DIR = $dataDir
  # Port 0 lets the OS pick, so the two cases cannot collide with each other or
  # with anything already listening on the default 3000.
  $env:CODERELAY_CURSOR_LISTEN_ADDR = '127.0.0.1:0'

  # Start-Process rejects an empty ArgumentList array, so the control case
  # (which passes no arguments) must go through a different call shape. Getting
  # this wrong silently starts no process at all and makes the control result
  # meaningless.
  if ($withParentPid) {
    $bridge = Start-Process -FilePath $exe -ArgumentList @('--parent-pid', "$($parent.Id)") `
      -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -NoNewWindow
  } else {
    $bridge = Start-Process -FilePath $exe `
      -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -NoNewWindow
  }
  Write-Host "BRIDGE_PID=$($bridge.Id)"

  # Ready handshake first: the watchdog must not fire before serving.
  $ready = $null
  for ($i = 0; $i -lt 60; $i++) {
    Start-Sleep -Milliseconds 500
    if (Test-Path $stdout) {
      $text = Get-Content $stdout -Raw -ErrorAction SilentlyContinue
      if ($text -and $text.Trim().Length -gt 0) { $ready = $text.Trim(); break }
    }
  }
  Write-Host "READY_LINE=$ready"
  Write-Host "BRIDGE_ALIVE_BEFORE_KILL=$(-not $bridge.HasExited)"

  # Force-kill the parent: no shutdown hook, no cooperative exit.
  Write-Host "FORCE-KILLING FAKE PARENT $($parent.Id)"
  Stop-Process -Id $parent.Id -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 800
  Write-Host "FAKE_PARENT_EXITED=$($parent.HasExited)"

  # The watchdog polls for one second, so allow a few seconds of slack.
  $exitedWithin = Wait-ForExit $bridge 20
  Write-Host "BRIDGE_SELF_EXITED_WITHIN_20S=$exitedWithin"

  # These tail dumps must go through Write-Host: a bare Get-Content writes to
  # the function's output stream and would be captured into the return value
  # alongside the boolean, making the summary unreadable.
  Write-Host "--- bridge stderr tail (case $tag) ---"
  if (Test-Path $stderr) { Get-Content $stderr -Tail 6 | Write-Host }

  if (-not $exitedWithin) {
    Stop-Process -Id $bridge.Id -Force -ErrorAction SilentlyContinue
  }

  $env:APPDATA = $previousAppData
  $env:CODERELAY_CURSOR_DATA_DIR = $previousDataDir
  $env:CODERELAY_CURSOR_LISTEN_ADDR = $previousListen
  return $exitedWithin
}

$withWatchdog = Run-Case 'watchdog' $true
$withoutWatchdog = Run-Case 'control' $false

Write-Host "`n================ SUMMARY ================"
Write-Host "WITH --parent-pid   -> bridge self-exited: $withWatchdog   (expected True)"
Write-Host "WITHOUT --parent-pid -> bridge self-exited: $withoutWatchdog (expected False)"
Write-Host "ROOT_KEPT=$root"

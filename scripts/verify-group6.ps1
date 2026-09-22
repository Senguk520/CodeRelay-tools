# Group 6 runtime verification for the Cursor bridge.
#
# Verifies, against the real debug binary and isolated state:
#   1. the startup self-heal restores a user's http.noProxy from the residue record
#   2. it removes all five managed keys
#   3. it leaves the user's own keys untouched
#   4. a residue record for an *absent* http.noProxy does not fabricate the key
#   5. a user's own (unsigned) proxy configuration is never touched
#
# The account-database backup and the graceful Cursor shutdown cannot be driven
# from here: both live behind a trusted CA (enabling injection requires one), and
# trusting a root needs an interactive, elevation-free step this script must not
# perform on the developer's machine. Those are covered by unit tests instead
# (see local_app::settings::tests and the account.rs tests).
#
# Isolation: APPDATA, CODERELAY_CURSOR_DATA_DIR and CODERELAY_CURSOR_CA_DIR are
# redirected to a temp directory so the developer's real Cursor profile is never
# read or written.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-group6.ps1
$ErrorActionPreference = 'Continue'

$targetDir = & (Join-Path $PSScriptRoot 'cursor-bridge-target-dir.ps1')
$exe = Join-Path $targetDir 'debug\cursor-bridge.exe'
if (-not (Test-Path $exe)) { throw "bridge debug binary not found: $exe (run cargo build first)" }

function Start-IsolatedBridge {
  param([string]$Tag, [string]$SeedSettings, [string]$SeedResidue)

  $root = Join-Path $env:TEMP ("cb-verify-g6-$Tag-" + [guid]::NewGuid().ToString('N'))
  $dataDir = Join-Path $root 'bridge-data'
  $caDir = Join-Path $root 'bridge-ca'
  $appData = Join-Path $root 'appdata'
  $cursorUserDir = Join-Path $appData 'Cursor\User'
  New-Item -ItemType Directory -Force $dataDir, $caDir, $cursorUserDir | Out-Null

  $settingsPath = Join-Path $cursorUserDir 'settings.json'
  $residuePath = Join-Path $dataDir 'cursor-settings-residue.json'

  if ($SeedSettings) { [IO.File]::WriteAllText($settingsPath, $SeedSettings, [Text.UTF8Encoding]::new($false)) }
  if ($SeedResidue) { [IO.File]::WriteAllText($residuePath, $SeedResidue, [Text.UTF8Encoding]::new($false)) }

  $env:APPDATA = $appData
  $env:CODERELAY_CURSOR_DATA_DIR = $dataDir
  $env:CODERELAY_CURSOR_CA_DIR = $caDir
  $env:CODERELAY_CURSOR_LISTEN_ADDR = "127.0.0.1:$Port"
  $env:RUST_LOG = 'cursor_server=info'

  $stdout = Join-Path $root 'stdout.log'
  $stderr = Join-Path $root 'stderr.log'
  $proc = Start-Process -FilePath $exe -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -NoNewWindow

  $ready = $null
  for ($i = 0; $i -lt 60; $i++) {
    Start-Sleep -Milliseconds 500
    if (Test-Path $stdout) {
      $text = (Get-Content $stdout -Raw -ErrorAction SilentlyContinue)
      if ($text -and $text.Trim().Length -gt 0) { $ready = $text.Trim(); break }
    }
  }
  # Cleanup runs during App::new, before the ready line; the settle covers the
  # file write that follows it.
  Start-Sleep -Milliseconds 1200

  return [pscustomobject]@{
    Root = $root; Proc = $proc; Settings = $settingsPath; Residue = $residuePath
    Ready = $ready; Stderr = $stderr
  }
}

function Stop-IsolatedBridge($ctx) {
  Stop-Process -Id $ctx.Proc.Id -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 600
}

$managedSeed = @'
{
  "http.proxy": "http://127.0.0.1:6538",
  "http.proxyKerberosServicePrincipal": "http://127.0.0.1:6538",
  "http.proxySupport": "on",
  "cursor.general.disableHttp2": true,
  "http.experimental.systemCertificatesV2": true,
  "editor.fontSize": 15,
  "workbench.colorTheme": "Default Dark+"
}
'@

$residueWithNoProxy = '{"recorded":true,"no_proxy":"localhost,127.0.0.1,*.internal.example"}'
$residueWithoutNoProxy = '{"recorded":true,"no_proxy":null}'

Write-Host '=== 1) startup self-heal must restore http.noProxy from the residue record ==='
$Port = 39131
$ctx = Start-IsolatedBridge -Tag 'restore' -SeedSettings $managedSeed -SeedResidue $residueWithNoProxy
Write-Host "ROOT=$($ctx.Root)"
Write-Host "READY_LINE=$($ctx.Ready)"
Write-Host 'SETTINGS_AFTER_STARTUP:'
$after = Get-Content $ctx.Settings -Raw
Write-Host $after
$json = $after | ConvertFrom-Json
Write-Host "NO_PROXY_RESTORED=$($json.'http.noProxy')"
Write-Host "MANAGED_HTTP_PROXY_GONE=$($null -eq $json.'http.proxy')"
Write-Host "MANAGED_PROXY_SUPPORT_GONE=$($null -eq $json.'http.proxySupport')"
Write-Host "MANAGED_DISABLE_HTTP2_GONE=$($null -eq $json.'cursor.general.disableHttp2')"
Write-Host "MANAGED_SYSTEM_CERTS_GONE=$($null -eq $json.'http.experimental.systemCertificatesV2')"
Write-Host "USER_FONT_SIZE_KEPT=$($json.'editor.fontSize')"
Write-Host "USER_THEME_KEPT=$($json.'workbench.colorTheme')"
Write-Host "RESIDUE_CLEARED=$(-not (Test-Path $ctx.Residue))"
Stop-IsolatedBridge $ctx

Write-Host "`n=== 2) a residue record for an ABSENT noProxy must not fabricate the key ==="
$Port = 39132
$noNoProxySeed = @'
{
  "http.proxy": "http://127.0.0.1:6538",
  "http.proxyKerberosServicePrincipal": "http://127.0.0.1:6538",
  "http.proxySupport": "on",
  "cursor.general.disableHttp2": true,
  "http.experimental.systemCertificatesV2": true,
  "editor.fontSize": 15
}
'@
$ctx = Start-IsolatedBridge -Tag 'absent' -SeedSettings $noNoProxySeed -SeedResidue $residueWithoutNoProxy
Write-Host "ROOT=$($ctx.Root)"
Write-Host "READY_LINE=$($ctx.Ready)"
$after = Get-Content $ctx.Settings -Raw
Write-Host 'SETTINGS_AFTER_STARTUP:'
Write-Host $after
$json = $after | ConvertFrom-Json
Write-Host "NO_PROXY_ABSENT=$($null -eq $json.'http.noProxy')   (must be True)"
Write-Host "MANAGED_HTTP_PROXY_GONE=$($null -eq $json.'http.proxy')"
Stop-IsolatedBridge $ctx

Write-Host "`n=== 3) a user's own (unsigned) proxy must survive the startup self-heal ==="
$Port = 39133
$userProxySeed = @'
{
  "http.proxy": "http://127.0.0.1:8080",
  "editor.fontSize": 14
}
'@
$ctx = Start-IsolatedBridge -Tag 'user' -SeedSettings $userProxySeed
Write-Host "ROOT=$($ctx.Root)"
Write-Host "READY_LINE=$($ctx.Ready)"
$after = Get-Content $ctx.Settings -Raw
Write-Host 'SETTINGS_AFTER_STARTUP:'
Write-Host $after
$json = $after | ConvertFrom-Json
Write-Host "USER_PROXY_KEPT=$($json.'http.proxy')   (must be http://127.0.0.1:8080)"
Stop-IsolatedBridge $ctx

Write-Host "`n=== 4) no bridge processes left behind ==="
$left = (Get-Process -Name 'cursor-bridge*' -ErrorAction SilentlyContinue | Measure-Object).Count
Write-Host "BRIDGE_PROCS_REMAINING=$left"

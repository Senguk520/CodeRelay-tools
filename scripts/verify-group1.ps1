# Group 1 / Group 4 runtime verification for the Cursor bridge.
#
# Verifies, against the real debug binary and isolated state:
#   1. a fresh database reports takeover_requested = false
#   2. GET status is read-only: it does not terminate Cursor and writes no settings
#   3. DELETE /harness/cursor/injection exists and is idempotent
#   4. the clear path removes only the five managed keys and keeps user keys
#   5. the http.noProxy original value survives the clear
#
# Isolation: APPDATA and CODERELAY_CURSOR_DATA_DIR are redirected to a temp
# directory so the developer's real %APPDATA%\Cursor\User\settings.json is never
# read or written. The temp root is printed so it can be inspected afterwards.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-group1.ps1
$ErrorActionPreference = 'Continue'

# The bridge keeps its own target directory (see scripts/build-cursor-bridge.ps1).
$targetDir = if ($env:CODERELAY_CURSOR_TARGET_DIR) { $env:CODERELAY_CURSOR_TARGET_DIR } else { 'F:/target-cursor-bridge' }
$exe = Join-Path $targetDir 'debug\cursor-bridge.exe'
if (-not (Test-Path $exe)) { throw "bridge debug binary not found: $exe (run cargo build first)" }

$root = Join-Path $env:TEMP ('cb-verify-g1-' + [guid]::NewGuid().ToString('N'))
$dataDir = Join-Path $root 'bridge-data'
$appData = Join-Path $root 'appdata'
$cursorUserDir = Join-Path $appData 'Cursor\User'
New-Item -ItemType Directory -Force $dataDir, $cursorUserDir | Out-Null

$settingsPath = Join-Path $cursorUserDir 'settings.json'

$env:APPDATA = $appData
$env:CODERELAY_CURSOR_DATA_DIR = $dataDir
$env:CODERELAY_CURSOR_LISTEN_ADDR = '127.0.0.1:39118'
$env:RUST_LOG = 'cursor_server=info'

$stdout = Join-Path $root 'stdout.log'
$stderr = Join-Path $root 'stderr.log'

Write-Host "ROOT=$root"
Write-Host "EXE=$exe"

$proc = Start-Process -FilePath $exe -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -NoNewWindow
Write-Host "PID=$($proc.Id)"

# --- ready handshake: one JSON object per line on stdout ---
$ready = $null
for ($i = 0; $i -lt 60; $i++) {
  Start-Sleep -Milliseconds 500
  if (Test-Path $stdout) {
    $text = (Get-Content $stdout -Raw -ErrorAction SilentlyContinue)
    if ($text -and $text.Trim().Length -gt 0) { $ready = $text.Trim(); break }
  }
}
Write-Host "READY_LINE=$ready"
if (-not $ready) {
  Write-Host 'BRIDGE DID NOT ANNOUNCE READY'
  if (Test-Path $stderr) { Get-Content $stderr -Tail 20 }
  Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
  exit 1
}

$base = 'http://127.0.0.1:39118'

function Invoke-Probe([string]$method, [string]$url, [string]$body) {
  try {
    $p = @{ Uri = $url; Method = $method; TimeoutSec = 15; UseBasicParsing = $true }
    if ($body) { $p.Body = $body; $p.ContentType = 'application/json' }
    $r = Invoke-WebRequest @p
    return @{ Code = $r.StatusCode; Body = $r.Content }
  } catch {
    $resp = $_.Exception.Response
    if ($resp) {
      $code = [int]$resp.StatusCode
      $reader = New-Object System.IO.StreamReader($resp.GetResponseStream())
      return @{ Code = $code; Body = $reader.ReadToEnd() }
    }
    return @{ Code = 'ERR'; Body = $_.Exception.Message }
  }
}

$statusUrl = "$base/__byok-api__/api/harness/cursor/status"

Write-Host "`n=== 1) GET status on a fresh database (takeover_requested must be false) ==="
$r = Invoke-Probe 'GET' $statusUrl
Write-Host "HTTP $($r.Code)"
Write-Host $r.Body

Write-Host "`n=== 2) GET status x3 must be read-only (Cursor process count unchanged) ==="
$before = (Get-Process -Name Cursor -ErrorAction SilentlyContinue | Measure-Object).Count
for ($i = 1; $i -le 3; $i++) { $null = Invoke-Probe 'GET' $statusUrl }
Start-Sleep -Milliseconds 800
$after = (Get-Process -Name Cursor -ErrorAction SilentlyContinue | Measure-Object).Count
Write-Host "CURSOR_PROCS_BEFORE=$before AFTER_THREE_STATUS_READS=$after"

Write-Host "`n=== 3) read-only check: no settings.json was created by the status reads ==="
Write-Host "SETTINGS_EXISTS_BEFORE_SEED=$(Test-Path $settingsPath)"

# --- seed a realistic Cursor settings.json: managed residual + user keys ---
@'
{
  "http.proxy": "http://127.0.0.1:6538",
  "http.proxyKerberosServicePrincipal": "http://127.0.0.1:6538",
  "http.proxySupport": "on",
  "cursor.general.disableHttp2": true,
  "http.experimental.systemCertificatesV2": true,
  "http.noProxy": "localhost,127.0.0.1,*.internal.example",
  "editor.fontSize": 15,
  "workbench.colorTheme": "Default Dark+"
}
'@ | Set-Content -Path $settingsPath -Encoding UTF8
Write-Host "SEEDED=$settingsPath"

Write-Host "`n=== 4) DELETE /harness/cursor/injection (new endpoint) ==="
$injectionUrl = "$base/__byok-api__/api/harness/cursor/injection"
$r = Invoke-Probe 'DELETE' $injectionUrl
Write-Host "HTTP $($r.Code)"
Write-Host $r.Body

Write-Host "`n=== 5) DELETE again (idempotent) ==="
$r = Invoke-Probe 'DELETE' $injectionUrl
Write-Host "HTTP $($r.Code)"
Write-Host $r.Body

Write-Host "`n=== 6) settings.json after clear (managed keys gone, user keys kept) ==="
Get-Content $settingsPath -Raw

Write-Host "`n=== 7) PUT enabled=true must fail while the CA is missing ==="
$r = Invoke-Probe 'PUT' "$base/__byok-api__/api/harness/cursor/enabled" '{"enabled":true}'
Write-Host "HTTP $($r.Code)"
Write-Host $r.Body

Write-Host "`n=== 8) takeover row in the isolated database ==="
$db = Join-Path $dataDir 'cursor-bridge.db'
Write-Host "DB_EXISTS=$(Test-Path $db)"
$python = Get-Command python -ErrorAction SilentlyContinue
if ($python) {
  $pyFile = Join-Path $root 'dump_rows.py'
  @'
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
rows = conn.execute(
    "SELECT setting_key, value_json FROM service_settings WHERE setting_key LIKE '%takeover%'"
).fetchall()
print('TAKEOVER_ROWS=', rows)
'@ | Set-Content -Path $pyFile -Encoding UTF8
  & python $pyFile $db 2>&1
} else {
  Write-Host 'python unavailable; skipping row dump'
}

Write-Host "`n=== stderr tail ==="
if (Test-Path $stderr) { Get-Content $stderr -Tail 8 }

Write-Host "`n=== cleanup ==="
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800
Write-Host "HAS_EXITED=$($proc.HasExited)"
Write-Host "ROOT_KEPT=$root"

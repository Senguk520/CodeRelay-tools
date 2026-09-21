# Group 5 runtime verification for the Cursor bridge.
#
# Verifies, against the real debug binary and isolated state:
#   1. a model API key is sealed at rest -- the SQLite column carries the
#      `coderelay-protected:1:` prefix and the plaintext canary is absent
#   2. the outbound proxy password is sealed at rest the same way, and
#      GET /api/settings/proxy reports `has_password` instead of the value
#   3. the CA now lives in its own directory (CODERELAY_CURSOR_CA_DIR), not
#      inside the bridge data directory
#   4. a CA left in the legacy <data dir>/ca location is migrated, not regenerated
#   5. the generated CA carries NameConstraints for cursor.sh / .cursor.sh
#   6. harness status exposes a matching ca_uninstall_command
#
# Isolation: APPDATA, CODERELAY_CURSOR_DATA_DIR and CODERELAY_CURSOR_CA_DIR are
# redirected to a temp directory so the developer's real state is never touched.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-group5.ps1
$ErrorActionPreference = 'Continue'

$targetDir = if ($env:CODERELAY_CURSOR_TARGET_DIR) { $env:CODERELAY_CURSOR_TARGET_DIR } else { 'F:/target-cursor-bridge' }
$exe = Join-Path $targetDir 'debug\cursor-bridge.exe'
if (-not (Test-Path $exe)) { throw "bridge debug binary not found: $exe (run cargo build first)" }

$root = Join-Path $env:TEMP ('cb-verify-g5-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $root | Out-Null
$env:APPDATA = Join-Path $root 'appdata'
New-Item -ItemType Directory -Force $env:APPDATA | Out-Null
$env:RUST_LOG = 'cursor_server=info'

$token = 'verify-group5-token-4c7e'
$env:CODERELAY_CURSOR_CONTROL_TOKEN = $token

Write-Host "ROOT=$root"
Write-Host "EXE=$exe"

function Start-Bridge([string]$label, [string]$dataDir, [string]$caDir) {
  $stdout = Join-Path $root "$label.stdout.log"
  $stderr = Join-Path $root "$label.stderr.log"
  New-Item -ItemType Directory -Force $dataDir, $caDir | Out-Null
  $env:CODERELAY_CURSOR_DATA_DIR = $dataDir
  $env:CODERELAY_CURSOR_CA_DIR = $caDir
  $env:CODERELAY_CURSOR_LISTEN_ADDR = '127.0.0.1:0'

  # stderr goes to a FILE, never to an undrained pipe: an unread redirected pipe
  # deadlocks the child once its log buffer fills.
  $proc = Start-Process -FilePath $exe -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -NoNewWindow

  $ready = $null
  for ($i = 0; $i -lt 60; $i++) {
    Start-Sleep -Milliseconds 400
    if ($proc.HasExited) { break }
    if (Test-Path $stdout) {
      $text = (Get-Content $stdout -Raw -ErrorAction SilentlyContinue)
      if ($text -and $text.Trim().Length -gt 0) { $ready = $text.Trim(); break }
    }
  }
  $port = $null
  if ($ready -match '"port":(\d+)') { $port = [int]$Matches[1] }
  return @{ Proc = $proc; Port = $port; ReadyLine = $ready; Stdout = $stdout; Stderr = $stderr; DataDir = $dataDir; CaDir = $caDir }
}

function Stop-Bridge($b) {
  if ($b -and $b.Proc) {
    Stop-Process -Id $b.Proc.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
  }
}

function Invoke-Probe([string]$method, [string]$url, [string]$body) {
  $headers = @{ Authorization = "Bearer $token" }
  try {
    $p = @{ Uri = $url; Method = $method; TimeoutSec = 30; UseBasicParsing = $true; Headers = $headers }
    if ($body) { $p.Body = $body; $p.ContentType = 'application/json' }
    $r = Invoke-WebRequest @p
    return @{ Code = [int]$r.StatusCode; Body = $r.Content }
  } catch {
    $resp = $_.Exception.Response
    if ($resp) {
      $reader = New-Object System.IO.StreamReader($resp.GetResponseStream())
      return @{ Code = [int]$resp.StatusCode; Body = $reader.ReadToEnd() }
    }
    return @{ Code = 'ERR'; Body = $_.Exception.Message }
  }
}

# Reads one scalar out of the isolated SQLite file. Uses python (present on this
# machine); if it is missing the check is reported as skipped, never as passing.
function Read-Sql([string]$db, [string]$query) {
  $py = Get-Command python -ErrorAction SilentlyContinue
  if (-not $py) { return '<python unavailable>' }
  $script = Join-Path $root 'probe.py'
  @'
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
for row in conn.execute(sys.argv[2]).fetchall():
    print(row)
'@ | Set-Content -Path $script -Encoding UTF8
  return (& python $script $db $query 2>&1 | Out-String).Trim()
}

# ---------------------------------------------------------------------------
Write-Host "`n=== 1) start the bridge in the new layout ==="
$dataDir = Join-Path $root 'data'
$caDir = Join-Path $root 'ca'
$b = Start-Bridge 'main' $dataDir $caDir
Write-Host "READY_LINE=$($b.ReadyLine)"
if (-not $b.Port) {
  Write-Host 'BRIDGE DID NOT START'
  Get-Content $b.Stderr -Tail 30 -ErrorAction SilentlyContinue
  Stop-Bridge $b
  exit 1
}
$base = "http://127.0.0.1:$($b.Port)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 2) a model API key must be sealed at rest ==="
$canary = 'sk-verify-atrest-canary-9d14ab'
$model = @{
  display_name = 'At-rest Model'
  type         = 'openai'
  base_url     = 'https://provider.example/v1/chat/completions'
  use_full_url = $true
  api_key      = $canary
  tooltip_data = 'at-rest'
  model_id     = 'at-rest-model'
} | ConvertTo-Json -Compress
$r = Invoke-Probe 'POST' "$base/__byok-api__/api/models" "{`"models`":[$model]}"
Write-Host "POST models -> HTTP $($r.Code)"

$db = Join-Path $dataDir 'cursor-bridge.db'
Write-Host "DB_EXISTS=$(Test-Path $db)"
Write-Host "RAW_COLUMN=$(Read-Sql $db "SELECT api_key FROM model_configs WHERE model_id='at-rest-model'")"
Write-Host "PLAINTEXT_CANARY_IN_DB=$(Read-Sql $db "SELECT COUNT(*) FROM model_configs WHERE api_key LIKE '%$canary%'")   (must be 0)"

# Defensive: the WAL holds the same bytes as the page cache, so check it too.
# Deferred to step 8: the files are exclusively locked while the bridge runs, and
# a read that fails must not be reported as "no plaintext found".

$r = Invoke-Probe 'GET' "$base/__byok-api__/api/models" $null
Write-Host "GET models HAS_CLEARTEXT_API_KEY_FIELD=$($r.Body -match '"api_key":')   (must be False)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 3) the proxy password must be sealed at rest and never echoed ==="
$proxyPassword = 'proxy-verify-canary-71b3'
$proxy = @{
  mode         = 'custom'
  address      = 'http://127.0.0.1:7890'
  auth_enabled = $true
  username     = 'verify-user'
  password     = $proxyPassword
} | ConvertTo-Json -Compress
$r = Invoke-Probe 'PUT' "$base/__byok-api__/api/settings/proxy" $proxy
Write-Host "PUT proxy -> HTTP $($r.Code)"
Write-Host "PUT proxy body -> $($r.Body)"

Write-Host "PROXY_ROW=$(Read-Sql $db "SELECT value_json FROM service_settings WHERE setting_key='outbound_proxy'")"
Write-Host "PROXY_PLAINTEXT_IN_DB=$(Read-Sql $db "SELECT COUNT(*) FROM service_settings WHERE value_json LIKE '%$proxyPassword%'")   (must be 0)"

$r = Invoke-Probe 'GET' "$base/__byok-api__/api/settings/proxy" $null
Write-Host "GET proxy -> HTTP $($r.Code)"
Write-Host "GET proxy body -> $($r.Body)"
Write-Host "PASSWORD_ECHOED_IN_GET=$($r.Body -like "*$proxyPassword*")   (must be False)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 4) the CA must live outside the data directory ==="
Write-Host "CA_DIR_IS_SEPARATE=$($caDir -ne $dataDir)"
Write-Host "LEGACY_CA_IN_DATA_DIR_BEFORE=$(Test-Path (Join-Path $dataDir 'ca'))   (must be False)"

$r = Invoke-Probe 'POST' "$base/__byok-api__/api/harness/cursor/ca/initialize" ''
Write-Host "POST ca/initialize -> HTTP $($r.Code)"

Write-Host "CA_CRT_IN_CA_DIR=$(Test-Path (Join-Path $caDir 'ca.crt'))   (must be True)"
Write-Host "CA_KEY_IN_CA_DIR=$(Test-Path (Join-Path $caDir 'ca.key'))   (must be True)"
Write-Host "CA_ANYTHING_IN_DATA_DIR=$(Test-Path (Join-Path $dataDir 'ca'))   (must be False)"

$keyBytes = [System.IO.File]::ReadAllBytes((Join-Path $caDir 'ca.key'))
$keyText = [System.Text.Encoding]::UTF8.GetString($keyBytes)
Write-Host "CA_KEY_IS_SEALED=$($keyText.StartsWith('coderelay-protected:1:'))   (must be True)"
Write-Host "CA_KEY_IS_RAW_PEM=$($keyText.StartsWith('-----BEGIN'))   (must be False)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 5) the CA certificate must carry NameConstraints for cursor.sh ==="
$crt = Join-Path $caDir 'ca.crt'
$openssl = (Get-Command openssl -ErrorAction SilentlyContinue).Source
if (-not $openssl) { $openssl = 'G:\msys2\mingw64\bin\openssl.exe' }
if (Test-Path $openssl) {
  $dump = & $openssl x509 -in $crt -noout -text 2>&1 | Out-String
  $nc = ($dump -split "`n" | Select-String -Pattern 'Name Constraints' -Context 0,6) | Out-String
  Write-Host $nc.Trim()
  Write-Host "HAS_NAME_CONSTRAINTS=$($dump -match 'Name Constraints')   (must be True)"
  Write-Host "HAS_PERMITTED_DNS=$($dump -match 'DNS:\.?cursor\.sh')   (must be True)"
} else {
  Write-Host 'openssl unavailable; NameConstraints check SKIPPED (not verified)'
}

# ---------------------------------------------------------------------------
Write-Host "`n=== 6) harness status must expose an uninstall command ==="
$r = Invoke-Probe 'GET' "$base/__byok-api__/api/harness/cursor/status" $null
Write-Host "GET status -> HTTP $($r.Code)"
Write-Host $r.Body
Write-Host "HAS_UNINSTALL_COMMAND=$($r.Body -match 'ca_uninstall_command')   (must be True)"

Stop-Bridge $b

# ---------------------------------------------------------------------------
# Run only after the process is gone: SQLite holds these files open, and a read
# that throws must never be reported as "no plaintext found".
Write-Host "`n=== 8) plaintext scan of every bridge database file (process stopped) ==="
foreach ($suffix in @('', '-wal', '-shm')) {
  $file = "$db$suffix"
  if (-not (Test-Path $file)) { Write-Host "SKIP $file (absent)"; continue }
  try {
    $bytes = [System.IO.File]::ReadAllBytes($file)
  } catch {
    Write-Host "CANARY_SCAN_FAILED=$file ($($_.Exception.Message))  -- UNVERIFIED, not a pass"
    continue
  }
  $text = [System.Text.Encoding]::UTF8.GetString($bytes)
  $bytes = $null
  Write-Host "SCANNED=$file bytes_in_file_contains_canary=$($text.Contains($canary))   (must be False)"
}

# ---------------------------------------------------------------------------
Write-Host "`n=== 7) a legacy <data dir>/ca must be migrated, not regenerated ==="
$dataDir2 = Join-Path $root 'data-legacy'
$caDir2 = Join-Path $root 'ca-legacy'
$legacyCa = Join-Path $dataDir2 'ca'
New-Item -ItemType Directory -Force $legacyCa, $caDir2 | Out-Null

# Generate a marker CA in the legacy location via a first run, then move it.
$pre = Start-Bridge 'legacygen' $dataDir2 $caDir2
if ($pre.Port) {
  $null = Invoke-Probe 'POST' "http://127.0.0.1:$($pre.Port)/__byok-api__/api/harness/cursor/ca/initialize" ''
  Stop-Bridge $pre
}
$fingerprintBefore = if (Test-Path (Join-Path $caDir2 'ca.crt')) {
  (Get-FileHash (Join-Path $caDir2 'ca.crt') -Algorithm SHA256).Hash
} else { '<none>' }

# Put it back where an older build left it, and clear the new location.
New-Item -ItemType Directory -Force $legacyCa | Out-Null
Move-Item -Force (Join-Path $caDir2 'ca.crt') (Join-Path $legacyCa 'ca.crt')
Move-Item -Force (Join-Path $caDir2 'ca.key') (Join-Path $legacyCa 'ca.key')
Write-Host "LEGACY_SEEDED_CRT=$(Test-Path (Join-Path $legacyCa 'ca.crt'))"
Write-Host "NEW_LOCATION_EMPTY_BEFORE=$(Test-Path (Join-Path $caDir2 'ca.crt'))"

$post = Start-Bridge 'legacymigrate' $dataDir2 $caDir2
Write-Host "READY_LINE=$($post.ReadyLine)"
$fingerprintAfter = if (Test-Path (Join-Path $caDir2 'ca.crt')) {
  (Get-FileHash (Join-Path $caDir2 'ca.crt') -Algorithm SHA256).Hash
} else { '<none>' }
Write-Host "FINGERPRINT_BEFORE=$fingerprintBefore"
Write-Host "FINGERPRINT_AFTER =$fingerprintAfter"
Write-Host "MIGRATED_SAME_CERT=$($fingerprintBefore -eq $fingerprintAfter)   (must be True: a re-generated CA would change this)"
Write-Host "LEGACY_DIR_REMAINS=$(Test-Path (Join-Path $dataDir2 'ca'))   (must be False once fully moved)"
Stop-Bridge $post

Write-Host "`n=== stderr tail (main) ==="
Get-Content (Join-Path $root 'main.stderr.log') -Tail 10 -ErrorAction SilentlyContinue

Write-Host "`nROOT_KEPT=$root"

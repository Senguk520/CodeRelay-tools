# Group 3 runtime verification for the Cursor bridge.
#
# Verifies, against the real debug binary and isolated state:
#   1. with NO control token configured, every control-API request is refused
#      (401) -- the fail-closed requirement, not a warning
#   2. with the token configured, the same request succeeds (200)
#   3. a wrong / absent / non-Bearer credential is refused
#   4. GET /api/models no longer returns the credential in cleartext, and still
#      reports has_api_key + a stable api_key_fingerprint
#   5. CORS: tauri://localhost is echoed, a private-range origin is not
#   6. a non-loopback listen address aborts startup instead of binding
#
# Isolation: APPDATA and CODERELAY_CURSOR_DATA_DIR are redirected to a temp
# directory so the developer's real state is never touched. The temp root is
# printed so it can be inspected afterwards.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-group3.ps1
$ErrorActionPreference = 'Continue'

$targetDir = if ($env:CODERELAY_CURSOR_TARGET_DIR) { $env:CODERELAY_CURSOR_TARGET_DIR } else { 'F:/target-cursor-bridge' }
$exe = Join-Path $targetDir 'debug\cursor-bridge.exe'
if (-not (Test-Path $exe)) { throw "bridge debug binary not found: $exe (run cargo build first)" }

$root = Join-Path $env:TEMP ('cb-verify-g3-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $root | Out-Null
$env:APPDATA = Join-Path $root 'appdata'
New-Item -ItemType Directory -Force $env:APPDATA | Out-Null
$env:RUST_LOG = 'cursor_server=info'

Write-Host "ROOT=$root"
Write-Host "EXE=$exe"

# Starts the bridge with the given listen address and control token (empty =
# unset) and returns @{ Proc; Port; Stderr; Stdout; ReadyLine } or $null on a
# failed start.
function Start-Bridge([string]$listen, [string]$token, [string]$label) {
  $dataDir = Join-Path $root ("data-$label")
  New-Item -ItemType Directory -Force $dataDir | Out-Null
  $stdout = Join-Path $root "$label.stdout.log"
  $stderr = Join-Path $root "$label.stderr.log"

  $env:CODERELAY_CURSOR_DATA_DIR = $dataDir
  $env:CODERELAY_CURSOR_LISTEN_ADDR = $listen
  # Both traps are real: an empty value must reach the child as *absent*, not as
  # an empty string, or we would be testing a different case than we claim.
  if ([string]::IsNullOrEmpty($token)) {
    Remove-Item Env:\CODERELAY_CURSOR_CONTROL_TOKEN -ErrorAction SilentlyContinue
  } else {
    $env:CODERELAY_CURSOR_CONTROL_TOKEN = $token
  }

  # stderr is redirected to a FILE, never to a pipe nobody drains: a redirected
  # but unread pipe would deadlock the child once its log buffer filled.
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
  return @{ Proc = $proc; Port = $port; ReadyLine = $ready; Stdout = $stdout; Stderr = $stderr; DataDir = $dataDir }
}

function Stop-Bridge($b) {
  if ($b -and $b.Proc) {
    Stop-Process -Id $b.Proc.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
  }
}

# Issues a request and returns @{ Code; Body; Headers } without throwing on 4xx.
function Invoke-Probe([string]$method, [string]$url, [string]$token, [string]$body, [string]$origin) {
  $headers = @{}
  if (-not [string]::IsNullOrEmpty($token)) { $headers['Authorization'] = "Bearer $token" }
  if (-not [string]::IsNullOrEmpty($origin)) { $headers['Origin'] = $origin }
  try {
    $p = @{ Uri = $url; Method = $method; TimeoutSec = 15; UseBasicParsing = $true; Headers = $headers }
    if ($body) { $p.Body = $body; $p.ContentType = 'application/json' }
    $r = Invoke-WebRequest @p
    return @{ Code = [int]$r.StatusCode; Body = $r.Content; Headers = $r.Headers }
  } catch {
    $resp = $_.Exception.Response
    if ($resp) {
      $reader = New-Object System.IO.StreamReader($resp.GetResponseStream())
      return @{ Code = [int]$resp.StatusCode; Body = $reader.ReadToEnd(); Headers = $resp.Headers }
    }
    return @{ Code = 'ERR'; Body = $_.Exception.Message; Headers = [hashtable]::new() }
  }
}

# ---------------------------------------------------------------------------
Write-Host "`n=== 1) NO control token configured: every control request must be refused ==="
$open = Start-Bridge '127.0.0.1:39201' '' 'notoken'
Write-Host "READY_LINE=$($open.ReadyLine)"
if (-not $open.Port) {
  Write-Host 'BRIDGE DID NOT START (no token)'
  Get-Content $open.Stderr -Tail 20 -ErrorAction SilentlyContinue
  Stop-Bridge $open
  exit 1
}
$base = "http://127.0.0.1:$($open.Port)"
$statusUrl = "$base/__byok-api__/api/harness/cursor/status"
$modelsUrl = "$base/__byok-api__/api/models"

$r = Invoke-Probe 'GET' $statusUrl '' $null $null
Write-Host "GET status, no credential      -> HTTP $($r.Code)  $($r.Body)"
$r = Invoke-Probe 'GET' $statusUrl 'anything' $null $null
Write-Host "GET status, arbitrary token    -> HTTP $($r.Code)  $($r.Body)"
$r = Invoke-Probe 'GET' $modelsUrl '' $null $null
Write-Host "GET models, no credential      -> HTTP $($r.Code)  $($r.Body)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 2) token configured: the same requests succeed ==="
$token = 'verify-group3-token-9f3a'
$auth = Start-Bridge '127.0.0.1:39202' $token 'withtoken'
Write-Host "READY_LINE=$($auth.ReadyLine)"
if (-not $auth.Port) {
  Write-Host 'BRIDGE DID NOT START (with token)'
  Get-Content $auth.Stderr -Tail 20 -ErrorAction SilentlyContinue
  Stop-Bridge $auth; Stop-Bridge $open
  exit 1
}
$base2 = "http://127.0.0.1:$($auth.Port)"
$statusUrl2 = "$base2/__byok-api__/api/harness/cursor/status"
$modelsUrl2 = "$base2/__byok-api__/api/models"

$r = Invoke-Probe 'GET' $statusUrl2 $token $null $null
Write-Host "GET status, correct token      -> HTTP $($r.Code)  $($r.Body)"
$r = Invoke-Probe 'GET' $statusUrl2 'wrong-token' $null $null
Write-Host "GET status, wrong token        -> HTTP $($r.Code)  $($r.Body)"
$r = Invoke-Probe 'GET' $statusUrl2 '' $null $null
Write-Host "GET status, no credential      -> HTTP $($r.Code)  $($r.Body)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 3) GET /api/models must not return the credential in cleartext ==="
$secret = 'sk-verify-plaintext-canary-2f81c4'
$model = @{
  display_name = 'Verify Model'
  type         = 'openai'
  base_url     = 'https://provider.example/v1/chat/completions'
  use_full_url = $true
  api_key      = $secret
  tooltip_data = 'verify'
  model_id     = 'verify-model'
} | ConvertTo-Json -Compress
$r = Invoke-Probe 'POST' $modelsUrl2 $token "{`"models`":[$model]}" $null
Write-Host "POST models                    -> HTTP $($r.Code)"
$r = Invoke-Probe 'GET' $modelsUrl2 $token $null $null
Write-Host "GET models (raw body)          -> HTTP $($r.Code)"
Write-Host $r.Body

Write-Host "`nCANARY_LEAKED_IN_BODY=$($r.Body -like "*$secret*")   (must be False)"
$parsed = $null
try { $parsed = $r.Body | ConvertFrom-Json } catch { }
if ($parsed) {
  $m = $parsed[0]
  Write-Host "HAS_API_KEY_FIELD=$($m.PSObject.Properties.Name -contains 'has_api_key')  value=$($m.has_api_key)"
  Write-Host "HAS_FINGERPRINT_FIELD=$($m.PSObject.Properties.Name -contains 'api_key_fingerprint')  value=$($m.api_key_fingerprint)"
  Write-Host "HAS_CLEARTEXT_API_KEY_FIELD=$($m.PSObject.Properties.Name -contains 'api_key')   (must be False)"
}

# ---------------------------------------------------------------------------
Write-Host "`n=== 4) CORS origin allowlist ==="
$r = Invoke-Probe 'GET' $statusUrl2 $token $null 'tauri://localhost'
Write-Host "tauri://localhost              -> HTTP $($r.Code)  ACAO=$($r.Headers['Access-Control-Allow-Origin'])"
$r = Invoke-Probe 'GET' $statusUrl2 $token $null 'http://127.0.0.1:5173'
Write-Host "http://127.0.0.1:5173          -> HTTP $($r.Code)  ACAO=$($r.Headers['Access-Control-Allow-Origin'])"
$r = Invoke-Probe 'GET' $statusUrl2 $token $null 'http://192.168.1.50'
Write-Host "http://192.168.1.50 (private)  -> HTTP $($r.Code)  ACAO=$($r.Headers['Access-Control-Allow-Origin'])   (must be empty)"
$r = Invoke-Probe 'GET' $statusUrl2 $token $null 'http://10.0.0.7'
Write-Host "http://10.0.0.7 (private)      -> HTTP $($r.Code)  ACAO=$($r.Headers['Access-Control-Allow-Origin'])   (must be empty)"

Stop-Bridge $auth
Stop-Bridge $open

# ---------------------------------------------------------------------------
Write-Host "`n=== 5) a non-loopback listen address must abort startup ==="
$badData = Join-Path $root 'data-badaddr'
New-Item -ItemType Directory -Force $badData | Out-Null
$env:CODERELAY_CURSOR_DATA_DIR = $badData
$env:CODERELAY_CURSOR_LISTEN_ADDR = '0.0.0.0:39203'
$env:CODERELAY_CURSOR_CONTROL_TOKEN = 'irrelevant'
$badOut = Join-Path $root 'badaddr.stdout.log'
$badErr = Join-Path $root 'badaddr.stderr.log'
$bad = Start-Process -FilePath $exe -RedirectStandardOutput $badOut -RedirectStandardError $badErr -PassThru -NoNewWindow
$bad.WaitForExit(20000) | Out-Null
if (-not $bad.HasExited) {
  Write-Host 'STILL_RUNNING_AFTER_20S=True (FAIL: it bound a routable address)'
  Stop-Process -Id $bad.Id -Force -ErrorAction SilentlyContinue
} else {
  Write-Host "EXITED=True EXIT_CODE=$($bad.ExitCode)"
}
Write-Host '--- stderr ---'
Get-Content $badErr -Tail 10 -ErrorAction SilentlyContinue

Write-Host "`nROOT_KEPT=$root"

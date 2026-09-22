# Group 8 release verification for the Cursor bridge.
#
# Runs the *release* artifact (not the debug binary the other verify scripts use)
# against isolated state and checks, in one pass:
#
#   1. stdout prints {"type":"ready","port":N} and N equals the address we set
#   2. GET /__byok-api__/healthz is reachable
#   3. GET /__byok-api__/api/models never returns the credential in cleartext
#      (it must expose has_api_key instead) -- the group-3 guarantee, re-checked
#      against the shipped binary
#   4. a control-API request without the token is refused (401) while the same
#      request with the token succeeds (200) -- the contrast, on one instance,
#      plus the fail-closed case of a bridge started with no token at all
#   5. GET /__byok-api__/api/settings/tab is still 404 (the TAB subsystem stays
#      deleted)
#   6. the user's home state directory ~/.coderelay-cursor-bridge is untouched:
#      CODERELAY_CURSOR_DATA_DIR fully redirects state
#
# Two documented traps are avoided on purpose:
#   - stderr is redirected to a FILE, never to a pipe nobody drains (a
#     redirected-but-unread pipe deadlocks the child once its log buffer fills,
#     and the ready line is never observed)
#   - the process is matched by its FULL name cursor-bridge-x86_64-pc-windows-msvc,
#     because the artifact has a target-triple suffix while the process name
#     drops the extension
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-group8-release.ps1
$ErrorActionPreference = 'Continue'

$repo = Resolve-Path (Join-Path $PSScriptRoot '..')
$exe = Join-Path $repo 'sidecars\cursor-bridge\bin\cursor-bridge-x86_64-pc-windows-msvc.exe'
if (-not (Test-Path $exe)) { throw "release binary not found: $exe (run scripts/build-cursor-bridge.ps1)" }
$exeItem = Get-Item $exe
$exeHash = (Get-FileHash $exe -Algorithm SHA256).Hash
Write-Host "EXE=$exe"
Write-Host "EXE_BYTES=$($exeItem.Length)  EXE_MTIME=$($exeItem.LastWriteTime.ToString('s'))  EXE_SHA256=$exeHash"

$root = Join-Path $env:TEMP ('cb-verify-g8-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $root | Out-Null
$env:APPDATA = Join-Path $root 'appdata'
New-Item -ItemType Directory -Force $env:APPDATA | Out-Null
$env:RUST_LOG = 'cursor_server=info'
Write-Host "ROOT=$root"

# The user's real state directory. It may have been created by earlier runs, so
# "isolation" is asserted as "its contents and mtime do not change", not as
# "it does not exist".
$homeStateDir = Join-Path $env:USERPROFILE '.coderelay-cursor-bridge'
function Get-HomeStateFingerprint {
  if (-not (Test-Path $homeStateDir)) { return 'ABSENT' }
  $item = Get-Item $homeStateDir
  $children = Get-ChildItem $homeStateDir -Force -Recurse -ErrorAction SilentlyContinue |
    Sort-Object FullName |
    ForEach-Object { "$($_.FullName)|$($_.Length)|$($_.LastWriteTimeUtc.ToString('o'))" }
  return (($item.LastWriteTimeUtc.ToString('o')) + "`n" + ($children -join "`n"))
}
$homeBefore = Get-HomeStateFingerprint
Write-Host "HOME_STATE_DIR=$homeStateDir  EXISTS_BEFORE=$(Test-Path $homeStateDir)"

function Get-FreePort {
  $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
  $listener.Start()
  $port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
  $listener.Stop()
  return $port
}

# Starts the bridge; returns @{ Proc; Port; ReadyLine; StdoutLog; StderrLog; DataDir }
function Start-Bridge([int]$port, [string]$token, [string]$label) {
  $dataDir = Join-Path $root "data-$label"
  New-Item -ItemType Directory -Force $dataDir | Out-Null
  $stdout = Join-Path $root "$label.stdout.log"
  $stderr = Join-Path $root "$label.stderr.log"

  $env:CODERELAY_CURSOR_DATA_DIR = $dataDir
  $env:CODERELAY_CURSOR_LISTEN_ADDR = "127.0.0.1:$port"
  if ([string]::IsNullOrEmpty($token)) {
    Remove-Item Env:\CODERELAY_CURSOR_CONTROL_TOKEN -ErrorAction SilentlyContinue
  } else {
    $env:CODERELAY_CURSOR_CONTROL_TOKEN = $token
  }

  $proc = Start-Process -FilePath $exe -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -NoNewWindow

  $ready = $null
  for ($i = 0; $i -lt 90; $i++) {
    Start-Sleep -Milliseconds 400
    if ($proc.HasExited) { break }
    if (Test-Path $stdout) {
      $text = (Get-Content $stdout -Raw -ErrorAction SilentlyContinue)
      if ($text -and $text.Trim().Length -gt 0) { $ready = $text.Trim().Split("`n")[0].Trim(); break }
    }
  }
  $bound = $null
  if ($ready -match '"port":(\d+)') { $bound = [int]$Matches[1] }
  return @{ Proc = $proc; Port = $bound; ReadyLine = $ready; Stdout = $stdout; Stderr = $stderr; DataDir = $dataDir }
}

function Stop-Bridge($b) {
  if ($b -and $b.Proc) {
    Stop-Process -Id $b.Proc.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
  }
}

# Stop by FULL process name (trap 2).
function Stop-AllBridges {
  $stray = Get-Process | Where-Object { $_.ProcessName -eq 'cursor-bridge-x86_64-pc-windows-msvc' }
  foreach ($p in $stray) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
  return @($stray).Count
}

function Invoke-Probe([string]$method, [string]$url, [string]$token, [string]$body) {
  $headers = @{}
  if (-not [string]::IsNullOrEmpty($token)) { $headers['Authorization'] = "Bearer $token" }
  try {
    $p = @{ Uri = $url; Method = $method; TimeoutSec = 20; UseBasicParsing = $true; Headers = $headers }
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

$token = 'verify-group8-release-token'
$port = Get-FreePort
Write-Host "REQUESTED_PORT=$port"

# ---------------------------------------------------------------------------
Write-Host "`n=== 1) ready handshake: port must equal the requested address ==="
$b = Start-Bridge $port $token 'main'
Write-Host "READY_LINE=$($b.ReadyLine)"
if (-not $b.Port) {
  Write-Host 'BRIDGE DID NOT START'
  Get-Content $b.Stderr -Tail 30 -ErrorAction SilentlyContinue
  Stop-AllBridges | Out-Null
  exit 1
}
Write-Host "READY_PORT=$($b.Port)  MATCHES_REQUESTED=$($b.Port -eq $port)"
$base = "http://127.0.0.1:$($b.Port)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 2) healthz reachable (not behind the control token) ==="
$r = Invoke-Probe 'GET' "$base/__byok-api__/healthz" '' $null
Write-Host "GET healthz (no token)         -> HTTP $($r.Code)   (expect 204)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 4a) control API: no token refused, correct token accepted ==="
$statusUrl = "$base/__byok-api__/api/harness/cursor/status"
$r = Invoke-Probe 'GET' $statusUrl '' $null
Write-Host "GET status, no credential      -> HTTP $($r.Code)  $($r.Body)   (expect 401)"
$r = Invoke-Probe 'GET' $statusUrl 'wrong-token' $null
Write-Host "GET status, wrong token        -> HTTP $($r.Code)  $($r.Body)   (expect 401)"
$r = Invoke-Probe 'GET' $statusUrl $token $null
Write-Host "GET status, correct token      -> HTTP $($r.Code)   (expect 200)"

# ---------------------------------------------------------------------------
Write-Host "`n=== 3) models must not return the credential in cleartext ==="
$secret = 'sk-g8-release-plaintext-canary-71ab'
$model = @{
  display_name = 'G8 Verify Model'
  type         = 'openai'
  base_url     = 'https://provider.example/v1/chat/completions'
  use_full_url = $true
  api_key      = $secret
  tooltip_data = 'g8'
  model_id     = 'g8-verify-model'
} | ConvertTo-Json -Compress
$modelsUrl = "$base/__byok-api__/api/models"
$r = Invoke-Probe 'POST' $modelsUrl $token "{`"models`":[$model]}"
Write-Host "POST models                    -> HTTP $($r.Code)   (expect 201)"
$r = Invoke-Probe 'GET' $modelsUrl $token $null
Write-Host "GET models (raw body)          -> HTTP $($r.Code)"
Write-Host $r.Body
Write-Host "CANARY_LEAKED_IN_BODY=$($r.Body -like "*$secret*")   (must be False)"
$parsed = $null
try { $parsed = $r.Body | ConvertFrom-Json } catch { }
if ($parsed) {
  $m = $parsed[0]
  Write-Host "HAS_API_KEY_FIELD=$($m.PSObject.Properties.Name -contains 'has_api_key')  value=$($m.has_api_key)"
  Write-Host "HAS_FINGERPRINT_FIELD=$($m.PSObject.Properties.Name -contains 'api_key_fingerprint')  value=$($m.api_key_fingerprint)"
  Write-Host "HAS_CLEARTEXT_API_KEY_FIELD=$($m.PSObject.Properties.Name -contains 'api_key')   (must be False)"
}

# ---------------------------------------------------------------------------
Write-Host "`n=== 5) TAB route stays deleted: /api/settings/tab must be 404 ==="
$r = Invoke-Probe 'GET' "$base/__byok-api__/api/settings/tab" $token $null
Write-Host "GET settings/tab (with token)  -> HTTP $($r.Code)   (expect 404)"
$r = Invoke-Probe 'GET' "$base/__byok-api__/api/settings/tab" '' $null
Write-Host "GET settings/tab (no token)    -> HTTP $($r.Code)   (expect 404)"

Stop-Bridge $b

# ---------------------------------------------------------------------------
Write-Host "`n=== 4b) fail-closed: a bridge started with NO token refuses everything ==="
$port2 = Get-FreePort
$open = Start-Bridge $port2 '' 'notoken'
Write-Host "READY_LINE=$($open.ReadyLine)"
if ($open.Port) {
  $base2 = "http://127.0.0.1:$($open.Port)"
  $r = Invoke-Probe 'GET' "$base2/__byok-api__/api/harness/cursor/status" '' $null
  Write-Host "GET status, no token configured -> HTTP $($r.Code)  $($r.Body)   (expect 401)"
  $r = Invoke-Probe 'GET' "$base2/__byok-api__/healthz" '' $null
  Write-Host "GET healthz, no token configured -> HTTP $($r.Code)   (expect 204; healthz is not a control route)"
} else {
  Write-Host 'BRIDGE DID NOT START (no token)'
  Get-Content $open.Stderr -Tail 20 -ErrorAction SilentlyContinue
}
Stop-Bridge $open

# ---------------------------------------------------------------------------
Write-Host "`n=== 6) home state directory untouched ==="
$homeAfter = Get-HomeStateFingerprint
Write-Host "HOME_EXISTS_AFTER=$(Test-Path $homeStateDir)"
Write-Host "HOME_STATE_UNCHANGED=$($homeBefore -eq $homeAfter)   (must be True)"

$killed = Stop-AllBridges
Write-Host "`nSTRAY_PROCESSES_KILLED=$killed"
$dataDirs = (Get-ChildItem $root -Directory | Where-Object { $_.Name -like 'data-*' } | ForEach-Object { $_.Name }) -join ' '
Write-Host "DATA_DIRS=$dataDirs"
Write-Host "ROOT_KEPT=$root"

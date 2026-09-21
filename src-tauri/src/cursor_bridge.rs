//! Manages the Cursor bridge sidecar: process lifetime, the persisted binding
//! list, and the control-API calls that keep the bridge in step with CodeRelay.
//!
//! The design mirrors `gateway.rs` deliberately. The bridge speaks the same
//! ready handshake as the Go relay sidecar, so `StartupLatch`, `atomic_write`,
//! `load_json`, `terminate_child_tree` and `spawn_stderr_reader` are reused
//! rather than reimplemented.
//!
//! Ownership split, which the plan fixes and the code must not drift from:
//!
//! - CodeRelay owns the process and the *binding list* (which API key feeds
//!   which model). That list lives in `cursor-bridge.json`.
//! - The bridge's SQLite file is a runtime cache of that list. It is rebuilt
//!   through `PUT /models/reconcile` whenever the relay's port changes, because
//!   the bridge derives `model_hash` from the request URL. Without that, a port
//!   change would leave the previous generation of rows behind and every model
//!   would show up twice in Cursor's picker.
use crate::gateway::{
    app_data_dir, app_state_snapshot, atomic_write, load_json, spawn_stderr_reader,
    terminate_child_tree, StartupLatch, StartupResult, READY_TIMEOUT,
};
use crate::models::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
const CONFIG_FILE: &str = "cursor-bridge.json";
/// Subdirectory of the app data dir handed to the bridge. It keeps the bridge's
/// SQLite file and CA away from CodeRelay's own `state.json`, and it is *not*
/// the bridge's default home-directory location, so a separately installed
/// `cursor-bridge` can never end up sharing the database file.
const BRIDGE_DATA_DIR: &str = "cursor-bridge";
/// The bridge's HTTP path prefix. Kept from upstream so the vendored control
/// API stays diffable against it; renaming would be a large cosmetic diff.
const API_PREFIX: &str = "/__byok-api__/api";
/// Default per-request timeout for control-API calls.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Environment variable carrying the control-API token to the bridge. The name
/// is shared with the bridge's side of the contract
/// (`server/src/config.rs::CONTROL_TOKEN_ENV`).
const CONTROL_TOKEN_ENV: &str = "CODERELAY_CURSOR_CONTROL_TOKEN";
/// CA generation is CPU-bound and enabling takeover may terminate Cursor, so
/// those two calls get a much longer budget than a plain status read.
const SLOW_HTTP_TIMEOUT: Duration = Duration::from_secs(120);

/// Port the bridge is listening on, or 0 when it is not running.
///
/// A process-global rather than a field on `AppState`, because `AppState` is
/// rebuilt and persisted all over `gateway.rs` and a derived value must never
/// reach `state.json`. CodeRelay enforces a single instance, so one bridge per
/// process is the actual invariant.
static BRIDGE_PORT: AtomicU16 = AtomicU16::new(0);

/// Shared secret for the bridge's control API.
///
/// Generated once per CodeRelay process and handed to the bridge through the
/// environment, then echoed on every control call. The control API shares a port
/// with the Cursor protocol and can read model credentials and toggle the
/// system-wide MITM injection, so without this any local process could drive it
/// with a single `curl`.
///
/// A process-global for the same reason `BRIDGE_PORT` is one: it must never be
/// serialized into `state.json`. Regenerating per CodeRelay launch (rather than
/// persisting) is deliberate — the secret only has to live as long as the bridge
/// it guards, and a persisted one would leak through config backups.
static CONTROL_TOKEN: OnceLock<String> = OnceLock::new();

/// The control token for this process, created on first use.
fn control_token() -> &'static str {
    CONTROL_TOKEN.get_or_init(|| uuid::Uuid::new_v4().simple().to_string())
}

/// `Some(port)` while the bridge is serving; the value placed on the read-only
/// `AppState::cursor_bridge_port` derived field.
pub(crate) fn current_port() -> Option<u16> {
    match BRIDGE_PORT.load(Ordering::SeqCst) {
        0 => None,
        port => Some(port),
    }
}

pub struct CursorBridgeState {
    inner: Arc<CursorBridgeInner>,
}

impl CursorBridgeState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CursorBridgeInner {
                child: Mutex::new(None),
                // Serialises start/stop so two commands cannot race into two
                // bridge processes, mirroring `gateway`'s lifecycle lock.
                lifecycle: Mutex::new(()),
                generation: AtomicU64::new(0),
                config: Mutex::new(CursorBridgeConfig::default()),
            }),
        }
    }
}

impl Default for CursorBridgeState {
    fn default() -> Self {
        Self::new()
    }
}

struct CursorBridgeInner {
    child: Mutex<Option<Child>>,
    lifecycle: Mutex<()>,
    generation: AtomicU64,
    config: Mutex<CursorBridgeConfig>,
}

fn locked<'a, T>(mutex: &'a Mutex<T>, name: &str) -> Result<MutexGuard<'a, T>, String> {
    mutex
        .lock()
        .map_err(|_| format!("{name} 状态锁已损坏，请重启 CodeRelay"))
}

// ---------------------------------------------------------------------------
// Persisted state
// ---------------------------------------------------------------------------

/// One "binding": the user-visible pairing of an API key with a model, plus the
/// tuning knobs Cursor should receive. The relay URL is deliberately absent — it
/// is derived at sync time from the relay's *resolved* port, so a port change
/// cannot leave a stale URL behind in the file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CursorBinding {
    pub id: String,
    pub key_id: String,
    pub model_id: String,
    /// Empty means "fall back to `model_id`"; the bridge rejects an empty
    /// display name, so CodeRelay fills it in rather than letting it through.
    pub display_name: String,
    pub remark: String,
    pub reasoning_effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

/// Bridge-side preferences that CodeRelay drives.
///
/// These live here rather than in `localStorage` because the bridge is the
/// component that acts on them, and a second preference store would drift.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CursorBridgePreferences {
    /// Requested bridge service port; 0 means "let the OS pick".
    pub service_port: u16,
    /// Requested MITM proxy port; 0 means "let the OS pick".
    pub proxy_port: u16,
    pub proxy_mode: String,
    pub proxy_address: String,
    pub proxy_auth_enabled: bool,
    pub proxy_username: String,
    /// The outbound-proxy password, **write-only**.
    ///
    /// Empty means "keep the stored password", matching the bridge's own
    /// `ProxySettingsInput` contract. `skip_serializing` is what makes it
    /// write-only: this struct is persisted to `cursor-bridge.json` *and*
    /// embedded in [`CursorBridgeStatus`], so without it the password would sit
    /// in cleartext on disk and be echoed straight into the WebView's controlled
    /// `<input type="password">`. The bridge keeps the only stored copy and
    /// reports merely [`CursorBridgeStatus::has_proxy_password`].
    ///
    /// Sending a password still works: `skip_serializing` does not affect
    /// deserialization, so the frontend can hand one in on save.
    #[serde(skip_serializing)]
    pub proxy_password: String,
    /// Empty means 直连: forward Cursor's own commit RPC untouched.
    pub commit_model_id: String,
    pub commit_prompt: String,
}

impl Default for CursorBridgePreferences {
    fn default() -> Self {
        Self {
            service_port: 0,
            proxy_port: 0,
            proxy_mode: "default".into(),
            proxy_address: String::new(),
            proxy_auth_enabled: false,
            proxy_username: String::new(),
            proxy_password: String::new(),
            commit_model_id: String::new(),
            commit_prompt: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CursorBridgeConfig {
    version: u32,
    bindings: Vec<CursorBinding>,
    preferences: CursorBridgePreferences,
    /// The user's explicit takeover intent, owned by CodeRelay.
    ///
    /// Persisted so the injection can be re-attached after a CodeRelay restart
    /// without asking the user again, and so a failure to attach is visible
    /// rather than silently reinterpreted as "the user never wanted it". Never
    /// defaulted to `true`: an absent value means "never asked".
    takeover_enabled: bool,
}

impl Default for CursorBridgeConfig {
    fn default() -> Self {
        Self {
            version: 1,
            bindings: Vec::new(),
            preferences: CursorBridgePreferences::default(),
            takeover_enabled: false,
        }
    }
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_dir(app)?.join(CONFIG_FILE))
}

fn save_config(app: &AppHandle, config: &CursorBridgeConfig) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(config)
        .map_err(|error| format!("序列化 Cursor 桥接配置失败：{error}"))?;
    atomic_write(&config_path(app)?, &data)
}

// ---------------------------------------------------------------------------
// Status reported to the frontend
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorBridgeModel {
    /// The bridge keys its commit-message setting by `model_hash`, not by model
    /// id, so the selector has to offer these values (see
    /// `commit_message.rs::ensure_configured_model`).
    pub model_hash: String,
    pub model_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorBridgeStatus {
    pub running: bool,
    pub port: Option<u16>,
    /// `missing` / `untrusted` / `ready` / `invalid`, or `unknown` when the
    /// bridge is unreachable.
    pub ca: String,
    /// `disabled` / `enabled` / `degraded` / `unknown`.
    pub integration: String,
    pub settings_applied: bool,
    /// The user's persisted intent to inject, as recorded by CodeRelay.
    ///
    /// Distinct from `integration`, which is what the bridge reports about
    /// right now. The switch in the UI follows this value, because it is the
    /// one that survives a restart and the one a re-attach acts on.
    pub takeover_requested: bool,
    pub configured_models: usize,
    /// Models the bridge currently holds, for the commit-model selector.
    pub models: Vec<CursorBridgeModel>,
    pub bindings: Vec<CursorBinding>,
    pub preferences: CursorBridgePreferences,
    pub install_command: Option<String>,
    /// The matching "remove this root again" command, shown next to the install
    /// one. Without it the CA is a one-way door: the trusted root outlives the
    /// feature and the user has no documented way to withdraw it.
    pub uninstall_command: Option<String>,
    pub proxy_url: Option<String>,
    /// Whether the bridge holds an outbound-proxy password.
    ///
    /// `None` means the bridge could not be reached, so the answer is *unknown*
    /// rather than "no". Reporting `false` in that case would be a lie the user
    /// could act on: they might conclude no password is stored and be surprised
    /// when the proxy still authenticates. The password value itself never
    /// returns — this flag is the only signal the UI gets, and it is what lets
    /// the settings page say "a password is stored" instead of leaving the user
    /// to infer it from an empty box.
    pub has_proxy_password: Option<bool>,
    pub last_error: Option<String>,
    /// Display names of bindings that were skipped because their API key is
    /// missing or disabled. Surfaced so the user is told rather than left
    /// wondering why a model they configured never reaches Cursor.
    pub unresolved_bindings: Vec<String>,
    /// The bridge's built-in commit prompt. The settings UI needs it so
    /// "restore default" can put the real text back rather than an empty box.
    pub commit_default_prompt: Option<String>,
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// Builds the HTTP client used for **control-API** calls.
///
/// Every control endpoint requires the per-process token, so it is attached here
/// as a default header rather than at each call site: one place to get right,
/// and no future call can silently forget it. The Cursor protocol routes do not
/// go through this client and are unaffected.
fn http_client(timeout: Duration) -> Result<reqwest::Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", control_token()))
        .map_err(|error| format!("构造 Cursor 桥接鉴权头失败：{error}"))?;
    headers.insert(reqwest::header::AUTHORIZATION, value);
    reqwest::Client::builder()
        .timeout(timeout)
        .default_headers(headers)
        .build()
        .map_err(|error| format!("创建 Cursor 桥接 HTTP 客户端失败：{error}"))
}

fn bridge_base(port: u16) -> String {
    format!("http://127.0.0.1:{port}{API_PREFIX}")
}

async fn read_json(response: reqwest::Response, url: &str) -> Result<Value, String> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("读取 Cursor 桥接响应失败（{url}）：{error}"))?;
    if !status.is_success() {
        let detail = body.trim();
        return Err(format!(
            "Cursor 桥接返回 {status}（{url}）：{detail}"
        ));
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&body)
        .map_err(|error| format!("解析 Cursor 桥接响应失败（{url}）：{error}"))
}

async fn fetch_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("请求 Cursor 桥接失败（{url}）：{error}"))?;
    read_json(response, url).await
}

async fn send_json(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    body: &Value,
) -> Result<Value, String> {
    let response = client
        .request(method, url)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("请求 Cursor 桥接失败（{url}）：{error}"))?;
    read_json(response, url).await
}

/// Reads the harness status, which is where CA and takeover state live.
async fn fetch_harness_status(port: u16) -> Result<Value, String> {
    let client = http_client(HTTP_TIMEOUT)?;
    let url = format!("{}/harness/cursor/status", bridge_base(port));
    fetch_json(&client, &url).await
}

/// Reads the built-in commit prompt, so the UI can restore it.
async fn fetch_commit_default_prompt(port: u16) -> Option<String> {
    let client = http_client(HTTP_TIMEOUT).ok()?;
    let url = format!("{}/settings/commit", bridge_base(port));
    let value = fetch_json(&client, &url).await.ok()?;
    value
        .get("default_prompt")
        .and_then(Value::as_str)
        .filter(|prompt| !prompt.trim().is_empty())
        .map(str::to_string)
}

/// Whether the bridge holds an outbound-proxy password.
///
/// `None` when the bridge is unreachable, so the UI shows "unknown" instead of
/// asserting "no password" — an assertion the user could act on and be wrong
/// about. The password itself is never requested: the bridge reports
/// `has_password` and keeps the value.
async fn fetch_has_proxy_password(port: u16) -> Option<bool> {
    let client = http_client(HTTP_TIMEOUT).ok()?;
    let url = format!("{}/settings/proxy", bridge_base(port));
    let value = fetch_json(&client, &url).await.ok()?;
    value.get("has_password").and_then(Value::as_bool)
}

fn json_str(value: &Value, key: &str, fallback: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

// ---------------------------------------------------------------------------
// Process management
// ---------------------------------------------------------------------------

fn bridge_binary(app: &AppHandle) -> Result<PathBuf, String> {
    let target = option_env!("TARGET").unwrap_or("x86_64-pc-windows-msvc");
    let extension = if cfg!(target_os = "windows") {
        ".exe"
    } else {
        ""
    };
    let names = [
        format!("cursor-bridge-{target}{extension}"),
        format!("cursor-bridge{extension}"),
    ];
    let mut directories = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sidecars/cursor-bridge/bin")];
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            directories.push(parent.to_path_buf());
            directories.push(parent.join("resources"));
            if let Some(contents) = parent.parent() {
                directories.push(contents.join("Resources"));
            }
        }
    }
    if let Ok(resource) = app.path().resource_dir() {
        directories.push(resource);
    }
    let candidates: Vec<PathBuf> = directories
        .into_iter()
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .collect();
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| {
            let checked = candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("，");
            format!("找不到 cursor-bridge sidecar，已检查：{checked}。请先运行 npm run build:cursor-bridge。")
        })
}

/// Data directory handed to the bridge, kept under CodeRelay's app data dir so
/// the bridge's SQLite file never collides with CodeRelay's own state.
fn bridge_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app_data_dir(app)?.join(BRIDGE_DATA_DIR);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("创建 Cursor 桥接数据目录失败：{error}"))?;
    Ok(directory)
}

fn read_stdout_loop(
    stdout: ChildStdout,
    inner: Arc<CursorBridgeInner>,
    startup: Arc<StartupLatch>,
    generation: u64,
) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            startup.signal(StartupResult::Failed(
                "读取 Cursor 桥接标准输出失败".to_string(),
            ));
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str).unwrap_or_default() {
            "ready" => {
                let port = value.get("port").and_then(Value::as_u64).unwrap_or(0) as u16;
                if port > 0 {
                    BRIDGE_PORT.store(port, Ordering::SeqCst);
                    startup.signal(StartupResult::Ready { port });
                } else {
                    startup.signal(StartupResult::Failed(
                        "cursor-bridge ready 事件未提供有效端口".to_string(),
                    ));
                }
            }
            "error" => {
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("cursor-bridge 报告未知启动错误");
                startup.signal(StartupResult::Failed(message.to_string()));
            }
            _ => {}
        }
    }
    // Only clear the port if this is still the current process: a superseded
    // stdout reader must not wipe the port of the bridge that replaced it.
    if inner.generation.load(Ordering::SeqCst) == generation {
        BRIDGE_PORT.store(0, Ordering::SeqCst);
    }
    startup.signal(StartupResult::Failed(
        "cursor-bridge 在 ready 事件前关闭了标准输出".to_string(),
    ));
}

fn stop_process_locked(inner: &Arc<CursorBridgeInner>) {
    inner.generation.fetch_add(1, Ordering::SeqCst);
    let mut child = inner.child.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(process) = child.as_mut() {
        // Only taskkill a live tree: on Windows a recycled PID would otherwise
        // take down an unrelated process. Same guard as `gateway`.
        let exited = matches!(process.try_wait(), Ok(Some(_)));
        if !exited {
            terminate_child_tree(process);
        }
    }
    if let Some(mut process) = child.take() {
        let _ = process.wait();
    }
    BRIDGE_PORT.store(0, Ordering::SeqCst);
}

/// Starts the bridge and waits for its ready line. Blocking, so it must only be
/// called from `spawn_blocking`.
fn start_process_locked(app: &AppHandle, inner: &Arc<CursorBridgeInner>) -> Result<u16, String> {
    {
        let mut child = locked(&inner.child, "Cursor 桥接进程")?;
        if let Some(process) = child.as_mut() {
            if process
                .try_wait()
                .map_err(|error| format!("检查 Cursor 桥接进程状态失败：{error}"))?
                .is_none()
            {
                let port = BRIDGE_PORT.load(Ordering::SeqCst);
                if port > 0 {
                    return Ok(port);
                }
            }
        }
    }
    stop_process_locked(inner);

    let binary = bridge_binary(app)?;
    let data_dir = bridge_data_dir(app)?;
    let service_port = locked(&inner.config, "Cursor 桥接配置")?
        .preferences
        .service_port;

    // The bridge resolves its listen address from the environment. A requested
    // port of 0 means the OS assigns one and the true value comes back on the
    // ready line, which is why nothing here assumes the port up front.
    let listen_addr = if service_port == 0 {
        "127.0.0.1:0".to_string()
    } else {
        format!("127.0.0.1:{service_port}")
    };

    let mut command = Command::new(&binary);
    command
        .env("CODERELAY_CURSOR_DATA_DIR", &data_dir)
        .env("CODERELAY_CURSOR_LISTEN_ADDR", &listen_addr)
        // Per-process shared secret for the control API. Regenerated each launch
        // rather than persisted, so it cannot leak through a config backup, and
        // rotated by simply restarting. The bridge refuses all control requests
        // when this is absent, so a failure to pass it is fail-closed.
        .env(CONTROL_TOKEN_ENV, control_token())
        // Hand the bridge our own pid so it can outlive-proof itself: if
        // CodeRelay dies without running its exit hook (crash, Task Manager
        // kill), the bridge notices the parent handle signalling and shuts down
        // on its own. Without this an orphan keeps the SQLite file open and
        // leaves Cursor pointing at an injection nobody can reach — and the
        // next launch competes for the same database. Same contract as the Go
        // relay sidecar.
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .current_dir(&data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let binary_display = binary.display().to_string();
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 cursor-bridge（{binary_display}）失败：{error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法捕获 cursor-bridge 标准输出".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法捕获 cursor-bridge 标准错误".to_string())?;

    let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
    *locked(&inner.child, "Cursor 桥接进程")? = Some(child);

    let startup = Arc::new(StartupLatch::default());
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    spawn_stderr_reader(stderr, stderr_buffer.clone());
    {
        let inner = inner.clone();
        let startup = startup.clone();
        thread::spawn(move || read_stdout_loop(stdout, inner, startup, generation));
    }

    match startup.wait(READY_TIMEOUT) {
        Some(StartupResult::Ready { port }) => Ok(port),
        Some(StartupResult::Failed(error)) => {
            stop_process_locked(inner);
            Err(format!("启动 Cursor 桥接失败：{error}"))
        }
        None => {
            let detail = stderr_buffer
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .trim()
                .to_string();
            stop_process_locked(inner);
            let timeout_seconds = READY_TIMEOUT.as_secs();
            Err(if detail.is_empty() {
                format!("等待 cursor-bridge ready 事件超时（{timeout_seconds} 秒）")
            } else {
                format!("等待 cursor-bridge ready 事件超时：{detail}")
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Binding sync (Phase 3 reconcile)
// ---------------------------------------------------------------------------

/// The bridge's `ModelConfigInput` derives in `snake_case`, so every key below
/// is spelled the way the wire format expects. `type` is the one renamed field.
///
/// The user never types a URL or a key id here: each value is derived from the
/// relay's resolved address and the selected API key.
fn model_payload(
    binding: &CursorBinding,
    api_key: &str,
    relay_base_url: &str,
    sort_order: i64,
) -> Value {
    let display_name = if binding.display_name.trim().is_empty() {
        binding.model_id.clone()
    } else {
        binding.display_name.clone()
    };
    let mut payload = json!({
        "sort_order": sort_order,
        "display_name": display_name,
        "type": "openai",
        "base_url": relay_base_url,
        "api_key": api_key,
        "model_id": binding.model_id,
        // The bridge validates this as non-empty and shows it as the tooltip.
        "tooltip_data": display_name,
        "openai_endpoint": "/v1/chat/completions",
    });
    if let Some(effort) = normalize_effort(&binding.reasoning_effort) {
        payload["reasoning_effort"] = json!(effort);
    }
    if let Some(tokens) = binding.context_window_tokens {
        payload["context_window_tokens"] = json!(tokens);
    }
    if let Some(tokens) = binding.max_output_tokens {
        payload["max_completion_tokens"] = json!(tokens);
    }
    if let Some(extra) = binding.extra_params.clone() {
        if !extra.is_null() {
            payload["openai_extra_params_enabled"] = json!(true);
            payload["openai_extra_params"] = extra;
        }
    }
    payload
}

/// Maps the UI's reasoning labels onto the wire values the bridge accepts.
///
/// The bridge rejects anything outside its closed set, and a rejection would
/// fail the entire reconcile, so an unrecognised label is dropped here rather
/// than sent. An empty result means "no reasoning effort", which is correct for
/// both an empty label and an unknown one.
fn normalize_effort(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "extra high" | "extrahigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

/// The relay address the bridge must call.
///
/// Prefers the port the relay actually bound over the configured one, because
/// `actual_port` is the session value and the configured port may have been
/// taken.
fn relay_base_url(state: &AppState) -> String {
    let port = state.actual_port.unwrap_or(state.config.port);
    format!("http://127.0.0.1:{port}/v1")
}

/// One reconcile request plus what the UI needs to explain it.
///
/// There is deliberately no `base_url` field: the URL is derived inside
/// [`model_payload`] and only ever appears in the models themselves. Keeping a
/// second copy here is what previously left an unread `reconciled_base_url` on
/// the persisted config — drift is detected by comparing the bridge's rows
/// (`fingerprints_match`), not by remembering a URL.
struct SyncPayload {
    models: Vec<Value>,
    /// Display names of bindings skipped because their API key is gone or
    /// disabled. Surfaced to the user instead of failing silently.
    unresolved: Vec<String>,
}

fn build_sync_payload(config: &CursorBridgeConfig, state: &AppState) -> SyncPayload {
    let base_url = relay_base_url(state);
    let keys: HashMap<&str, &str> = state
        .keys
        .iter()
        .filter(|key| key.enabled)
        .map(|key| (key.id.as_str(), key.key.as_str()))
        .collect();
    let mut models = Vec::new();
    let mut unresolved = Vec::new();
    for binding in &config.bindings {
        let Some(secret) = keys.get(binding.key_id.as_str()) else {
            unresolved.push(if binding.display_name.trim().is_empty() {
                binding.model_id.clone()
            } else {
                binding.display_name.clone()
            });
            continue;
        };
        models.push(model_payload(binding, secret, &base_url, models.len() as i64));
    }
    SyncPayload {
        models,
        unresolved,
    }
}

async fn push_models(port: u16, models: &[Value]) -> Result<Value, String> {
    let client = http_client(HTTP_TIMEOUT)?;
    let url = format!("{}/models/reconcile", bridge_base(port));
    send_json(
        &client,
        reqwest::Method::PUT,
        &url,
        &json!({ "models": models }),
    )
    .await
}

async fn fetch_models(port: u16) -> Result<Vec<Value>, String> {
    let client = http_client(HTTP_TIMEOUT)?;
    let url = format!("{}/models", bridge_base(port));
    match fetch_json(&client, &url).await? {
        Value::Array(items) => Ok(items),
        _ => Ok(Vec::new()),
    }
}

/// Projects the bridge's model rows onto what the UI needs.
///
/// `model_hash` is included because the commit-message setting is keyed by it,
/// not by `model_id` (`commit_message.rs::ensure_configured_model`).
fn parse_models(items: Vec<Value>) -> Vec<CursorBridgeModel> {
    items
        .into_iter()
        .filter_map(|item| {
            let model_hash = item.get("model_hash")?.as_str()?.to_string();
            let model_id = item
                .get("model_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let display_name = item
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(CursorBridgeModel {
                model_hash,
                model_id,
                display_name,
            })
        })
        .collect()
}

/// A comparable projection of one model, used to decide whether the bridge
/// still matches what CodeRelay wants.
///
/// `base_url` is the field that betrays a relay port change, and `model_id`
/// plus `display_name` is the identity that would show up twice. The tuning
/// knobs and the API key are included so that editing them propagates even
/// though the *set* of models is nominally unchanged. `model_hash` itself is
/// deliberately excluded: CodeRelay cannot recompute it (it hashes a normalized
/// form it does not model), and comparing the inputs that feed it is equivalent.
///
/// The key is compared as a **fingerprint**, never as the key itself: the bridge
/// stopped returning `api_key` precisely so a local process cannot read the
/// credential out of this endpoint, and CodeRelay must not depend on a field
/// that no longer exists. Comparing digests preserves the semantics — a rotated
/// key still changes the value — without needing the cleartext back.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct ModelFingerprint {
    base_url: String,
    model_id: String,
    display_name: String,
    reasoning_effort: String,
    context_window_tokens: Option<u64>,
    max_completion_tokens: Option<u64>,
    api_key_fingerprint: Option<String>,
    extra_params: String,
}

/// Digest of a provider credential, used only to notice that it changed.
///
/// Must stay byte-for-byte identical to the bridge's
/// `ModelConfig::api_key_fingerprint` (`sha256(key.trim())[..8]`, lowercase hex),
/// or every status read would report drift and force a pointless reconcile.
fn api_key_fingerprint(api_key: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return None;
    }
    let digest = Sha256::digest(api_key.as_bytes());
    Some(hex::encode(&digest[..8]))
}

fn fingerprint(value: &Value) -> ModelFingerprint {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let number = |key: &str| value.get(key).and_then(Value::as_u64);
    // The stored side (a bridge response) carries the digest; the desired side
    // (a payload CodeRelay built) carries the key and is digested here. Treating
    // both through one helper is what keeps the two sides comparable.
    let key_fingerprint = value
        .get("api_key_fingerprint")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| api_key_fingerprint(&text("api_key")));
    ModelFingerprint {
        base_url: text("base_url"),
        model_id: text("model_id"),
        display_name: text("display_name"),
        reasoning_effort: text("reasoning_effort"),
        context_window_tokens: number("context_window_tokens"),
        max_completion_tokens: number("max_completion_tokens"),
        api_key_fingerprint: key_fingerprint,
        // Compared as text so an absent and an empty object both compare equal.
        extra_params: value
            .get("openai_extra_params")
            .filter(|extra| !extra.is_null())
            .map(|extra| extra.to_string())
            .unwrap_or_default(),
    }
}

fn fingerprints_match(stored: &[Value], desired: &[Value]) -> bool {
    if stored.len() != desired.len() {
        return false;
    }
    let mut stored: Vec<ModelFingerprint> = stored.iter().map(fingerprint).collect();
    let mut desired: Vec<ModelFingerprint> = desired.iter().map(fingerprint).collect();
    stored.sort();
    desired.sort();
    stored == desired
}

/// Converges the bridge onto the persisted binding list.
///
/// Called on start and whenever drift is detected, so a relay port change heals
/// itself instead of leaving a second copy of every model behind.
async fn reconcile(port: u16, app: &AppHandle, inner: &Arc<CursorBridgeInner>) -> Result<usize, String> {
    let state = app_state_snapshot(app)?;
    let payload = {
        let config = locked(&inner.config, "Cursor 桥接配置")?.clone();
        build_sync_payload(&config, &state)
    };
    push_models(port, &payload.models).await?;
    Ok(payload.models.len())
}

/// Reconciles only when the bridge's rows disagree with what CodeRelay wants.
///
/// `Ok(None)` means already converged. This is the routine called on every
/// status read, so it must stay cheap: one list request, and a write only on
/// real drift.
async fn reconcile_if_drifted(
    port: u16,
    app: &AppHandle,
    inner: &Arc<CursorBridgeInner>,
) -> Result<Option<usize>, String> {
    let state = app_state_snapshot(app)?;
    let payload = {
        let config = locked(&inner.config, "Cursor 桥接配置")?.clone();
        build_sync_payload(&config, &state)
    };
    let stored = fetch_models(port).await?;
    if fingerprints_match(&stored, &payload.models) {
        return Ok(None);
    }
    reconcile(port, app, inner).await.map(Some)
}

// ---------------------------------------------------------------------------
// Status assembly
// ---------------------------------------------------------------------------

async fn build_status(
    app: &AppHandle,
    inner: &Arc<CursorBridgeInner>,
) -> Result<CursorBridgeStatus, String> {
    let (bindings, preferences, takeover_requested) = {
        let config = locked(&inner.config, "Cursor 桥接配置")?;
        (
            config.bindings.clone(),
            config.preferences.clone(),
            config.takeover_enabled,
        )
    };
    // Computed from the live state rather than remembered from the last sync, so
    // deleting an API key shows up immediately even if no reconcile ran.
    let unresolved_bindings = match app_state_snapshot(app) {
        Ok(state) => {
            let config = locked(&inner.config, "Cursor 桥接配置")?.clone();
            build_sync_payload(&config, &state).unresolved
        }
        Err(_) => Vec::new(),
    };
    let stopped = |error: Option<String>| CursorBridgeStatus {
        running: false,
        port: None,
        ca: "unknown".into(),
        integration: "unknown".into(),
        settings_applied: false,
        takeover_requested,
        configured_models: 0,
        // No process means no stored rows to report; the selector is empty.
        models: Vec::new(),
        bindings: bindings.clone(),
        preferences: preferences.clone(),
        install_command: None,
        uninstall_command: None,
        proxy_url: None,
        // The bridge is not running, so whether it holds a password is unknown
        // rather than "no".
        has_proxy_password: None,
        last_error: error,
        unresolved_bindings: unresolved_bindings.clone(),
        commit_default_prompt: None,
    };

    let Some(port) = current_port() else {
        return Ok(stopped(None));
    };
    let harness = match fetch_harness_status(port).await {
        Ok(value) => value,
        Err(error) => return Ok(stopped(Some(error))),
    };

    // The relay may have rebound to a different port since the last sync, which
    // changes every model hash in the bridge. Converging here keeps the Cursor
    // picker from ever showing the same model twice.
    let mut last_error = None;
    if let Err(error) = reconcile_if_drifted(port, app, inner).await {
        last_error = Some(error);
    }
    let harness = if last_error.is_none() {
        fetch_harness_status(port).await.unwrap_or(harness)
    } else {
        harness
    };

    // The commit model is stored by `model_hash`, so the selector needs the real
    // rows rather than the binding list (whose ids CodeRelay chose).
    let models = fetch_models(port).await.map(parse_models).unwrap_or_default();
    let commit_default_prompt = fetch_commit_default_prompt(port).await;
    let has_proxy_password = fetch_has_proxy_password(port).await;

    Ok(CursorBridgeStatus {
        running: true,
        port: Some(port),
        ca: json_str(&harness, "ca", "unknown"),
        integration: json_str(&harness, "integration", "unknown"),
        settings_applied: harness
            .get("settings_applied")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        // CodeRelay's own record is authoritative for intent. The bridge's copy
        // is deliberately not consulted: it is written by whichever control call
        // arrived last, so treating it as the source of truth is how "row missing
        // means enabled" crept in. Intent lives in exactly one place.
        takeover_requested,
        configured_models: harness
            .get("configured_models")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        models,
        bindings,
        preferences,
        install_command: harness
            .get("ca_install_command")
            .and_then(Value::as_str)
            .map(str::to_string),
        uninstall_command: harness
            .get("ca_uninstall_command")
            .and_then(Value::as_str)
            .map(str::to_string),
        proxy_url: harness
            .get("proxy_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        has_proxy_password,
        last_error,
        unresolved_bindings,
        commit_default_prompt,
    })
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Loads `cursor-bridge.json` at startup and forces the stopped state.
///
/// Mirrors the relay contract that every launch begins stopped. The persisted
/// file has no `running` field, so the value that could otherwise lie is the
/// process-global port, which is cleared here.
pub fn initialize(app: &AppHandle, state: &CursorBridgeState) -> Result<(), String> {
    BRIDGE_PORT.store(0, Ordering::SeqCst);
    let config: CursorBridgeConfig = load_json(&config_path(app)?);
    *locked(&state.inner.config, "Cursor 桥接配置")? = config;
    Ok(())
}

/// Re-attaches a takeover the user had explicitly enabled before a restart.
///
/// Called right after `start_process_locked` succeeds, so the proxy is already
/// listening and the settings written below point at a live port. The intent
/// comes from CodeRelay's own record (`takeover_enabled`), never from the
/// bridge's database: a missing row there means "never asked", and treating it
/// as "asked for" is exactly the implicit takeover this function must not
/// reintroduce.
///
/// Best-effort: a failure is reported through the returned status rather than
/// propagated, because the bridge itself started fine and the user needs to see
/// a working page with an explanation, not an error page.
async fn reattach_takeover(inner: &Arc<CursorBridgeInner>, port: u16) {
    let run = || async {
        let enabled = locked(&inner.config, "Cursor 桥接配置")?.takeover_enabled;
        if !enabled {
            return Ok::<(), String>(());
        }
        let client = http_client(SLOW_HTTP_TIMEOUT)?;
        let url = format!("{}/harness/cursor/enabled", bridge_base(port));
        send_json(
            &client,
            reqwest::Method::PUT,
            &url,
            &json!({ "enabled": true }),
        )
        .await?;
        Ok(())
    };
    if let Err(error) = run().await {
        eprintln!("cursor-bridge: re-attaching the enabled takeover failed: {error}");
    }
}

/// Tears the bridge down. Called from the app's exit hook so quitting CodeRelay
/// never orphans the process.
///
/// The Cursor-side injection is cleared **before** the process goes away, and
/// that ordering is the whole point: the proxy is an in-process instance of the
/// bridge, so killing the process first would leave `settings.json` pointing at
/// a port nothing is listening on and take Cursor's network down with it.
/// Clearing it while the bridge is still alive is the only moment at which the
/// revert actually works.
///
/// The persisted takeover flag is deliberately left set, so the next launch can
/// re-attach if the user had turned injection on.
pub fn shutdown(state: &CursorBridgeState) {
    let inner = state.inner.clone();
    let _lifecycle = match inner.lifecycle.lock() {
        Ok(guard) => Some(guard),
        Err(error) => Some(error.into_inner()),
    };
    if let Some(port) = current_port() {
        // The exit hook cannot await: `RunEvent::Exit` runs on the main thread
        // with the event loop already winding down. A short blocking wait on a
        // dedicated runtime is the only way to get the clear-injection request
        // out before the process tree is killed.
        let _ = tauri::async_runtime::block_on(clear_injection(port, HTTP_TIMEOUT));
    }
    stop_process_locked(&inner);
}

/// Tells the bridge to remove Cursor's managed proxy settings.
///
/// Best-effort by contract: the caller is on a shutdown or stop path, where
/// failing to send the request must never prevent the process from being
/// stopped. A failure here means the next launch's `cleanup_stale_settings()`
/// self-heal is the thing that repairs the residue.
async fn clear_injection(port: u16, timeout: Duration) -> Result<(), String> {
    let client = http_client(timeout)?;
    let url = format!("{}/harness/cursor/injection", bridge_base(port));
    send_json(&client, reqwest::Method::DELETE, &url, &json!({})).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cursor_bridge_status(
    app: AppHandle,
    runtime: State<'_, CursorBridgeState>,
) -> Result<CursorBridgeStatus, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    build_status(&app, &inner).await
}

#[tauri::command]
pub async fn cursor_bridge_start(
    app: AppHandle,
    runtime: State<'_, CursorBridgeState>,
) -> Result<CursorBridgeStatus, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    let port = tauri::async_runtime::spawn_blocking({
        let app = app.clone();
        let inner = inner.clone();
        move || {
            let _lifecycle = locked(&inner.lifecycle, "Cursor 桥接生命周期")?;
            start_process_locked(&app, &inner)
        }
    })
    .await
    .map_err(|error| format!("启动 Cursor 桥接任务执行失败：{error}"))??;
    // Push the binding list immediately: a fresh bridge has an empty (or stale)
    // model set, and Cursor would otherwise show nothing.
    let _ = reconcile(port, &app, &inner).await;
    // Re-attach the injection the user had explicitly enabled before. Done here
    // rather than from a status read, so nothing can take Cursor over as a side
    // effect of the UI asking what the state is.
    reattach_takeover(&inner, port).await;
    build_status(&app, &inner).await
}

#[tauri::command]
pub async fn cursor_bridge_stop(
    app: AppHandle,
    runtime: State<'_, CursorBridgeState>,
) -> Result<CursorBridgeStatus, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    // Hand the Cursor-side revert to the bridge *before* the process dies. The
    // proxy lives inside that process, so once it is gone the settings.json it
    // wrote point at a dead port and Cursor stops being able to reach anything.
    // A failure here is logged, not fatal: stopping the bridge is what the user
    // asked for, and the next start's self-heal repairs the residue.
    if let Some(port) = current_port() {
        if let Err(error) = clear_injection(port, HTTP_TIMEOUT).await {
            eprintln!("cursor-bridge: clearing Cursor injection before stop failed: {error}");
        }
    }
    tauri::async_runtime::spawn_blocking({
        let inner = inner.clone();
        move || {
            let _lifecycle = locked(&inner.lifecycle, "Cursor 桥接生命周期")?;
            stop_process_locked(&inner);
            Ok::<(), String>(())
        }
    })
    .await
    .map_err(|error| format!("停止 Cursor 桥接任务执行失败：{error}"))??;
    build_status(&app, &inner).await
}

#[tauri::command]
pub async fn cursor_bridge_init_ca(
    app: AppHandle,
    runtime: State<'_, CursorBridgeState>,
) -> Result<CursorBridgeStatus, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    let port = current_port()
        .ok_or_else(|| "Cursor 桥接尚未运行，请先启动后再初始化证书。".to_string())?;
    let client = http_client(SLOW_HTTP_TIMEOUT)?;
    let url = format!("{}/harness/cursor/ca/initialize", bridge_base(port));
    send_json(&client, reqwest::Method::POST, &url, &json!({})).await?;
    build_status(&app, &inner).await
}

#[tauri::command]
pub async fn cursor_bridge_install_command(
    runtime: State<'_, CursorBridgeState>,
) -> Result<Option<String>, String> {
    let _inner = runtime.inner.clone();
    drop(runtime);
    let Some(port) = current_port() else {
        return Ok(None);
    };
    let harness = fetch_harness_status(port).await?;
    Ok(harness
        .get("ca_install_command")
        .and_then(Value::as_str)
        .map(str::to_string))
}

#[tauri::command]
pub async fn cursor_bridge_set_enabled(
    app: AppHandle,
    runtime: State<'_, CursorBridgeState>,
    enabled: bool,
) -> Result<CursorBridgeStatus, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    let port = current_port()
        .ok_or_else(|| "Cursor 桥接尚未运行，请先启动后再切换注入。".to_string())?;
    // Enabling may terminate Cursor and rewrite its settings.json, so this call
    // is allowed to take far longer than a plain control read.
    let client = http_client(SLOW_HTTP_TIMEOUT)?;
    let url = format!("{}/harness/cursor/enabled", bridge_base(port));
    send_json(
        &client,
        reqwest::Method::PUT,
        &url,
        &json!({ "enabled": enabled }),
    )
    .await?;
    // Record the intent *after* the bridge accepted it, so a rejected enable
    // cannot leave CodeRelay remembering a takeover that never happened. This is
    // the only writer of the flag: a later launch re-attaches from here, and
    // nothing derives it from the bridge's own database.
    {
        let mut config = locked(&inner.config, "Cursor 桥接配置")?.clone();
        if config.takeover_enabled != enabled {
            config.takeover_enabled = enabled;
            save_config(&app, &config)?;
            *locked(&inner.config, "Cursor 桥接配置")? = config;
        }
    }
    build_status(&app, &inner).await
}

#[tauri::command]
pub async fn cursor_bridge_sync_models(
    app: AppHandle,
    runtime: State<'_, CursorBridgeState>,
) -> Result<CursorBridgeStatus, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    if let Some(port) = current_port() {
        reconcile(port, &app, &inner).await?;
    }
    build_status(&app, &inner).await
}

/// Persists the binding list.
///
/// Not in the plan's command list, but the plan's flow is unimplementable
/// without it: the frontend has to hand the edited list to the process that
/// owns `cursor-bridge.json` before `cursor_bridge_sync_models` can push it to
/// the bridge. Kept as a separate command so reading status stays side-effect
/// free apart from the drift check.
#[tauri::command]
pub async fn cursor_bridge_save_bindings(
    app: AppHandle,
    runtime: State<'_, CursorBridgeState>,
    bindings: Vec<CursorBinding>,
) -> Result<CursorBridgeStatus, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    let mut config = locked(&inner.config, "Cursor 桥接配置")?.clone();
    let mut seen = std::collections::HashSet::new();
    for binding in &bindings {
        if binding.model_id.trim().is_empty() {
            return Err("每个绑定都必须选择一个模型。".to_string());
        }
        if binding.key_id.trim().is_empty() {
            return Err("每个绑定都必须选择一个 API Key。".to_string());
        }
        if !seen.insert(binding.id.clone()) {
            return Err("绑定的标识重复，请刷新页面后重试。".to_string());
        }
    }
    config.bindings = bindings;
    save_config(&app, &config)?;
    *locked(&inner.config, "Cursor 桥接配置")? = config;

    if let Some(port) = current_port() {
        reconcile(port, &app, &inner).await?;
    }
    build_status(&app, &inner).await
}

/// Persists the bridge preferences (ports, outbound proxy, commit model).
///
/// Separate from the binding list because the two are edited on different
/// screens and a failure in one should not roll back the other.
#[tauri::command]
pub async fn cursor_bridge_save_preferences(
    app: AppHandle,
    runtime: State<'_, CursorBridgeState>,
    preferences: CursorBridgePreferences,
) -> Result<CursorBridgeStatus, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    let mut config = locked(&inner.config, "Cursor 桥接配置")?.clone();
    config.preferences = preferences;
    save_config(&app, &config)?;
    let preferences = config.preferences.clone();
    *locked(&inner.config, "Cursor 桥接配置")? = config;

    // Preferences are applied to the bridge, not merely stored: the bridge keeps
    // its own copy of the proxy/port/commit rows and would otherwise never see
    // the change.
    if let Some(port) = current_port() {
        apply_preferences(port, &preferences).await?;
    }
    build_status(&app, &inner).await
}

async fn apply_preferences(
    port: u16,
    preferences: &CursorBridgePreferences,
) -> Result<(), String> {
    let client = http_client(HTTP_TIMEOUT)?;
    let base = bridge_base(port);
    send_json(
        &client,
        reqwest::Method::PUT,
        &format!("{base}/settings/ports"),
        &json!({
            "proxy_port": preferences.proxy_port,
            "service_port": preferences.service_port,
        }),
    )
    .await?;
    let mut proxy = json!({
        "mode": if preferences.proxy_mode == "custom" { "custom" } else { "default" },
        "address": preferences.proxy_address,
        "auth_enabled": preferences.proxy_auth_enabled,
        "username": preferences.proxy_username,
    });
    // An omitted password means "keep the stored one"; sending an empty string
    // would erase it.
    if !preferences.proxy_password.is_empty() {
        proxy["password"] = json!(preferences.proxy_password);
    }
    send_json(
        &client,
        reqwest::Method::PUT,
        &format!("{base}/settings/proxy"),
        &proxy,
    )
    .await?;
    send_json(
        &client,
        reqwest::Method::PUT,
        &format!("{base}/settings/commit"),
        &json!({
            "model_id": preferences.commit_model_id,
            "prompt": preferences.commit_prompt,
        }),
    )
    .await?;
    Ok(())
}

use crate::models::{Account, AccountCredential, ApiKey, AppState, RequestLog, ServiceConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::menu::MenuItem;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;

const STATE_FILE: &str = "state.json";
const CREDENTIALS_FILE: &str = "credentials.json";
const RUNTIME_DIR: &str = "sidecar-runtime";
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_PENDING_REQUESTS: usize = 4096;
const MAX_STDERR_BYTES: usize = 16 * 1024;
const STATE_CHANGED_EVENT: &str = "coderelay-state-changed";

#[derive(Clone)]
pub struct RuntimeState {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    app: Mutex<AppState>,
    credentials: Mutex<HashMap<String, AccountCredential>>,
    child: Mutex<Option<Child>>,
    lifecycle: Mutex<()>,
    events: Mutex<EventContext>,
    generation: AtomicU64,
    tray_start: Mutex<Option<MenuItem<tauri::Wry>>>,
    tray_stop: Mutex<Option<MenuItem<tauri::Wry>>>,
}

#[derive(Default)]
struct EventContext {
    pending: HashMap<String, PendingRequest>,
}

#[derive(Default)]
struct PendingRequest {
    method: String,
    path: String,
    model: String,
    account_id: String,
    api_key_id: String,
}

#[derive(Debug)]
struct RuntimeFiles {
    root: PathBuf,
    config_path: PathBuf,
    manifest_path: PathBuf,
}

#[derive(Debug, Clone)]
enum StartupResult {
    Ready { port: u16 },
    Failed(String),
}

#[derive(Default)]
struct StartupLatch {
    result: Mutex<Option<StartupResult>>,
    changed: Condvar,
}

impl StartupLatch {
    fn signal(&self, result: StartupResult) {
        let mut current = self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current.is_none() {
            *current = Some(result);
            self.changed.notify_all();
        }
    }

    fn wait(&self, timeout: Duration) -> Option<StartupResult> {
        let current = self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current.is_some() {
            return current.clone();
        }
        let (current, _) = self
            .changed
            .wait_timeout_while(current, timeout, |result| result.is_none())
            .unwrap_or_else(|error| error.into_inner());
        current.clone()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CredentialStore {
    version: u32,
    accounts: Vec<AccountCredential>,
}

impl RuntimeState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                app: Mutex::new(AppState::default()),
                credentials: Mutex::new(HashMap::new()),
                child: Mutex::new(None),
                lifecycle: Mutex::new(()),
                events: Mutex::new(EventContext::default()),
                generation: AtomicU64::new(0),
                tray_start: Mutex::new(None),
                tray_stop: Mutex::new(None),
            }),
        }
    }
}

fn locked<'a, T>(mutex: &'a Mutex<T>, name: &str) -> Result<MutexGuard<'a, T>, String> {
    mutex
        .lock()
        .map_err(|_| format!("{name} 状态锁已损坏，请重启 CodeRelay"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|error| format!("读取应用数据目录失败：{error}"))
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("路径缺少父目录：{}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("创建目录 {} 失败：{error}", parent.display()))
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    ensure_parent(path)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("data");
    let temporary = path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), now_ms()));
    let mut file = fs::File::create(&temporary)
        .map_err(|error| format!("创建临时文件 {} 失败：{error}", temporary.display()))?;
    file.write_all(data)
        .map_err(|error| format!("写入临时文件 {} 失败：{error}", temporary.display()))?;
    file.sync_all()
        .map_err(|error| format!("同步临时文件 {} 失败：{error}", temporary.display()))?;
    drop(file);
    if let Err(first_error) = fs::rename(&temporary, path) {
        if path.exists() {
            fs::remove_file(path)
                .map_err(|error| format!("替换文件 {} 失败：{error}", path.display()))?;
            fs::rename(&temporary, path).map_err(|error| {
                format!(
                    "提交临时文件 {} 失败（初始错误：{first_error}）：{error}",
                    temporary.display()
                )
            })?;
        } else {
            return Err(format!(
                "提交临时文件 {} 失败：{first_error}",
                temporary.display()
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("设置文件权限 {} 失败：{error}", path.display()))?;
    }
    Ok(())
}

fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn state_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_dir(app)?.join(STATE_FILE))
}

fn credentials_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_dir(app)?.join(CREDENTIALS_FILE))
}

fn save_app_state(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let mut persistent = state.clone();
    persistent.sanitize_for_persistence();
    let data = serde_json::to_vec_pretty(&persistent)
        .map_err(|error| format!("序列化应用状态失败：{error}"))?;
    atomic_write(&state_path(app)?, &data)
}

fn save_credentials(
    app: &AppHandle,
    credentials: &HashMap<String, AccountCredential>,
) -> Result<(), String> {
    let mut accounts: Vec<AccountCredential> = credentials.values().cloned().collect();
    accounts.sort_by(|left, right| left.account_id.cmp(&right.account_id));
    let store = CredentialStore {
        version: 1,
        accounts,
    };
    let data = serde_json::to_vec_pretty(&store)
        .map_err(|error| format!("序列化账号凭据失败：{error}"))?;
    atomic_write(&credentials_path(app)?, &data)
}

fn load_credentials(app: &AppHandle) -> Result<HashMap<String, AccountCredential>, String> {
    let store: CredentialStore = load_json(&credentials_path(app)?);
    Ok(store
        .accounts
        .into_iter()
        .filter(|credential| {
            !credential.account_id.trim().is_empty() && !credential.access_token.trim().is_empty()
        })
        .map(|credential| (credential.account_id.clone(), credential))
        .collect())
}

pub fn initialize(app: &AppHandle, runtime: &RuntimeState) -> Result<(), String> {
    let mut state: AppState = load_json(&state_path(app)?);
    // 请求日志只保留当天（本地时区），跨天自动清零；统计与日志解耦，按天聚合持久化。
    state.retain_today_logs();
    state.logs.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    state.migrate_legacy();
    let mut credentials = load_credentials(app)?;
    let mut migrated = false;

    for account in &mut state.accounts {
        let access_token = account
            .access_token
            .take()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        if let Some(access_token) = access_token {
            credentials.insert(
                account.id.clone(),
                AccountCredential {
                    account_id: account.id.clone(),
                    access_token,
                    refresh_token: account
                        .refresh_token
                        .take()
                        .map(|token| token.trim().to_string())
                        .filter(|token| !token.is_empty()),
                },
            );
            migrated = true;
        } else {
            account.refresh_token = None;
        }
    }

    // A previous process may have persisted enabled=true. The product contract
    // requires every application launch to leave the proxy stopped.
    state.running = false;
    state.config.enabled = false;
    state.actual_port = None;
    state.sync_today_snapshot();
    *locked(&runtime.inner.app, "应用")? = state.clone();
    *locked(&runtime.inner.credentials, "凭据")? = credentials.clone();
    if migrated {
        save_credentials(app, &credentials)?;
    }
    save_app_state(app, &state)
}

fn validate_config(config: &mut ServiceConfig) -> Result<(), String> {
    if config.port < 1024 {
        return Err("服务端口必须在 1024 到 65535 之间".to_string());
    }
    config.scope = config.scope.trim().to_ascii_lowercase();
    if config.scope != "localhost" && config.scope != "lan" {
        return Err("访问范围必须是 localhost 或 lan".to_string());
    }
    config.bind_host = if config.scope == "lan" {
        "0.0.0.0".to_string()
    } else {
        "127.0.0.1".to_string()
    };
    if config.request_timeout_ms < 1_000 {
        return Err("请求超时不能小于 1000 毫秒".to_string());
    }
    if config.max_retries > 8 {
        return Err("失败重试次数不能超过 8 次".to_string());
    }
    if ![
        "auto",
        "random",
        "single_account",
        "quota_high_first",
        "custom",
    ]
    .contains(&config.routing_strategy.as_str())
    {
        return Err(format!("不支持的路由策略：{}", config.routing_strategy));
    }
    if !["enabled", "images_only", "disabled"].contains(&config.image_generation_mode.as_str()) {
        return Err(format!(
            "不支持的图片生成模式：{}",
            config.image_generation_mode
        ));
    }
    Ok(())
}

fn auth_file_name(account_id: &str) -> String {
    let safe: String = account_id
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.trim_matches('_').is_empty() {
        "account".to_string()
    } else {
        safe
    };
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in account_id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{safe}-{hash:016x}.json")
}

fn plan_rank(plan: &str) -> i32 {
    match plan.trim().to_ascii_lowercase().as_str() {
        "enterprise" | "企业版" | "团队版" => 3,
        "team" | "pro" | "专业版" => 2,
        "plus" | "会员" => 1,
        _ => 0,
    }
}

fn auth_json(account: &Account, credential: &AccountCredential) -> Value {
    let value = json!({
        "type": "codebuddy",
        "access_token": credential.access_token,
        "refresh_token": credential.refresh_token.clone().unwrap_or_default(),
        "uid": account.uid.clone().unwrap_or_default(),
        "enterprise_id": account.enterprise_id.clone().unwrap_or_default(),
        "domain": account.domain.clone().unwrap_or_default(),
        "base_url": "https://copilot.tencent.com",
        "region": "cn",
        "email": account.email,
        "plan_rank": plan_rank(&account.plan),
        "payment_type": account.plan,
        "quota_remain": account.quota,
    });
    value
}

fn runtime_files(app: &AppHandle) -> Result<RuntimeFiles, String> {
    let root = app_data_dir(app)?.join(RUNTIME_DIR);
    Ok(RuntimeFiles {
        config_path: root.join("config.json"),
        manifest_path: root.join("manifest.json"),
        root,
    })
}

fn prepare_runtime_files(
    app: &AppHandle,
    state: &AppState,
    credentials: &HashMap<String, AccountCredential>,
) -> Result<RuntimeFiles, String> {
    let files = runtime_files(app)?;
    let auths_dir = files.root.join("auths");
    fs::create_dir_all(&auths_dir)
        .map_err(|error| format!("创建 sidecar 认证目录失败：{error}"))?;

    let mut expected = HashSet::new();
    let mut manifest_accounts = Vec::new();
    for account in state
        .accounts
        .iter()
        .filter(|account| account.status != "disabled")
    {
        if account.region != "cn" {
            continue;
        }
        let Some(credential) = credentials.get(&account.id) else {
            continue;
        };
        if credential.access_token.trim().is_empty() {
            continue;
        }
        let file_name = auth_file_name(&account.id);
        expected.insert(file_name.clone());
        let data = serde_json::to_vec_pretty(&auth_json(account, credential))
            .map_err(|error| format!("序列化账号 {} 的运行时凭据失败：{error}", account.email))?;
        atomic_write(&auths_dir.join(&file_name), &data)?;
        manifest_accounts.push(json!({
            "id": account.id,
            "email": account.email,
            "authId": file_name,
            "authKind": "oauth",
            "planType": account.plan,
            "remainingQuota": account.quota.round() as i64,
        }));
    }

    for entry in fs::read_dir(&auths_dir)
        .map_err(|error| format!("读取 sidecar 认证目录失败：{error}"))?
        .flatten()
    {
        let path = entry.path();
        let is_json = path.extension().and_then(|value| value.to_str()) == Some("json");
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if is_json && !expected.contains(name) {
            fs::remove_file(&path)
                .map_err(|error| format!("删除过期凭据 {} 失败：{error}", path.display()))?;
        }
    }
    if manifest_accounts.is_empty() {
        return Err("没有带有效 Token 且未禁用的 CodeBuddy 中国站账号".to_string());
    }

    let api_keys: Vec<String> = state
        .keys
        .iter()
        .filter(|key| key.enabled && key.key.starts_with("sk-") && !key.key.trim().is_empty())
        .map(|key| key.key.trim().to_string())
        .collect();
    if api_keys.is_empty() {
        return Err("没有启用的 API Key，请先创建以 sk- 开头的 Key".to_string());
    }
    let config = json!({
        "host": state.config.bind_host,
        "port": state.config.port,
        "auth-dir": auths_dir.to_string_lossy(),
        "debug": state.config.debug_logs,
        "api-keys": api_keys,
        "request-log": false,
        "logging-to-file": false,
        "commercial-mode": true,
        "ws-auth": true,
        "request-retry": state.config.max_retries,
        "max-retry-credentials": state.config.max_retries.max(1),
        "max-retry-interval": 30,
        "disable-cooling": false,
        "routing": {
            "strategy": state.config.routing_strategy,
            "session-affinity": state.config.session_affinity,
            "session-affinity-ttl": "30m",
        },
        "image-generation-mode": state.config.image_generation_mode,
        "max-concurrent-image-requests": 1,
    });
    let manifest_keys: Vec<Value> = state
        .keys
        .iter()
        .map(|key| {
            json!({
                "id": key.id,
                "label": key.name,
                "key": key.key,
                "enabled": key.enabled,
                "accountIds": key.account_ids,
                "allowedModels": key.models,
                "responsesWebsockets": false,
            })
        })
        .collect();
    let manifest = json!({
        "locale": "zh-CN",
        "apiKeys": manifest_keys,
        "accounts": manifest_accounts,
        "modelIds": ["auto"],
        "modelAliases": [],
        "excludedModels": [],
        "accountModelRules": [],
        "routingStrategy": state.config.routing_strategy,
        "customRoutingRules": [],
        "immediateSseResponse": true,
        "maxConcurrentImageRequests": 1,
        "debugLogs": state.config.debug_logs,
        "imageGenerationMode": state.config.image_generation_mode,
        "imageModels": ["codebuddy-image-1"],
    });
    atomic_write(
        &files.config_path,
        &serde_json::to_vec_pretty(&config)
            .map_err(|error| format!("序列化 sidecar 配置失败：{error}"))?,
    )?;
    atomic_write(
        &files.manifest_path,
        &serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("序列化 sidecar 清单失败：{error}"))?,
    )?;
    Ok(files)
}

fn sync_credentials_from_runtime(app: &AppHandle, inner: &Arc<RuntimeInner>) -> Result<(), String> {
    let auths_dir = runtime_files(app)?.root.join("auths");
    if !auths_dir.is_dir() {
        return Ok(());
    }
    let accounts = locked(&inner.app, "应用")?.accounts.clone();
    let mut credentials = locked(&inner.credentials, "凭据")?;
    let mut changed = false;
    for account in accounts {
        let path = auths_dir.join(auth_file_name(&account.id));
        let Ok(data) = fs::read(&path) else { continue; };
        let Ok(value) = serde_json::from_slice::<Value>(&data) else { continue; };
        let Some(access_token) = value
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string)
        else { continue; };
        let refresh_token = value
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        let next = AccountCredential { account_id: account.id.clone(), access_token, refresh_token };
        let differs = credentials.get(&account.id).map(|current| {
            current.access_token != next.access_token || current.refresh_token != next.refresh_token
        }).unwrap_or(true);
        if differs {
            credentials.insert(account.id.clone(), next);
            changed = true;
        }
    }
    if changed {
        save_credentials(app, &credentials)?;
    }
    Ok(())
}

fn clear_runtime_files(app: &AppHandle) -> Result<(), String> {
    let root = runtime_files(app)?.root;
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|error| format!("清理 sidecar 运行目录失败：{error}"))?;
    }
    Ok(())
}

fn sidecar_binary(app: &AppHandle) -> Result<PathBuf, String> {
    let target = option_env!("TARGET").unwrap_or("x86_64-pc-windows-msvc");
    let extension = if cfg!(target_os = "windows") {
        ".exe"
    } else {
        ""
    };
    let names = [
        format!("coderelay-proxy-{target}{extension}"),
        format!("coderelay-proxy{extension}"),
    ];
    let mut directories =
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sidecars/coderelay-proxy/bin")];
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
            format!(
                "找不到 coderelay-proxy sidecar，已检查：{}。请先运行 npm run build:sidecar。",
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("，")
            )
        })
}

fn append_stderr(buffer: &Arc<Mutex<String>>, chunk: &str) {
    let mut output = buffer.lock().unwrap_or_else(|error| error.into_inner());
    output.push_str(chunk);
    if output.len() > MAX_STDERR_BYTES {
        let keep = output.len() - MAX_STDERR_BYTES;
        let boundary = output
            .char_indices()
            .find_map(|(index, _)| (index >= keep).then_some(index))
            .unwrap_or(0);
        output.drain(..boundary);
    }
}

fn spawn_stderr_reader(stderr: impl Read + Send + 'static, buffer: Arc<Mutex<String>>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => append_stderr(&buffer, &line),
            }
        }
    });
}

fn event_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn event_or(value: &Value, key: &str, fallback: String) -> String {
    let current = event_string(value, key);
    if current.is_empty() {
        fallback
    } else {
        current
    }
}

fn value_u64(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn value_f64(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn persist_and_emit(app: &AppHandle, inner: &Arc<RuntimeInner>) {
    if let Ok(state) = inner.app.lock() {
        let _ = save_app_state(app, &state);
    }
    let _ = app.emit(STATE_CHANGED_EVENT, ());
    sync_tray_menu(app);
}

fn notify_failure(app: &AppHandle, event: &str, reason: &str) {
    let _ = app
        .notification()
        .builder()
        .title("CodeRelay")
        .body(format!("{event}：{reason}。当前服务状态：已停止。请在 CodeRelay 中检查端口占用、账号状态与日志后重试。"))
        .show();
}

pub fn register_tray_items(app: &AppHandle, start: MenuItem<tauri::Wry>, stop: MenuItem<tauri::Wry>) {
    let inner = app.state::<RuntimeState>().inner.clone();
    *inner
        .tray_start
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(start);
    *inner
        .tray_stop
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(stop);
    sync_tray_menu(app);
}

pub fn sync_tray_menu(app: &AppHandle) {
    let state = app.state::<RuntimeState>();
    let inner = state.inner.clone();
    let running = inner
        .app
        .lock()
        .map(|app_state| app_state.running)
        .unwrap_or(false);
    let start = inner
        .tray_start
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(item) = start.as_ref() {
        let _ = item.set_enabled(!running);
    }
    drop(start);
    let stop = inner
        .tray_stop
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(item) = stop.as_ref() {
        let _ = item.set_enabled(running);
    }
}

fn ingest_event(app: &AppHandle, inner: &Arc<RuntimeInner>, value: &Value) {
    let event_type = event_string(value, "type");
    let mut changed = false;
    match event_type.as_str() {
        "request_started" => {
            let request_id = event_string(value, "requestId");
            if request_id.is_empty() {
                return;
            }
            let mut events = inner
                .events
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            events.pending.insert(
                request_id,
                PendingRequest {
                    method: event_string(value, "method"),
                    path: event_string(value, "path"),
                    model: event_string(value, "model"),
                    api_key_id: event_string(value, "apiKeyId"),
                    ..PendingRequest::default()
                },
            );
            if events.pending.len() > MAX_PENDING_REQUESTS {
                events.pending.clear();
            }
        }
        "auth_selected" | "auth_result" => {
            let request_id = event_string(value, "requestId");
            let account_id = event_string(value, "accountId");
            if !request_id.is_empty() && !account_id.is_empty() {
                let mut events = inner
                    .events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                events.pending.entry(request_id).or_default().account_id = account_id.clone();
            }
            if event_type == "auth_selected" && !account_id.is_empty() {
                if let Ok(mut state) = inner.app.lock() {
                    if let Some(account) = state
                        .accounts
                        .iter_mut()
                        .find(|account| account.id == account_id)
                    {
                        account.last_used = Some(now_ms());
                        changed = true;
                    }
                }
            }
            if event_type == "auth_result" && !account_id.is_empty() {
                let success = value
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Ok(mut state) = inner.app.lock() {
                    if let Some(account) = state
                        .accounts
                        .iter_mut()
                        .find(|account| account.id == account_id)
                    {
                        if success {
                            account.status = "available".to_string();
                            account.failures = 0;
                        } else {
                            account.failures = account.failures.saturating_add(1);
                            account.status = if value.get("authAvailable").and_then(Value::as_bool)
                                == Some(false)
                            {
                                "needs_auth"
                            } else {
                                "cooling"
                            }
                            .to_string();
                        }
                        changed = true;
                    }
                }
            }
        }
        "request_completed" => {
            let request_id = event_string(value, "requestId");
            if request_id.is_empty() {
                return;
            }
            let pending = inner
                .events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pending
                .remove(&request_id)
                .unwrap_or_default();
            let status = value_u64(value, "status") as u16;
            let aborted = value
                .get("aborted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let account_id = pending.account_id.clone();
            let error_message = value
                .get("errorMessage")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(str::to_string)
                .or_else(|| aborted.then(|| "客户端已取消请求".to_string()));
            // 上游 11140 表示该账号的 chat 被官方风控拦截（区别于内容审核）。
            let chat_restricted = error_message
                .as_deref()
                .map(|message| message.contains("11140"))
                .unwrap_or(false);
            let log = RequestLog {
                request_id,
                timestamp: value
                    .get("completedAtMs")
                    .and_then(Value::as_i64)
                    .unwrap_or_else(now_ms),
                method: event_or(value, "method", pending.method),
                path: event_or(value, "path", pending.path),
                model: event_or(value, "model", pending.model),
                account_id: pending.account_id,
                api_key_id: event_or(value, "apiKeyId", pending.api_key_id),
                status,
                success: status > 0 && status < 400 && !aborted,
                latency_ms: value_u64(value, "latencyMs"),
                error: error_message,
                ..RequestLog::default()
            };
            if let Ok(mut state) = inner.app.lock() {
                // 跨天惰性清零：只保留今天的日志，再记录新请求。
                state.retain_today_logs();
                // 统计与日志解耦：增量累加请求数/延迟/成败/小时桶到当日与累计。
                state.record_completed(&log);
                if let Some(existing) = state
                    .logs
                    .iter_mut()
                    .find(|item| item.request_id == log.request_id)
                {
                    *existing = log;
                } else {
                    state.logs.insert(0, log);
                }
                if chat_restricted && !account_id.is_empty() {
                    if let Some(account) = state
                        .accounts
                        .iter_mut()
                        .find(|account| account.id == account_id)
                    {
                        account.status = "restricted".to_string();
                        account.failures = account.failures.saturating_add(1);
                    }
                }
                changed = true;
            }
        }
        "usage" => {
            let request_id = event_string(value, "requestId");
            if request_id.is_empty() {
                return;
            }
            let usage = value.get("usage").unwrap_or(&Value::Null);
            let input = value_u64(usage, "inputTokens");
            let output = value_u64(usage, "outputTokens");
            let cached = value_u64(usage, "cachedTokens");
            let credit = value_f64(usage, "credit");
            if let Ok(mut state) = inner.app.lock() {
                if let Some(index) = state.logs.iter().position(|item| item.request_id == request_id) {
                    // 先取旧值快照，更新后按"新值-旧值"差量累加 token/缓存/credit，避免重复计数。
                    let prev = state.logs[index].clone();
                    {
                        let log = &mut state.logs[index];
                        log.input_tokens = input;
                        log.output_tokens = output;
                        log.cache_hit = cached > 0;
                        log.credit = credit;
                        if let Some(status) = value.get("status").and_then(Value::as_u64) {
                            log.status = status as u16;
                        }
                        if let Some(success) = value.get("success").and_then(Value::as_bool) {
                            log.success = success;
                        }
                        let usage_error = value
                            .get("errorMessage")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|message| !message.is_empty())
                            .map(str::to_string);
                        if usage_error.is_some() {
                            log.error = usage_error;
                        }
                    }
                    let updated = state.logs[index].clone();
                    state.record_usage(&updated, &prev);
                    changed = true;
                }
            }
        }
        "error" => {
            let message = event_string(value, "message");
            if !message.is_empty() {
                if let Ok(mut state) = inner.app.lock() {
                    state.last_error = Some(message);
                    changed = true;
                }
            }
        }
        _ => {}
    }
    if changed {
        persist_and_emit(app, inner);
    }
}

fn read_stdout_loop(
    stdout: ChildStdout,
    app: AppHandle,
    inner: Arc<RuntimeInner>,
    startup: Arc<StartupLatch>,
) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            startup.signal(StartupResult::Failed(
                "读取 sidecar 标准输出失败".to_string(),
            ));
            return;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event_string(&value, "type").as_str() {
            "ready" => {
                let port = value_u64(&value, "port") as u16;
                if port > 0 {
                    startup.signal(StartupResult::Ready { port });
                } else {
                    startup.signal(StartupResult::Failed(
                        "sidecar ready 事件未提供有效端口".to_string(),
                    ));
                }
            }
            "error" => startup.signal(StartupResult::Failed(
                event_string(&value, "message").if_empty_then("sidecar 报告未知启动错误"),
            )),
            _ => {}
        }
        ingest_event(&app, &inner, &value);
    }
    startup.signal(StartupResult::Failed(
        "sidecar 在 ready 事件前关闭了标准输出".to_string(),
    ));
}

trait StringFallback {
    fn if_empty_then(self, fallback: &str) -> String;
}
impl StringFallback for String {
    fn if_empty_then(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn format_exit_error(status: ExitStatus, stderr: &str) -> String {
    if stderr.trim().is_empty() {
        format!("sidecar 已异常退出（{status}）")
    } else {
        format!("sidecar 已异常退出（{status}）：{}", stderr.trim())
    }
}

fn spawn_exit_monitor(
    app: AppHandle,
    inner: Arc<RuntimeInner>,
    generation: u64,
    startup: Arc<StartupLatch>,
    stderr: Arc<Mutex<String>>,
) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(100));
        if inner.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let status = {
            let mut child = inner
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            child
                .as_mut()
                .and_then(|process| process.try_wait().ok())
                .flatten()
        };
        let Some(status) = status else {
            continue;
        };
        let error = format_exit_error(
            status,
            &stderr.lock().unwrap_or_else(|value| value.into_inner()),
        );
        startup.signal(StartupResult::Failed(error.clone()));
        if inner.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        *inner
            .child
            .lock()
            .unwrap_or_else(|value| value.into_inner()) = None;
        if let Ok(mut state) = inner.app.lock() {
            state.running = false;
            state.actual_port = None;
            state.config.enabled = false;
            state.last_error = Some(error.clone());
        }
        notify_failure(&app, "反代服务异常退出", &error);
        persist_and_emit(&app, &inner);
        return;
    });
}

fn stop_process_only(inner: &Arc<RuntimeInner>) {
    inner.generation.fetch_add(1, Ordering::SeqCst);
    let mut child = inner
        .child
        .lock()
        .map(|mut value| value.take())
        .unwrap_or(None);
    if let Some(process) = child.as_mut() {
        // 进程若已自行退出（如端口占用导致的启动失败），直接回收即可；
        // 对已退出 PID 执行 taskkill /T /F，在该 PID 被系统复用给无关进程时
        // 会误杀整棵进程树，可能拖垮整个系统（表现为全机卡顿）。
        let exited = matches!(process.try_wait(), Ok(Some(_)));
        if !exited {
            terminate_child_tree(process);
        }
    }
    if let Some(mut process) = child.take() {
        let _ = process.wait();
    }
    if let Ok(mut events) = inner.events.lock() {
        events.pending.clear();
    }
}

#[cfg(target_os = "windows")]
fn terminate_child_tree(child: &mut Child) {
    // taskkill /T /F 终止整个进程树，避免 sidecar 派生的子进程残留占用端口。
    use std::os::windows::process::CommandExt;
    let pid = child.id();
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW：GUI 进程不闪现控制台
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // 兜底：taskkill 若因权限等原因失败，仍尝试直接 kill。
    let _ = child.kill();
}

#[cfg(not(target_os = "windows"))]
fn terminate_child_tree(child: &mut Child) {
    let _ = child.kill();
}

fn record_start_failure(app: &AppHandle, inner: &Arc<RuntimeInner>, message: &str) {
    if let Ok(mut state) = inner.app.lock() {
        state.running = false;
        state.config.enabled = false;
        state.actual_port = None;
        state.last_error = Some(message.to_string());
    }
    notify_failure(app, "反代服务启动失败", message);
    persist_and_emit(app, inner);
}

fn start_service_locked(app: &AppHandle, inner: &Arc<RuntimeInner>) -> Result<AppState, String> {
    {
        let mut child = locked(&inner.child, "sidecar")?;
        if let Some(process) = child.as_mut() {
            if process
                .try_wait()
                .map_err(|error| format!("检查 sidecar 状态失败：{error}"))?
                .is_none()
            {
                let state = locked(&inner.app, "应用")?.clone();
                if state.running {
                    return Ok(state);
                }
            }
        }
    }
    stop_process_only(inner);
    sync_credentials_from_runtime(app, inner)?;
    let state = locked(&inner.app, "应用")?.clone();
    let credentials = locked(&inner.credentials, "凭据")?.clone();
    let files = prepare_runtime_files(app, &state, &credentials)?;
    let binary = sidecar_binary(app)?;
    let mut command = Command::new(&binary);
    command
        .arg("--config")
        .arg(&files.config_path)
        .arg("--manifest")
        .arg(&files.manifest_path)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .current_dir(&files.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 请求体转储（debug-log/codebuddy_debug.log）只在「调试日志」开启时启用，
    // 避免正常使用下持续写入诊断 dump 导致日志无限增长。
    if state.config.debug_logs {
        command
            .env("CODEBUDDY_DEBUG_BODY", "1")
            .env("CODEBUDDY_DEBUG_BODY_DIR", files.root.join("debug-log").to_string_lossy().as_ref());
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 sidecar {} 失败：{error}", binary.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法捕获 sidecar 标准输出".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法捕获 sidecar 标准错误".to_string())?;
    let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
    *locked(&inner.child, "sidecar")? = Some(child);
    let startup = Arc::new(StartupLatch::default());
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    spawn_stderr_reader(stderr, stderr_buffer.clone());
    {
        let app = app.clone();
        let inner = inner.clone();
        let startup = startup.clone();
        thread::spawn(move || read_stdout_loop(stdout, app, inner, startup));
    }
    spawn_exit_monitor(
        app.clone(),
        inner.clone(),
        generation,
        startup.clone(),
        stderr_buffer.clone(),
    );
    let result = startup.wait(READY_TIMEOUT).unwrap_or_else(|| {
        let detail = stderr_buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .trim()
            .to_string();
        StartupResult::Failed(if detail.is_empty() {
            format!(
                "等待 sidecar ready 事件超时（{} 秒）",
                READY_TIMEOUT.as_secs()
            )
        } else {
            format!("等待 sidecar ready 事件超时：{detail}")
        })
    });
    match result {
        StartupResult::Ready { port } => {
            let mut state = locked(&inner.app, "应用")?;
            state.running = true;
            state.config.enabled = true;
            state.actual_port = Some(port);
            state.last_error = None;
            save_app_state(app, &state)?;
            let result = state.clone();
            drop(state);
            let _ = app.emit(STATE_CHANGED_EVENT, ());
            Ok(result)
        }
        StartupResult::Failed(error) => {
            stop_process_only(inner);
            record_start_failure(app, inner, &error);
            Err(format!("启动反代失败：{error}"))
        }
    }
}

fn restart_if_running(app: &AppHandle, inner: &Arc<RuntimeInner>) -> Result<AppState, String> {
    let _lifecycle = locked(&inner.lifecycle, "服务生命周期")?;
    if locked(&inner.app, "应用")?.running {
        stop_process_only(inner);
        start_service_locked(app, inner)
    } else {
        Ok(locked(&inner.app, "应用")?.clone())
    }
}

#[tauri::command]
pub fn get_app_state(runtime: State<'_, RuntimeState>) -> Result<AppState, String> {
    let mut state = locked(&runtime.inner.app, "应用")?;
    // 读取时惰性跨天清零日志并同步"今天"快照，保证零点后统计自动归零。
    state.retain_today_logs();
    state.sync_today_snapshot();
    Ok(state.clone())
}

// export_accounts 将指定账号（含凭据 token）导出为 JSON，供备份与跨机器迁移。
// 弹出系统「另存为」对话框由用户选择保存位置；用户取消时返回 None。
// 返回 Some(实际保存路径) 表示成功。
#[tauri::command]
pub fn export_accounts(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    account_ids: Vec<String>,
    default_file_name: String,
) -> Result<Option<String>, String> {
    if account_ids.is_empty() {
        return Err("没有选择要导出的账号".to_string());
    }
    // 1. 收集要导出的账号元数据（保持 account_ids 顺序）与对应凭据。
    let accounts = locked(&runtime.inner.app, "应用")?.clone();
    let credentials = locked(&runtime.inner.credentials, "凭据")?.clone();

    let mut export_items = Vec::new();
    for id in &account_ids {
        let Some(account) = accounts.accounts.iter().find(|a| a.id == *id) else {
            continue;
        };
        let credential = credentials.get(id);
        export_items.push(json!({
            "id": account.id,
            "email": account.email,
            "region": account.region,
            "plan": account.plan,
            "status": account.status,
            "quota": account.quota,
            "quotaTotal": account.quota_total,
            "uid": account.uid,
            "enterpriseId": account.enterprise_id,
            "domain": account.domain,
            "accessToken": credential.map(|c| c.access_token.clone()).unwrap_or_default(),
            "refreshToken": credential.and_then(|c| c.refresh_token.clone()).unwrap_or_default(),
        }));
    }
    if export_items.is_empty() {
        return Err("所选账号均已不存在".to_string());
    }

    let payload = serde_json::to_vec_pretty(&json!({ "accounts": export_items }))
        .map_err(|error| format!("序列化导出内容失败：{error}"))?;

    // 2. 弹「另存为」对话框，等待用户选择路径（阻塞式）。
    let file_name = default_file_name.trim().to_string();
    let mut builder = app.dialog().file().add_filter("JSON", &["json"]);
    if !file_name.is_empty() {
        builder = builder.set_file_name(&file_name);
    }
    let Some(path) = builder.blocking_save_file() else {
        return Ok(None); // 用户取消
    };
    let path = path.into_path().map_err(|_| "无法解析保存路径".to_string())?;

    // 3. 原子写入所选路径。
    atomic_write(&path, &payload)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn save_service_config(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    mut config: ServiceConfig,
) -> Result<AppState, String> {
    validate_config(&mut config)?;
    let mut state = locked(&runtime.inner.app, "应用")?;
    config.enabled = state.running;
    state.config = config;
    save_app_state(&app, &state)?;
    let result = state.clone();
    drop(state);
    let _ = app.emit(STATE_CHANGED_EVENT, ());
    Ok(result)
}

#[tauri::command]
pub fn save_accounts(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    mut accounts: Vec<Account>,
) -> Result<AppState, String> {
    let mut ids = HashSet::new();
    for account in &mut accounts {
        account.id = account.id.trim().to_string();
        account.email = account.email.trim().to_string();
        account.region = account.region.trim().to_ascii_lowercase();
        if account.id.is_empty() || account.email.is_empty() {
            return Err("账号 ID 和邮箱不能为空".to_string());
        }
        if account.region != "cn" {
            return Err(format!(
                "账号 {} 目前只支持 CodeBuddy 中国站",
                account.email
            ));
        }
        if !ids.insert(account.id.clone()) {
            return Err(format!("账号 ID 重复：{}", account.id));
        }
    }
    let mut credentials = locked(&runtime.inner.credentials, "凭据")?;
    let mut next = HashMap::new();
    for account in &mut accounts {
        let supplied = account
            .access_token
            .take()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        let credential = supplied
            .map(|access_token| AccountCredential {
                account_id: account.id.clone(),
                access_token,
                refresh_token: account
                    .refresh_token
                    .take()
                    .map(|token| token.trim().to_string())
                    .filter(|token| !token.is_empty()),
            })
            .or_else(|| credentials.get(&account.id).cloned());
        account.refresh_token = None;
        if let Some(credential) = credential {
            next.insert(account.id.clone(), credential);
        }
    }
    save_credentials(&app, &next)?;
    *credentials = next;
    drop(credentials);
    {
        let mut state = locked(&runtime.inner.app, "应用")?;
        state.accounts = accounts;
        save_app_state(&app, &state)?;
    }
    restart_if_running(&app, &runtime.inner)
}

#[tauri::command]
pub fn save_api_keys(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    mut keys: Vec<ApiKey>,
) -> Result<AppState, String> {
    let mut ids = HashSet::new();
    let mut values = HashSet::new();
    for key in &mut keys {
        key.id = key.id.trim().to_string();
        key.name = key.name.trim().to_string();
        key.key = key.key.trim().to_string();
        if key.id.is_empty() || key.name.is_empty() || key.key.is_empty() {
            return Err("API Key 的 ID、名称和值不能为空".to_string());
        }
        if !key.key.starts_with("sk-") {
            return Err(format!("API Key {} 必须以 sk- 开头", key.name));
        }
        if !ids.insert(key.id.clone()) {
            return Err(format!("API Key ID 重复：{}", key.id));
        }
        if !values.insert(key.key.clone()) {
            return Err(format!("API Key 值重复：{}", key.name));
        }
    }
    {
        let mut state = locked(&runtime.inner.app, "应用")?;
        state.keys = keys;
        save_app_state(&app, &state)?;
    }
    restart_if_running(&app, &runtime.inner)
}

#[tauri::command]
pub fn clear_request_logs(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<AppState, String> {
    let mut state = locked(&runtime.inner.app, "应用")?;
    // 清理日志只清空请求日志列表，不影响总览统计数据（按天聚合与累计均保留）。
    state.logs.clear();
    save_app_state(&app, &state)?;
    let result = state.clone();
    drop(state);
    let _ = app.emit(STATE_CHANGED_EVENT, ());
    Ok(result)
}

async fn refresh_account_inner(app: &AppHandle, inner: &Arc<RuntimeInner>, account_id: &str) -> Result<bool, String> {
    let (account, credential) = {
        let state = locked(&inner.app, "应用")?;
        let account = state
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .cloned()
            .ok_or_else(|| "账号不存在，可能已被删除".to_string())?;
        let credential = locked(&inner.credentials, "凭据")?
            .get(&account.id)
            .cloned()
            .ok_or_else(|| format!("账号 {} 没有可用 Token，请重新认证", account.email))?;
        (account, credential)
    };
    let result = crate::codebuddy_oauth::refresh_quota(
        &credential.access_token,
        credential.refresh_token.as_deref(),
        account.uid.as_deref(),
        account.enterprise_id.as_deref(),
        account.domain.as_deref(),
    )
    .await;
    match result {
        Ok(outcome) => {
            {
                let mut state = locked(&inner.app, "应用")?;
                if let Some(item) = state
                    .accounts
                    .iter_mut()
                    .find(|item| item.id == account_id)
                {
                    item.quota = outcome.quota;
                    item.quota_total = outcome.quota_total;
                    item.plan = outcome.plan.clone();
                    item.status = "available".to_string();
                    item.failures = 0;
                    if outcome.domain.is_some() {
                        item.domain = outcome.domain.clone();
                    }
                }
                save_app_state(app, &state)?;
            }
            if outcome.token_changed {
                let mut credentials = locked(&inner.credentials, "凭据")?;
                credentials.insert(
                    account_id.to_string(),
                    AccountCredential {
                        account_id: account_id.to_string(),
                        access_token: outcome.access_token.clone(),
                        refresh_token: outcome.refresh_token.clone(),
                    },
                );
                save_credentials(app, &credentials)?;
            }
            Ok(outcome.token_changed)
        }
        Err(error) => {
            let mut state = locked(&inner.app, "应用")?;
            if let Some(item) = state
                .accounts
                .iter_mut()
                .find(|item| item.id == account_id)
            {
                item.failures = item.failures.saturating_add(1);
            }
            save_app_state(app, &state)?;
            Err(error)
        }
    }
}

// hot_update_credential_file 将指定账号的最新凭据重写进 sidecar 的 auths 目录。
// sidecar 通过 fsnotify 监听该目录，文件变更会触发凭据热更新（Add/Modify），
// 无需重启 sidecar，从而避免中断进行中的反代请求。
fn hot_update_credential_file(
    app: &AppHandle,
    inner: &Arc<RuntimeInner>,
    account_id: &str,
) -> Result<(), String> {
    let running = locked(&inner.app, "应用")?.running;
    if !running {
        // 服务未运行：无需写运行时文件，凭据已由 refresh_account_inner 持久化。
        return Ok(());
    }
    let (account, credential) = {
        let state = locked(&inner.app, "应用")?;
        let Some(account) = state.accounts.iter().find(|a| a.id == account_id).cloned() else {
            return Ok(());
        };
        let Some(credential) = locked(&inner.credentials, "凭据")?
            .get(account_id)
            .cloned()
        else {
            return Ok(());
        };
        (account, credential)
    };
    if account.status == "disabled" || account.region != "cn" || credential.access_token.trim().is_empty() {
        return Ok(());
    }
    let auths_dir = runtime_files(app)?.root.join("auths");
    let path = auths_dir.join(auth_file_name(&account.id));
    let data = serde_json::to_vec_pretty(&auth_json(&account, &credential))
        .map_err(|error| format!("序列化账号 {} 的运行时凭据失败：{error}", account.email))?;
    atomic_write(&path, &data)
}

#[tauri::command]
pub async fn refresh_account_quota(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    account_id: String,
) -> Result<AppState, String> {
    let token_changed = refresh_account_inner(&app, &runtime.inner, &account_id).await?;
    if token_changed {
        // token 轮换：热更新凭据文件让 sidecar 无缝切换，不重启进程，避免
        // 中断正在进行的反代请求。
        if let Err(error) = hot_update_credential_file(&app, &runtime.inner, &account_id) {
            notify_failure(&app, "更新账号凭据失败", &error);
            return Err(error);
        }
    }
    let state = locked(&runtime.inner.app, "应用")?.clone();
    let _ = app.emit(STATE_CHANGED_EVENT, ());
    Ok(state)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshAllResponse {
    pub state: AppState,
    pub refreshed: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[tauri::command]
pub async fn refresh_all_quotas(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<RefreshAllResponse, String> {
    let (ids, skipped) = {
        let state = locked(&runtime.inner.app, "应用")?;
        let total = state.accounts.len();
        let ids: Vec<String> = state
            .accounts
            .iter()
            .filter(|account| account.status != "disabled")
            .map(|account| account.id.clone())
            .collect();
        let skipped = total.saturating_sub(ids.len());
        (ids, skipped)
    };
    if ids.is_empty() {
        return Err("还没有可刷新的 CodeBuddy 账号".to_string());
    }
    let mut refreshed = 0_usize;
    let mut failed = 0_usize;
    for id in &ids {
        match refresh_account_inner(&app, &runtime.inner, id).await {
            Ok(token_changed) => {
                refreshed += 1;
                if token_changed {
                    // token 轮换：热更新凭据文件让 sidecar 无缝切换，不重启
                    // 进程，避免中断正在进行的反代请求。
                    if let Err(error) = hot_update_credential_file(&app, &runtime.inner, id) {
                        notify_failure(&app, "更新账号凭据失败", &error);
                        failed += 1;
                    }
                }
            }
            Err(_) => {
                failed += 1;
            }
        }
    }
    let state = locked(&runtime.inner.app, "应用")?.clone();
    let _ = app.emit(STATE_CHANGED_EVENT, ());
    if refreshed == 0 {
        return Err(format!("全部 {} 个账号刷新失败，请检查网络连接或重新认证", ids.len()));
    }
    Ok(RefreshAllResponse {
        state,
        refreshed,
        failed,
        skipped,
    })
}

fn load_account_credential(inner: &Arc<RuntimeInner>, account_id: &str) -> Result<(Account, AccountCredential), String> {
    let state = locked(&inner.app, "应用")?;
    let account = state
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .cloned()
        .ok_or_else(|| "账号不存在，可能已被删除".to_string())?;
    let credential = locked(&inner.credentials, "凭据")?
        .get(&account.id)
        .cloned()
        .ok_or_else(|| format!("账号 {} 没有可用 Token，请重新认证", account.email))?;
    Ok((account, credential))
}

async fn checkin_account_inner(app: &AppHandle, inner: &Arc<RuntimeInner>, account_id: &str) -> Result<crate::codebuddy_oauth::CheckinResponse, String> {
    let (account, credential) = load_account_credential(inner, account_id)?;
    let response = crate::codebuddy_oauth::perform_checkin(
        &credential.access_token,
        account.uid.as_deref(),
        account.enterprise_id.as_deref(),
        account.domain.as_deref(),
    )
    .await?;
    if response.success {
        let mut state = locked(&inner.app, "应用")?;
        if let Some(item) = state.accounts.iter_mut().find(|item| item.id == account_id) {
            item.last_checkin = Some(now_ms());
            item.checkin_streak = response
                .streak_days
                .map(|days| days.max(0) as u32)
                .unwrap_or_else(|| item.checkin_streak.saturating_add(1));
        }
        drop(state);
        persist_and_emit(app, inner);
    }
    Ok(response)
}

#[tauri::command]
pub async fn codebuddy_checkin_status(
    runtime: State<'_, RuntimeState>,
    account_id: String,
) -> Result<crate::codebuddy_oauth::CheckinStatusResponse, String> {
    let inner = runtime.inner.clone();
    let (account, credential) = load_account_credential(&inner, &account_id)?;
    crate::codebuddy_oauth::get_checkin_status(
        &credential.access_token,
        account.uid.as_deref(),
        account.enterprise_id.as_deref(),
        account.domain.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn codebuddy_checkin(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    account_id: String,
) -> Result<crate::codebuddy_oauth::CheckinResponse, String> {
    let inner = runtime.inner.clone();
    checkin_account_inner(&app, &inner, &account_id).await
}

pub fn start_service_for_tray(app: &AppHandle) -> Result<AppState, String> {
    let runtime = app.state::<RuntimeState>().inner().clone();
    let _lifecycle = locked(&runtime.inner.lifecycle, "服务生命周期")?;
    start_service_locked(app, &runtime.inner)
}

fn stop_service_inner(app: &AppHandle, inner: &Arc<RuntimeInner>) -> Result<AppState, String> {
    let _lifecycle = locked(&inner.lifecycle, "服务生命周期")?;
    stop_process_only(inner);
    if let Err(error) = sync_credentials_from_runtime(app, inner) {
        notify_failure(app, "停止反代服务时保存凭据失败", &error);
        return Err(error);
    }
    if let Err(error) = clear_runtime_files(app) {
        notify_failure(app, "停止反代服务时清理运行目录失败", &error);
        return Err(error);
    }
    let mut state = locked(&inner.app, "应用")?;
    state.running = false;
    state.config.enabled = false;
    state.actual_port = None;
    state.last_error = None;
    save_app_state(app, &state)?;
    let result = state.clone();
    drop(state);
    let _ = app.emit(STATE_CHANGED_EVENT, ());
    sync_tray_menu(app);
    Ok(result)
}

pub fn stop_service_for_tray(app: &AppHandle) -> Result<AppState, String> {
    let runtime = app.state::<RuntimeState>().inner().clone();
    stop_service_inner(app, &runtime.inner)
}

pub fn quit_from_tray(app: &AppHandle) {
    let runtime = app.state::<RuntimeState>().inner().clone();
    shutdown(app, &runtime);
    app.exit(0);
}

#[tauri::command]
pub async fn start_service(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<AppState, String> {
    // start_service_locked 会同步等待 sidecar ready（最长 15 秒）。Tauri 的
    // 同步命令在主线程执行，直接阻塞会冻结窗口事件循环，且等待期间 sidecar
    // 事件触发的 get_app_state 等命令全部排队，造成 UI 彻底假死。移到阻塞
    // 线程池执行，主线程保持响应。
    let inner = runtime.inner.clone();
    drop(runtime);
    tauri::async_runtime::spawn_blocking(move || {
        let _lifecycle = locked(&inner.lifecycle, "服务生命周期")?;
        start_service_locked(&app, &inner)
    })
    .await
    .map_err(|error| format!("启动任务执行失败：{error}"))?
}

#[tauri::command]
pub async fn stop_service(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<AppState, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    tauri::async_runtime::spawn_blocking(move || stop_service_inner(&app, &inner))
        .await
        .map_err(|error| format!("停止任务执行失败：{error}"))?
}

pub fn shutdown(app: &AppHandle, runtime: &RuntimeState) {
    let _lifecycle = runtime
        .inner
        .lifecycle
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    stop_process_only(&runtime.inner);
    let _ = sync_credentials_from_runtime(app, &runtime.inner);
    let _ = clear_runtime_files(app);
    if let Ok(mut state) = runtime.inner.app.lock() {
        state.running = false;
        state.actual_port = None;
        state.config.enabled = false;
        let _ = save_app_state(app, &state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_file_name_is_stable_and_collision_resistant() {
        let first = auth_file_name("acc / 1");
        let second = auth_file_name("acc___1");
        assert!(first.ends_with(".json"));
        assert_ne!(first, second);
        assert_eq!(first, auth_file_name("acc / 1"));
    }

    #[test]
    fn auth_json_matches_sidecar_contract() {
        let account = Account {
            id: "acc-1".into(),
            email: "user@example.cn".into(),
            plan: "PRO".into(),
            ..Account::default()
        };
        let value = auth_json(
            &account,
            &AccountCredential {
                account_id: "acc-1".into(),
                access_token: "access-secret".into(),
                refresh_token: Some("refresh-secret".into()),
            },
        );
        assert_eq!(value["type"], "codebuddy");
        assert_eq!(value["access_token"], "access-secret");
        assert_eq!(value["base_url"], "https://copilot.tencent.com");
        assert_eq!(value["region"], "cn");
    }

    #[test]
    fn config_validation_rejects_unsafe_values() {
        let mut config = ServiceConfig::default();
        config.port = 80;
        assert!(validate_config(&mut config).is_err());
        config.port = 11435;
        config.scope = "lan".into();
        validate_config(&mut config).expect("valid config");
        assert_eq!(config.bind_host, "0.0.0.0");
    }
}

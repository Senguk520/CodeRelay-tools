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
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::menu::MenuItem;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;

const STATE_FILE: &str = "state.json";
const CREDENTIALS_FILE: &str = "credentials.json";
const RUNTIME_DIR: &str = "sidecar-runtime";
// RUNTIME_MODEL_CACHE_FILE 是 sidecar 写出的 CodeBuddy 模型清单缓存。停止服务/退出
// 程序清理运行目录时必须保留它：否则每次重启都要靠一次性的后端同步兜底，单次失败
// 就会让模型目录退化成只剩 auto + codex-auto-review。该文件不含任何凭据。
const RUNTIME_MODEL_CACHE_FILE: &str = "codebuddy_models_cache.json";
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
    /// 本机局域网 IPv4 的进程内缓存。解析要执行外部命令（ipconfig），
    /// 因此只在异步 / 后台路径刷新，状态读取（get_app_state）只读缓存。
    lan_ip_cache: Mutex<LanIpCache>,
    /// 当前运行实例的 config+manifest 稳定指纹；未运行时为 None。
    /// 用于判断配置保存后是否真的需要重启 sidecar。
    running_fingerprint: Mutex<Option<String>>,
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
                lan_ip_cache: Mutex::new(LanIpCache::default()),
                running_fingerprint: Mutex::new(None),
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
    save_app_state(app, &state)?;
    // 已配置为局域网模式时后台预热本机网卡地址，避免首次打开界面时地址空窗。
    // 解析要执行外部命令，必须离开主线程（函数内部走 spawn_blocking）。
    // 放在 initialize 末尾而非 lib.rs：启动流程的唯一入口在此，lib.rs 无需改动。
    prewarm_lan_ip_cache(runtime);
    Ok(())
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

// ---- 局域网地址解析 ----
//
// 目标：为「局域网访问」提供一个**可直接连接**的本机 IPv4（形如 http://192.168.1.23:11435）。
// 采集沿用系统自带的 ipconfig（本项目仅面向 Windows），不引入任何第三方依赖。
//
// 选择策略（对齐参照实现的完整版）：先按网卡类型打分（物理网卡优先、虚拟/隧道垫底），
// 再按地址段打分（192.168.* 优先于 10.*，其余私有段最后），最后用地址字节做确定性
// tiebreak——保证多网卡环境下结果稳定可复现，而不是随机挑一个。

/// 一个局域网候选：来自 ipconfig 的 (网卡名, IPv4)。
#[derive(Debug, Clone, PartialEq, Eq)]
struct LanIpv4Candidate {
    interface_name: String,
    addr: Ipv4Addr,
}

/// 局域网地址缓存有效期。解析要执行外部命令，缓存用于避免状态读取（同步命令、
/// 在主线程）触发进程创建；这个窗口同时也是「换网络后地址自行更新」的最大延迟，
/// 所以取一个既便宜又够实时的值——解析在后台阻塞线程执行，单次只有几十毫秒。
const LAN_IP_CACHE_TTL: Duration = Duration::from_secs(10);

/// 进程内缓存的本机局域网 IPv4。
#[derive(Debug, Default)]
struct LanIpCache {
    ip: Option<String>,
    resolved_at: Option<Instant>,
}

/// 虚拟 / 隧道 / 回环网卡关键词（**包含**匹配即视为虚拟）。
///
/// 虚拟判定优先于物理判定：中文 Windows 的头行是「以太网适配器 vEthernet (WSL)」，
/// 若先按"以太网"判成物理就会把 Hyper-V 虚拟交换机当成局域网出口。
const LAN_VIRTUAL_INTERFACE_KEYWORDS: &[&str] = &[
    "loopback",
    "vethernet",
    "hyper-v",
    "virtual",
    "vmware",
    "vmnet",
    "virtualbox",
    "vbox",
    "docker",
    "veth",
    "virbr",
    "br-",
    "bridge",
    "tailscale",
    "zerotier",
    "wireguard",
    "wintun",
    "tunnel",
    "utun",
    "awdl",
    "llw",
    "tap-",
    "tap ",
    "虚拟",
    "环回",
];

/// 物理网卡关键词（**前缀**匹配）。
const LAN_PHYSICAL_INTERFACE_PREFIXES: &[&str] = &[
    "en",
    "eth",
    "wlan",
    "wi-fi",
    "wifi",
    "ethernet",
    "wireless",
];

/// 物理网卡关键词（**包含**匹配）。中文 Windows 的网卡名形如
/// 「以太网适配器 以太网」「无线局域网适配器 WLAN」，不满足前缀匹配，必须用包含匹配兜住。
const LAN_PHYSICAL_INTERFACE_KEYWORDS: &[&str] = &[
    "ethernet",
    "wireless",
    "wlan",
    "wi-fi",
    "wifi",
    "以太网",
    "无线",
    "本地连接",
];

/// 判定是否为可用的局域网地址：只接受私有段（10/8、172.16/12、192.168/16）。
/// 该判定天然排除回环（127/8）、链路本地（169.254/16）与公网地址。
fn is_lan_ipv4(addr: Ipv4Addr) -> bool {
    addr.is_private()
}

/// 网卡类型打分：0 = 物理网卡（优先），1 = 未知，2 = 虚拟 / 隧道 / 回环（垫底）。
fn lan_interface_score(interface_name: &str) -> u8 {
    let name = interface_name.trim().to_ascii_lowercase();
    if LAN_VIRTUAL_INTERFACE_KEYWORDS
        .iter()
        .any(|keyword| name.contains(keyword))
    {
        return 2;
    }
    if LAN_PHYSICAL_INTERFACE_PREFIXES
        .iter()
        .any(|keyword| name.starts_with(keyword))
        || LAN_PHYSICAL_INTERFACE_KEYWORDS
            .iter()
            .any(|keyword| name.contains(keyword))
    {
        return 0;
    }
    1
}

/// 地址段打分：0 = 192.168.*（家用最常见），1 = 10.*，2 = 其余私有段（172.16-31.* 等）。
fn lan_addr_score(addr: Ipv4Addr) -> u8 {
    let octets = addr.octets();
    if octets[0] == 192 && octets[1] == 168 {
        return 0;
    }
    if octets[0] == 10 {
        return 1;
    }
    2
}

/// 从候选中选出首要局域网 IPv4：按（网卡类型, 地址段, 地址字节）升序取第一。
/// 无候选时返回 None，由调用方静默降级。
fn select_primary_lan_ipv4(mut candidates: Vec<LanIpv4Candidate>) -> Option<Ipv4Addr> {
    candidates.sort_by_key(|candidate| {
        (
            lan_interface_score(&candidate.interface_name),
            lan_addr_score(candidate.addr),
            candidate.addr.octets(),
        )
    });
    candidates.into_iter().next().map(|candidate| candidate.addr)
}

/// 解析 `ipconfig` 输出，抽出所有私有 IPv4 候选。
///
/// 规则（对中英文 Windows 均适用，不依赖字段文案）：
/// - **网卡头行**：不缩进且以 `:` 结尾，剥掉尾部 `:` 作为网卡名；
/// - **地址行**：含 ASCII 子串 `IPv4`（中文输出为「IPv4 地址 …」，仍含该子串），
///   取最后一个 `:` 之后的内容解析；
/// - 非私有地址（回环、169.254 链路本地、公网）直接丢弃。
fn parse_ipconfig_candidates(output: &str) -> Vec<LanIpv4Candidate> {
    let mut candidates = Vec::new();
    let mut current_interface = String::new();
    for line in output.lines() {
        let trimmed = line.trim();
        let is_indented = line
            .chars()
            .next()
            .map(|character| character.is_whitespace())
            .unwrap_or(false);
        if !is_indented && trimmed.ends_with(':') {
            current_interface = trimmed.trim_end_matches(':').trim().to_string();
            continue;
        }
        if !trimmed.contains("IPv4") {
            continue;
        }
        let Some(raw_addr) = trimmed.rsplit(':').next() else {
            continue;
        };
        let Ok(addr) = raw_addr.trim().parse::<Ipv4Addr>() else {
            continue;
        };
        if !is_lan_ipv4(addr) {
            continue;
        }
        candidates.push(LanIpv4Candidate {
            interface_name: current_interface.clone(),
            addr,
        });
    }
    candidates
}

/// 执行 `ipconfig` 并选出首要局域网 IPv4。命令失败 / 无候选时返回 None（静默降级）。
#[cfg(target_os = "windows")]
fn resolve_primary_lan_ipv4() -> Option<Ipv4Addr> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = Command::new("ipconfig")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    select_primary_lan_ipv4(parse_ipconfig_candidates(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// 非 Windows 平台不提供局域网地址解析（本项目仅面向 Windows）。
#[cfg(not(target_os = "windows"))]
fn resolve_primary_lan_ipv4() -> Option<Ipv4Addr> {
    None
}

/// 读取缓存结论。
///
/// - `Some(Some(ip))`：TTL 内解析成功；
/// - `Some(None)`：TTL 内已尝试过，结论是「本机当前没有可用局域网地址」；
/// - `None`：缓存缺失或已过期，需要重新解析。
///
/// 区分「已尝试但无结果」与「尚未尝试」很重要：否则解析不出地址时，
/// 每次状态读取都会重新拉起一次 ipconfig。
fn cached_lan_ip(inner: &Arc<RuntimeInner>) -> Option<Option<String>> {
    let cache = locked(&inner.lan_ip_cache, "局域网地址缓存").ok()?;
    if cache.resolved_at?.elapsed() > LAN_IP_CACHE_TTL {
        return None;
    }
    Some(cache.ip.clone())
}

/// 判断该 IPv4 是否仍属于本机网卡。
///
/// 必要性：切换 WiFi / 连手机热点 / 拔插网线后，旧地址会从网卡上消失，但缓存里
/// 还留着它。若继续把这个地址展示给用户，客户端会去连一个**已经不存在的** IP，
/// 表现为几十秒后超时（SYN 无人应答），而不是快速失败——非常难排查。
///
/// 校验手段是对该地址做一次 UDP 随机端口绑定：地址不属于本机时，操作系统直接
/// 返回「地址不可用」。只做一次系统调用、不监听端口、不发起连接，因此可以在
/// 状态读取路径（主线程）安全调用，也不需要引入任何第三方依赖。
fn is_local_ipv4(addr: Ipv4Addr) -> bool {
    UdpSocket::bind(SocketAddr::new(IpAddr::V4(addr), 0)).is_ok()
}

/// 作废局域网地址缓存，让下一次读取重新解析。
fn invalidate_lan_ip_cache(inner: &Arc<RuntimeInner>) {
    if let Ok(mut cache) = locked(&inner.lan_ip_cache, "局域网地址缓存") {
        cache.ip = None;
        cache.resolved_at = None;
    }
}

/// 从缓存取可用的局域网地址（TTL 内、解析成功、且地址仍在本机）。
/// **绝不现场执行外部命令**；但会做一次极轻量的归属校验，见 `is_local_ipv4`。
fn lan_ip_from_cache(inner: &Arc<RuntimeInner>) -> Option<String> {
    let ip = cached_lan_ip(inner).flatten()?;
    let Ok(addr) = ip.parse::<Ipv4Addr>() else {
        return None;
    };
    if is_local_ipv4(addr) {
        return Some(ip);
    }
    // 地址已不在本机（换网络了）→ 立刻作废缓存并返回空，让上层重新解析；
    // 宁可短暂显示"未识别到"，也不能把一个连不上的地址给用户复制。
    invalidate_lan_ip_cache(inner);
    None
}

/// 刷新局域网地址缓存。**会执行外部命令（ipconfig），禁止在主线程直接调用**，
/// 只能从 `spawn_blocking` 路径或后台线程调用。无论成功与否都记时间戳，
/// 让「解析不到」也享受 TTL 保护。
fn refresh_lan_ip_cache(inner: &Arc<RuntimeInner>) -> Option<String> {
    let ip = resolve_primary_lan_ipv4().map(|addr| addr.to_string());
    if let Ok(mut cache) = locked(&inner.lan_ip_cache, "局域网地址缓存") {
        cache.ip = ip.clone();
        cache.resolved_at = Some(Instant::now());
    }
    ip
}

/// 若当前配置为局域网模式，则刷新一次地址缓存（调用方必须已在阻塞线程 / 后台线程）。
fn warm_lan_ip_cache_if_lan(state: &AppState, inner: &Arc<RuntimeInner>) {
    if scope_is_lan(&state.config.scope) {
        refresh_lan_ip_cache(inner);
    }
}

/// 后台刷新局域网地址缓存，成功后广播状态变化事件让前端自动补上地址。
///
/// 用于状态读取（同步命令，跑在主线程）命中缓存空窗的场景——那里不能直接执行
/// 外部命令，否则会冻结窗口事件循环。
fn schedule_lan_ip_refresh(app: AppHandle, inner: Arc<RuntimeInner>) {
    // 先占位再派发：立刻把 resolved_at 写成当前时间，让并发 / 连续触发的调用
    // 在 TTL 窗口内不再各自拉起一个 ipconfig。判定与占位必须在同一次加锁内完成，
    // 否则两个线程可能同时通过判定、各派发一次。解析完成后由
    // refresh_lan_ip_cache 覆盖时间戳与地址；失败也由 TTL 兜底重试，无需额外状态位。
    let dispatched = match locked(&inner.lan_ip_cache, "局域网地址缓存") {
        Ok(mut cache) => {
            let fresh = cache
                .resolved_at
                .map(|at| at.elapsed() <= LAN_IP_CACHE_TTL)
                .unwrap_or(false);
            if fresh {
                false
            } else {
                cache.resolved_at = Some(Instant::now());
                true
            }
        }
        Err(_) => return,
    };
    if !dispatched {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        // 只在地址真的发生变化时广播：否则每次 TTL 到期的例行解析都会把前端叫起来
        // 重拉一次全量状态，白白形成「解析 → 广播 → 拉取 → 再解析」的循环。
        let before = cached_lan_ip(&inner).flatten();
        let after = refresh_lan_ip_cache(&inner);
        if after.is_some() && after != before {
            let _ = app.emit(STATE_CHANGED_EVENT, ());
        }
    });
}

/// 应用启动时后台预热一次局域网地址缓存（仅在已配置为局域网模式时）。
///
/// 由 `initialize` 末尾调用，让用户在打开界面之前缓存就已就绪；解析要执行
/// 外部命令，必须离开主线程。
fn prewarm_lan_ip_cache(runtime: &RuntimeState) {
    let inner = runtime.inner.clone();
    let scope = locked(&inner.app, "应用")
        .map(|state| state.config.scope.clone())
        .unwrap_or_default();
    if !scope_is_lan(&scope) {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        refresh_lan_ip_cache(&inner);
    });
}

/// 访问范围是否等于「局域网」。
///
/// `scope` 在配置里是自由字符串（历史原因），判定收敛到这里，
/// 避免 `eq_ignore_ascii_case("lan")` 散落在多个函数里。
fn scope_is_lan(scope: &str) -> bool {
    scope.trim().eq_ignore_ascii_case("lan")
}

/// 依据当前配置与缓存中的网卡地址，构造「局域网可连接地址」。
/// 仅在访问范围为 `lan` 且缓存命中时返回 Some。
fn lan_base_url_for_state(state: &AppState, inner: &Arc<RuntimeInner>) -> Option<String> {
    if !scope_is_lan(&state.config.scope) {
        return None;
    }
    let ip = lan_ip_from_cache(inner)?;
    // 运行中优先用 sidecar ready 上报的实际端口，与前端既有取值逻辑一致。
    let port = state.actual_port.unwrap_or(state.config.port);
    Some(format!("http://{ip}:{port}"))
}

/// 在**返回给前端的副本**上填充只读派生字段。
///
/// 关键约束：派生字段绝不能写进 `inner.app` 里那份状态，否则会被持久化到
/// `state.json` 并被误当成配置。这里只处理传值进来的克隆。
fn with_derived_fields(mut state: AppState, inner: &Arc<RuntimeInner>) -> AppState {
    state.lan_base_url = lan_base_url_for_state(&state, inner);
    state
}

/// FNV-1a 64 位哈希，输出 16 位小写十六进制。
///
/// 用途：运行实例指纹的**同进程内**相等性比较，不需要密码学强度，因此不引入
/// `sha2` 依赖（本项目要求零新增第三方依赖）。与 `auth_file_name` 共用同一算法，
/// 保持项目内哈希实现唯一。
fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 指纹稳定化：剔除「随运行状态变化、但不影响 sidecar 启动」的字段，
/// 避免额度刷新、统计变动引发无谓重启（那会打断进行中的请求）。
///
/// 已逐项核对（`prepare_runtime_files` 产出的 config / manifest）：
/// - manifest `accounts[].remainingQuota`：随额度刷新变化 → **剔除**；
/// - manifest `accounts[].planType`：随额度刷新变化 → **保留**（套餐变化重启一次可接受）；
/// - manifest `apiKeys[]`：条目里不含最近使用时间 → 无需处理；
/// - auths 里的 `quota_remain`：不在指纹范围内（本函数只处理 config / manifest）。
///
/// 序列化依赖 serde_json 的默认键序（BTreeMap，按键排序），结果稳定可复现。
fn stable_json_for_fingerprint(value: &Value) -> String {
    let mut value = value.clone();
    if let Some(accounts) = value.get_mut("accounts").and_then(Value::as_array_mut) {
        for account in accounts {
            if let Some(account) = account.as_object_mut() {
                account.remove("remainingQuota");
            }
        }
    }
    serde_json::to_string(&value).unwrap_or_default()
}

/// 运行实例指纹：config 与 manifest 的稳定化内容拼接后取 FNV-1a。
fn runtime_fingerprint(config: &Value, manifest: &Value) -> String {
    let mut buffer = stable_json_for_fingerprint(config);
    buffer.push_str("\n--manifest--\n");
    buffer.push_str(&stable_json_for_fingerprint(manifest));
    fnv1a_hex(buffer.as_bytes())
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
    format!("{safe}-{}.json", fnv1a_hex(account_id.as_bytes()))
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

/// 参与 sidecar 运行时的账号：未禁用、中国站、且存在非空 access_token 的凭据。
/// 返回值保持 `state.accounts` 的原始顺序。
fn runtime_accounts<'a>(
    state: &'a AppState,
    credentials: &'a HashMap<String, AccountCredential>,
) -> Vec<(&'a Account, &'a AccountCredential)> {
    state
        .accounts
        .iter()
        .filter(|account| account.status != "disabled" && account.region == "cn")
        .filter_map(|account| {
            let credential = credentials.get(&account.id)?;
            if credential.access_token.trim().is_empty() {
                return None;
            }
            Some((account, credential))
        })
        .collect()
}

/// 在内存中构建 sidecar 的 config / manifest（**纯函数：零 I/O、不写任何文件**）。
///
/// 为什么必须从 `prepare_runtime_files` 抽出来：`reconcile_if_running` 需要在**不写文件**
/// 的前提下算出指纹。`prepare_runtime_files` 会重写 `auths/*.json`，而那是 sidecar 的凭据
/// 热更新通道（fsnotify 监听）——运行中重复执行会触发无谓的热重载，还可能把尚未由
/// `sync_credentials_from_runtime` 同步回来的新 token 覆盖成旧值。
///
/// 校验顺序与拆分前保持一致：先校验账号，再校验 API Key。
fn build_runtime_payloads(
    state: &AppState,
    credentials: &HashMap<String, AccountCredential>,
    auths_dir: &Path,
) -> Result<(Value, Value), String> {
    let accounts = runtime_accounts(state, credentials);
    if accounts.is_empty() {
        return Err("没有带有效 Token 且未禁用的 CodeBuddy 中国站账号".to_string());
    }
    let manifest_accounts: Vec<Value> = accounts
        .iter()
        .map(|(account, _)| {
            json!({
                "id": account.id,
                "email": account.email,
                "authId": auth_file_name(&account.id),
                "authKind": "oauth",
                "planType": account.plan,
                "remainingQuota": account.quota.round() as i64,
            })
        })
        .collect();

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
    Ok((config, manifest))
}

/// 写入 sidecar 运行目录，返回「文件路径 + 本次 config/manifest 的稳定指纹」。
///
/// ⚠️ 本函数会写 `auths/*.json`（sidecar 的凭据热更新通道）与 config / manifest，
/// **不能**用于「只想算指纹」的场景——那种场景请用纯函数 `build_runtime_payloads`
/// 配合 `runtime_fingerprint`，避免多余的文件副作用。
fn prepare_runtime_files(
    app: &AppHandle,
    state: &AppState,
    credentials: &HashMap<String, AccountCredential>,
) -> Result<(RuntimeFiles, String), String> {
    let files = runtime_files(app)?;
    let auths_dir = files.root.join("auths");
    fs::create_dir_all(&auths_dir)
        .map_err(|error| format!("创建 sidecar 认证目录失败：{error}"))?;

    // 只写凭据文件（保持既有行为：先写期望集合，再清理陈旧文件）。
    let mut expected = HashSet::new();
    for (account, credential) in runtime_accounts(state, credentials) {
        let file_name = auth_file_name(&account.id);
        expected.insert(file_name.clone());
        let data = serde_json::to_vec_pretty(&auth_json(account, credential))
            .map_err(|error| format!("序列化账号 {} 的运行时凭据失败：{error}", account.email))?;
        atomic_write(&auths_dir.join(&file_name), &data)?;
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
    // 内容构建交给纯函数：与 reconcile 的指纹计算共用同一份逻辑，避免两处漂移。
    let (config, manifest) = build_runtime_payloads(state, credentials, &auths_dir)?;
    let fingerprint = runtime_fingerprint(&config, &manifest);
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
    Ok((files, fingerprint))
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
    if !root.exists() {
        return Ok(());
    }
    // 逐项清理而不是整体删目录：模型清单缓存要跨启动保留（见 RUNTIME_MODEL_CACHE_FILE），
    // 其余内容（config.json / manifest.json / auths/ 凭据）照旧清掉，token 不落盘。
    for entry in fs::read_dir(&root)
        .map_err(|error| format!("读取 sidecar 运行目录失败：{error}"))?
        .flatten()
    {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if runtime_cleanup_keeps(name) {
            continue;
        }
        let removal = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removal.map_err(|error| format!("清理 sidecar 运行目录失败：{error}"))?;
    }
    Ok(())
}

// runtime_cleanup_keeps 判断停止/退出清理运行目录时该条目是否必须保留。当前只保留
// 模型清单缓存：它是「打包后模型目录只剩 2 个」问题的关键兜底，被删掉后重启就只能
// 依赖一次性后端同步；凭据文件不在此列。
fn runtime_cleanup_keeps(name: &str) -> bool {
    name.eq_ignore_ascii_case(RUNTIME_MODEL_CACHE_FILE)
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
        // 模型同步失败：把原因（失败阶段 / HTTP 状态 / 业务码 / 使用的账号）写进
        // last_error，概览页与服务页会直接显示，不再只表现为「目录里少了一堆模型」。
        "codebuddy_model_sync_error" => {
            let message = event_string(value, "message");
            if !message.is_empty() {
                if let Ok(mut state) = inner.app.lock() {
                    state.last_error = Some(message);
                    changed = true;
                }
            }
        }
        // 同步恢复成功后清掉这条同步错误，避免失败提示常驻界面；其它来源的
        // last_error（如启动失败、异常退出）不受影响。
        "codebuddy_model_sync" => {
            if let Ok(mut state) = inner.app.lock() {
                let stale = state
                    .last_error
                    .as_deref()
                    .is_some_and(|message| message.starts_with("CodeBuddy 模型同步失败"));
                if stale {
                    state.last_error = None;
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
    // 进程已停，运行实例指纹随之失效；下次启动会重新记录。
    // 用带名字的 locked()：锁毒化时能给出可定位的中文错误（与全局约定一致）。
    if let Ok(mut slot) = locked(&inner.running_fingerprint, "运行指纹") {
        *slot = None;
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
    let (files, accepted_fingerprint) = prepare_runtime_files(app, &state, &credentials)?;
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
            // 记录本次运行实例的指纹，供配置保存时判断是否需要重启。
            // 注意先取指纹锁并释放，再取应用锁：与 reconcile_if_running 的加锁顺序
            // 保持一致（那里是先应用锁、后指纹锁，且两者不重叠持有）。
            if let Ok(mut slot) = locked(&inner.running_fingerprint, "运行指纹") {
                *slot = Some(accepted_fingerprint.clone());
            }
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
    let state = if locked(&inner.app, "应用")?.running {
        stop_process_only(inner);
        start_service_locked(app, inner)?
    } else {
        locked(&inner.app, "应用")?.clone()
    };
    // 返回给前端的 AppState 必须带派生字段：前端是**整份替换** state 的，
    // 缺字段会让局域网地址在界面上凭空消失，而部分命令又不 emit 事件、无法自愈。
    Ok(with_derived_fields(state, inner))
}

/// 配置保存后的「幂等收敛」：只有**真正影响 sidecar 启动**的变更才重启它。
///
/// - 未运行：直接返回（保留 CodeRelay 的手动启停语义，不擅自拉起服务）；
/// - 运行中且指纹一致：**不动进程**，保护进行中的请求（额度刷新、统计变动等不应触发重启）；
/// - 运行中且指纹变化：走既有 `restart_if_running`，从而自动获得
///   `sync_credentials_from_runtime` → `prepare_runtime_files` 的正确顺序。
///
/// 关键点：指纹由 `build_runtime_payloads` 在**内存中**构建，全程不写任何文件——
/// 绝不能在这里重复执行 `prepare_runtime_files`：那会重写 `auths/*.json`（sidecar 的
/// 凭据热更新通道），触发无谓热重载，并可能覆盖掉 sidecar 刚热刷新出来的新 token。
fn reconcile_if_running(app: &AppHandle, inner: &Arc<RuntimeInner>) -> Result<AppState, String> {
    let (running, state) = {
        let guard = locked(&inner.app, "应用")?;
        (guard.running, guard.clone())
    };
    if !running {
        return Ok(with_derived_fields(state, inner));
    }
    let credentials = locked(&inner.credentials, "凭据")?.clone();
    let auths_dir = runtime_files(app)?.root.join("auths");
    let (config, manifest) = match build_runtime_payloads(&state, &credentials, &auths_dir) {
        Ok(payloads) => payloads,
        Err(error) => {
            // 服务正在运行却构建不出运行载荷：不要动这个还在工作的进程（避免
            // 「保存成功却把服务弄停」），但**必须把异常暴露出来**——静默跳过会让
            // 用户以为配置已生效、实际一直跑着旧配置。
            let message = format!("服务配置未能生效：{error}");
            if let Ok(mut guard) = locked(&inner.app, "应用") {
                guard.last_error = Some(message);
            }
            notify_failure(app, "服务配置未能生效", &error);
            persist_and_emit(app, inner);
            let snapshot = locked(&inner.app, "应用")?.clone();
            return Ok(with_derived_fields(snapshot, inner));
        }
    };
    let fingerprint = runtime_fingerprint(&config, &manifest);
    let unchanged = {
        let current = locked(&inner.running_fingerprint, "运行指纹")?;
        current.as_deref() == Some(fingerprint.as_str())
    };
    if unchanged {
        return Ok(with_derived_fields(state, inner));
    }
    restart_if_running(app, inner)
}

#[tauri::command]
pub fn get_app_state(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<AppState, String> {
    let inner = runtime.inner.clone();
    drop(runtime);
    let snapshot = {
        let mut state = locked(&inner.app, "应用")?;
        // 读取时惰性跨天清零日志并同步"今天"快照，保证零点后统计自动归零。
        state.retain_today_logs();
        state.sync_today_snapshot();
        state.clone()
    };
    // 本命令是同步命令（跑在主线程），绝不能在这里执行 ipconfig：缓存没有结论时
    // 改为后台刷新一次，完成后广播事件让前端重新拉取，界面自动补上局域网地址。
    // 先算派生值：这一步会顺带校验缓存里的地址是否还挂在本机网卡上（切换网络后旧
    // 地址会被回收），失效时缓存被作废，紧接着的判定就能立刻补一次解析。
    let derived = with_derived_fields(snapshot, &inner);
    if scope_is_lan(&derived.config.scope) && cached_lan_ip(&inner).is_none() {
        schedule_lan_ip_refresh(app, inner.clone());
    }
    Ok(derived)
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
pub async fn save_service_config(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    mut config: ServiceConfig,
) -> Result<AppState, String> {
    // 本命令要写 state.json，并可能重启 sidecar（等待 ready 最长 15 秒）。Tauri 的
    // 同步命令在主线程执行会冻结窗口事件循环，且等待期间 sidecar 事件触发的
    // get_app_state 全部排队造成界面假死，因此改到阻塞线程池执行。
    let inner = runtime.inner.clone();
    drop(runtime);
    tauri::async_runtime::spawn_blocking(move || {
        validate_config(&mut config)?;
        {
            let mut state = locked(&inner.app, "应用")?;
            config.enabled = state.running;
            state.config = config;
            save_app_state(&app, &state)?;
        }
        let _ = app.emit(STATE_CHANGED_EVENT, ());
        // 配置已落盘。只有运行中且配置确实影响 sidecar 启动时才重启（指纹判定）；
        // 未运行时仅保存，保持既有的手动启停语义。
        let state = reconcile_if_running(&app, &inner)?;
        // 已在阻塞线程池内，可安全预热局域网地址缓存，让返回副本直接带上地址。
        warm_lan_ip_cache_if_lan(&state, &inner);
        Ok(with_derived_fields(state, &inner))
    })
    .await
    .map_err(|error| format!("保存服务配置任务执行失败：{error}"))?
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
    let snapshot = {
        let mut state = locked(&runtime.inner.app, "应用")?;
        // 清理日志只清空请求日志列表，不影响总览统计数据（按天聚合与累计均保留）。
        state.logs.clear();
        save_app_state(&app, &state)?;
        state.clone()
    };
    let _ = app.emit(STATE_CHANGED_EVENT, ());
    Ok(with_derived_fields(snapshot, &runtime.inner))
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
    Ok(with_derived_fields(state, &runtime.inner))
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
    let state = {
        let snapshot = locked(&runtime.inner.app, "应用")?.clone();
        with_derived_fields(snapshot, &runtime.inner)
    };
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
    let state = start_service_locked(app, &runtime.inner)?;
    warm_lan_ip_cache_if_lan(&state, &runtime.inner);
    Ok(with_derived_fields(state, &runtime.inner))
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
    let state = stop_service_inner(app, &runtime.inner)?;
    Ok(with_derived_fields(state, &runtime.inner))
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
        let state = start_service_locked(&app, &inner)?;
        // 已在阻塞线程池内，可以安全预热局域网地址缓存，让返回给前端的副本
        // 直接带上局域网地址，避免界面出现一次空窗。
        warm_lan_ip_cache_if_lan(&state, &inner);
        Ok(with_derived_fields(state, &inner))
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
    tauri::async_runtime::spawn_blocking(move || {
        let state = stop_service_inner(&app, &inner)?;
        Ok(with_derived_fields(state, &inner))
    })
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

    #[test]
    fn runtime_cleanup_keeps_only_model_cache() {
        // 模型清单缓存必须跨「停止服务 / 退出程序」保留，否则下次启动只剩一次性的
        // 后端同步兜底；凭据、配置与清单文件必须照旧清理。
        assert!(runtime_cleanup_keeps(RUNTIME_MODEL_CACHE_FILE));
        assert!(runtime_cleanup_keeps("CodeBuddy_Models_Cache.JSON"));
        assert!(!runtime_cleanup_keeps("manifest.json"));
        assert!(!runtime_cleanup_keeps("config.json"));
        assert!(!runtime_cleanup_keeps("auths"));
        assert!(!runtime_cleanup_keeps(
            "11533863-75e0-4cc5-9684-8887b21de491-0b1ba4631f29510f.json"
        ));
    }

    // ---- 局域网地址解析（纯函数）----
    //
    // 采样自真实 ipconfig 输出形态：网卡头行不缩进且以 ':' 结尾，字段行缩进。
    // 中文 Windows 的字段名是「IPv4 地址 …」，仍含 ASCII 子串 "IPv4"，解析器据此识别。

    fn ipconfig_english_sample() -> String {
        [
            "Windows IP Configuration",
            "",
            "Ethernet adapter Ethernet:",
            "",
            "   Connection-specific DNS Suffix  . :",
            "   Link-local IPv6 Address . . . . . : fe80::1%12",
            "   IPv4 Address. . . . . . . . . . . : 192.168.1.23",
            "   Subnet Mask . . . . . . . . . . . : 255.255.255.0",
            "   Default Gateway . . . . . . . . . : 192.168.1.1",
            "",
            "Wireless LAN adapter Wi-Fi:",
            "",
            "   IPv4 Address. . . . . . . . . . . : 10.0.0.5",
            "   Subnet Mask . . . . . . . . . . . : 255.0.0.0",
        ]
        .join("\n")
    }

    fn ipconfig_chinese_sample() -> String {
        [
            "Windows IP 配置",
            "",
            "以太网适配器 以太网:",
            "",
            "   连接特定的 DNS 后缀 . . . . . . . :",
            "   IPv4 地址 . . . . . . . . . . . . : 192.168.1.23",
            "   子网掩码  . . . . . . . . . . . . : 255.255.255.0",
            "   默认网关. . . . . . . . . . . . . : 192.168.1.1",
            "",
            "无线局域网适配器 WLAN:",
            "",
            "   IPv4 地址 . . . . . . . . . . . . : 192.168.1.88",
            "",
            "以太网适配器 vEthernet (WSL):",
            "",
            "   IPv4 地址 . . . . . . . . . . . . : 172.20.16.1",
            "",
            "以太网适配器 VMware Network Adapter VMnet1:",
            "",
            "   IPv4 地址 . . . . . . . . . . . . : 192.168.56.1",
        ]
        .join("\n")
    }

    #[test]
    fn parse_ipconfig_extracts_private_ipv4_with_interface_names() {
        let candidates = parse_ipconfig_candidates(&ipconfig_english_sample());
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].interface_name, "Ethernet adapter Ethernet");
        assert_eq!(candidates[0].addr.to_string(), "192.168.1.23");
        assert_eq!(
            candidates[1].interface_name,
            "Wireless LAN adapter Wi-Fi"
        );
        assert_eq!(candidates[1].addr.to_string(), "10.0.0.5");
    }

    #[test]
    fn parse_ipconfig_skips_loopback_and_link_local_addresses() {
        let output = [
            "Ethernet adapter Ethernet:",
            "",
            "   IPv4 Address. . . . . . . . . . . : 127.0.0.1",
            "",
            "Ethernet adapter vEthernet (Default Switch):",
            "",
            "   Autoconfiguration IPv4 Address. . : 169.254.10.20",
        ]
        .join("\n");
        assert!(parse_ipconfig_candidates(&output).is_empty());
    }

    #[test]
    fn lan_interface_score_prefers_physical_over_virtual() {
        assert_eq!(lan_interface_score("Ethernet adapter Ethernet"), 0);
        assert_eq!(lan_interface_score("Wireless LAN adapter Wi-Fi"), 0);
        assert_eq!(lan_interface_score("Ethernet adapter vEthernet (WSL)"), 2);
        assert_eq!(
            lan_interface_score("Ethernet adapter VMware Network Adapter VMnet1"),
            2
        );
        assert_eq!(lan_interface_score("Loopback Pseudo-Interface 1"), 2);
        assert_eq!(lan_interface_score("Some Unknown Adapter"), 1);
    }

    #[test]
    fn lan_interface_score_recognises_chinese_adapter_names() {
        assert_eq!(lan_interface_score("以太网适配器 以太网"), 0);
        assert_eq!(lan_interface_score("无线局域网适配器 WLAN"), 0);
        // vEthernet / VMnet 虽是中文头行，但必须按虚拟网卡处理（虚拟判定优先）。
        assert_eq!(lan_interface_score("以太网适配器 vEthernet (WSL)"), 2);
        assert_eq!(
            lan_interface_score("以太网适配器 VMware Network Adapter VMnet1"),
            2
        );
    }

    #[test]
    fn lan_addr_score_prefers_home_networks() {
        assert_eq!(lan_addr_score(Ipv4Addr::new(192, 168, 1, 23)), 0);
        assert_eq!(lan_addr_score(Ipv4Addr::new(10, 0, 0, 5)), 1);
        assert_eq!(lan_addr_score(Ipv4Addr::new(172, 20, 16, 1)), 2);
    }

    #[test]
    fn select_primary_lan_ipv4_prefers_physical_home_network() {
        let candidates = vec![
            LanIpv4Candidate {
                interface_name: "Ethernet adapter VMware Network Adapter VMnet1".to_string(),
                addr: Ipv4Addr::new(192, 168, 56, 1),
            },
            LanIpv4Candidate {
                interface_name: "Wireless LAN adapter Wi-Fi".to_string(),
                addr: Ipv4Addr::new(10, 0, 0, 5),
            },
            LanIpv4Candidate {
                interface_name: "Ethernet adapter Ethernet".to_string(),
                addr: Ipv4Addr::new(192, 168, 1, 23),
            },
        ];
        assert_eq!(
            select_primary_lan_ipv4(candidates).map(|addr| addr.to_string()),
            Some("192.168.1.23".to_string())
        );
    }

    #[test]
    fn select_primary_lan_ipv4_is_deterministic_on_ties() {
        let candidates = vec![
            LanIpv4Candidate {
                interface_name: "Ethernet adapter Ethernet".to_string(),
                addr: Ipv4Addr::new(192, 168, 1, 30),
            },
            LanIpv4Candidate {
                interface_name: "Ethernet adapter Ethernet 2".to_string(),
                addr: Ipv4Addr::new(192, 168, 1, 10),
            },
        ];
        assert_eq!(
            select_primary_lan_ipv4(candidates).map(|addr| addr.to_string()),
            Some("192.168.1.10".to_string())
        );
    }

    #[test]
    fn select_primary_lan_ipv4_returns_none_without_candidates() {
        assert!(select_primary_lan_ipv4(Vec::new()).is_none());
    }

    #[test]
    fn chinese_ipconfig_picks_real_nic_over_virtual_switches() {
        let candidates = parse_ipconfig_candidates(&ipconfig_chinese_sample());
        assert_eq!(candidates.len(), 4);
        assert_eq!(
            select_primary_lan_ipv4(candidates).map(|addr| addr.to_string()),
            Some("192.168.1.23".to_string())
        );
    }

    // ---- 运行指纹与运行载荷（纯函数）----

    #[test]
    fn fnv1a_hex_is_shared_with_auth_file_name() {
        // auth_file_name 复用同一算法：后缀必须等于 fnv1a_hex，钉死"项目内哈希实现唯一"。
        let name = auth_file_name("acc-1");
        assert!(name.ends_with(&format!("-{}.json", fnv1a_hex(b"acc-1"))));
        assert_ne!(fnv1a_hex(b"a"), fnv1a_hex(b"b"));
        assert_eq!(fnv1a_hex(b"a"), fnv1a_hex(b"a"));
    }

    fn fingerprint_fixture(host: &str, remaining_quota: i64, plan: &str) -> (Value, Value) {
        let config = json!({ "host": host, "port": 11435 });
        let manifest = json!({
            "accounts": [
                { "id": "acc-1", "planType": plan, "remainingQuota": remaining_quota }
            ]
        });
        (config, manifest)
    }

    #[test]
    fn stable_json_for_fingerprint_drops_quota_but_keeps_plan_type() {
        let (_, manifest) = fingerprint_fixture("0.0.0.0", 100, "PRO");
        let stable = stable_json_for_fingerprint(&manifest);
        assert!(!stable.contains("remainingQuota"));
        assert!(stable.contains("planType"));
        assert!(stable.contains("PRO"));
    }

    #[test]
    fn runtime_fingerprint_ignores_quota_refresh() {
        // 额度刷新不得触发重启：否则会打断进行中的请求（对应交接文档问题 5 的热更新链路）。
        let (config_a, manifest_a) = fingerprint_fixture("0.0.0.0", 100, "PRO");
        let (config_b, manifest_b) = fingerprint_fixture("0.0.0.0", 42, "PRO");
        assert_eq!(
            runtime_fingerprint(&config_a, &manifest_a),
            runtime_fingerprint(&config_b, &manifest_b)
        );
        assert_eq!(
            runtime_fingerprint(&config_a, &manifest_a),
            runtime_fingerprint(&config_a, &manifest_a)
        );
    }

    #[test]
    fn runtime_fingerprint_changes_on_startup_relevant_fields() {
        let (config_a, manifest_a) = fingerprint_fixture("127.0.0.1", 100, "PRO");
        // 访问范围变化 → 绑定地址变化 → 必须重启。
        let (config_b, manifest_b) = fingerprint_fixture("0.0.0.0", 100, "PRO");
        assert_ne!(
            runtime_fingerprint(&config_a, &manifest_a),
            runtime_fingerprint(&config_b, &manifest_b)
        );
        // 套餐变化会触发一次重启（已确认可接受）。
        let (config_c, manifest_c) = fingerprint_fixture("127.0.0.1", 100, "FREE");
        assert_ne!(
            runtime_fingerprint(&config_a, &manifest_a),
            runtime_fingerprint(&config_c, &manifest_c)
        );
    }

    #[test]
    fn build_runtime_payloads_requires_accounts_then_api_keys() {
        let auths_dir = Path::new("C:/coderelay-test/auths");
        let state = AppState::default();
        let error = build_runtime_payloads(&state, &HashMap::new(), auths_dir).unwrap_err();
        assert!(error.contains("账号"), "unexpected error: {error}");

        let mut state = AppState::default();
        state.accounts.push(Account {
            id: "acc-1".into(),
            email: "user@example.cn".into(),
            region: "cn".into(),
            plan: "PRO".into(),
            ..Account::default()
        });
        let mut credentials = HashMap::new();
        credentials.insert(
            "acc-1".to_string(),
            AccountCredential {
                account_id: "acc-1".into(),
                access_token: "access-secret".into(),
                refresh_token: None,
            },
        );
        // 账号齐了但没有任何启用的 Key → 报 API Key 错误（校验顺序与拆分前一致）。
        let error = build_runtime_payloads(&state, &credentials, auths_dir).unwrap_err();
        assert!(error.contains("API Key"), "unexpected error: {error}");
    }

    #[test]
    fn build_runtime_payloads_matches_sidecar_contract() {
        let mut state = AppState::default();
        state.accounts.push(Account {
            id: "acc-1".into(),
            email: "user@example.cn".into(),
            region: "cn".into(),
            plan: "PRO".into(),
            quota: 12.6,
            ..Account::default()
        });
        state.keys.push(ApiKey {
            id: "key-1".into(),
            name: "cursor".into(),
            key: "sk-test".into(),
            enabled: true,
            ..ApiKey::default()
        });
        let mut credentials = HashMap::new();
        credentials.insert(
            "acc-1".to_string(),
            AccountCredential {
                account_id: "acc-1".into(),
                access_token: "access-secret".into(),
                refresh_token: None,
            },
        );
        // 局域网模式下 host 必须是绑定地址，绝不能被"展示折叠"影响。
        state.config.scope = "lan".into();
        state.config.bind_host = "0.0.0.0".into();
        state.config.port = 11435;

        let (config, manifest) =
            build_runtime_payloads(&state, &credentials, Path::new("C:/coderelay-test/auths"))
                .expect("payloads");

        assert_eq!(config["host"], "0.0.0.0");
        assert_eq!(config["port"], 11435);
        assert_eq!(config["request-log"], false);
        assert_eq!(manifest["accounts"][0]["planType"], "PRO");
        assert_eq!(manifest["accounts"][0]["remainingQuota"], 13);
        assert_eq!(manifest["apiKeys"][0]["key"], "sk-test");
    }
}

import { invoke } from '@tauri-apps/api/core';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { Account, ApiKey, AppState, CheckinResponse, CheckinStatusResponse, CursorBinding, CursorBridgePreferences, CursorBridgeStatus, ModelInfo, OAuthCompleteResponse, OAuthStartResponse, ServiceConfig, UpdateCheckResult } from './types';
import { defaultState } from './types';

const STORAGE_KEY = 'coderelay-app-state';
const CREDENTIALS_KEY = 'coderelay-local-credentials';
const SERVICE_URL = 'http://127.0.0.1:11435';


function hasTauri() {
  return Boolean((window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);
}

function mergeState(value: Partial<AppState> | null | undefined): AppState {
  return {
    ...structuredClone(defaultState),
    ...(value ?? {}),
    config: { ...defaultState.config, ...(value?.config ?? {}) },
    stats: { ...defaultState.stats, ...(value?.stats ?? {}) },
  };
}

function loadLocal(): AppState {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored) return mergeState(JSON.parse(stored) as Partial<AppState>);
  } catch {
    // Use an empty, non-demo state when local preview data is invalid.
  }
  return structuredClone(defaultState);
}

function saveLocal(state: AppState) {
  const safeState = structuredClone(state);
  for (const account of safeState.accounts) {
    delete account.accessToken;
    delete account.refreshToken;
  }
  localStorage.setItem(STORAGE_KEY, JSON.stringify(safeState));
}

function savePreviewCredentials(accounts: Account[]) {
  const credentials = accounts.filter((account) => account.accessToken).map((account) => ({ id: account.id, accessToken: account.accessToken, refreshToken: account.refreshToken }));
  localStorage.setItem(CREDENTIALS_KEY, JSON.stringify(credentials));
}

async function invokeState(command: string, args?: Record<string, unknown>): Promise<AppState> {
  return mergeState(await invoke<Partial<AppState>>(command, args));
}

export async function getState(): Promise<AppState> {
  if (hasTauri()) {
    try { return await invokeState('get_app_state'); } catch { /* Use preview storage. */ }
  }
  return loadLocal();
}

export async function saveConfig(config: ServiceConfig): Promise<AppState> {
  if (hasTauri()) {
    try { return await invokeState('save_service_config', { config }); } catch (error) { throw new Error(String(error)); }
  }
  const state = loadLocal();
  state.config = { ...config, enabled: state.running };
  saveLocal(state);
  return state;
}

export async function startService(): Promise<AppState> {
  if (hasTauri()) return invokeState('start_service');
  const state = loadLocal();
  const credentials = JSON.parse(localStorage.getItem(CREDENTIALS_KEY) ?? '[]') as Array<{ id: string; accessToken?: string }>;
  const credentialIds = new Set(credentials.filter((item) => item.accessToken).map((item) => item.id));
  if (!state.accounts.some((account) => account.status !== 'disabled' && credentialIds.has(account.id))) {
    throw new Error('没有带有效 Token 的 CodeBuddy 中国站账号');
  }
  if (!state.keys.some((key) => key.enabled)) throw new Error('没有启用的 API Key');
  state.running = true;
  state.config.enabled = true;
  state.actualPort = state.config.port;
  state.lastError = null;
  saveLocal(state);
  return state;
}

export async function stopService(): Promise<AppState> {
  if (hasTauri()) return invokeState('stop_service');
  const state = loadLocal();
  state.running = false;
  state.config.enabled = false;
  state.actualPort = null;
  saveLocal(state);
  return state;
}

export async function saveAccounts(accounts: Account[]): Promise<AppState> {
  if (hasTauri()) return invokeState('save_accounts', { accounts });
  const state = loadLocal();
  state.accounts = accounts.map(({ accessToken: _accessToken, refreshToken: _refreshToken, ...account }) => account);
  savePreviewCredentials(accounts);
  saveLocal(state);
  return state;
}

export async function saveKeys(keys: ApiKey[]): Promise<AppState> {
  if (hasTauri()) return invokeState('save_api_keys', { keys });
  const state = loadLocal();
  state.keys = keys;
  saveLocal(state);
  return state;
}

export async function clearLogs(): Promise<AppState> {
  if (hasTauri()) return invokeState('clear_request_logs');
  const state = loadLocal();
  state.logs = [];
  saveLocal(state);
  return state;
}

export async function listModels(port = 11435, apiKey?: string): Promise<ModelInfo[]> {
  const response = await fetch(`http://127.0.0.1:${port}/v1/models`, {
    headers: apiKey ? { Authorization: `Bearer ${apiKey}` } : undefined,
  });
  if (!response.ok) throw new Error(`读取模型目录失败：HTTP ${response.status}`);
  const payload = await response.json() as { data?: ModelInfo[] };
  return payload.data ?? [];
}

// ModelSyncResult 是 sidecar 模型同步接口的返回结果。count 为后端实际拉取到的
// 模型数（0 表示失败），error 带具体失败原因（失败阶段 / HTTP 状态 / 业务码 /
// 使用的账号），界面必须按它展示结果，不能拿 /v1/models 的长度冒充同步结果。
export interface ModelSyncResult {
  count: number;
  refreshed: boolean;
  error?: string;
  attempts?: number;
  accountsTried?: number;
  accountId?: string;
  httpStatus?: number;
  bizCode?: number;
}

// syncModels 通知 sidecar 立即从 CodeBuddy CN 后端重新拉取模型清单并覆盖
// 本地缓存（POST /v1/coderelay/codebuddy/sync）。随后再调 listModels 读取
// 更新后的 /v1/models 目录。
export async function syncModels(port = 11435, apiKey?: string): Promise<ModelSyncResult> {
  const response = await fetch(`http://127.0.0.1:${port}/v1/coderelay/codebuddy/sync`, {
    method: 'POST',
    headers: apiKey ? { Authorization: `Bearer ${apiKey}` } : undefined,
  });
  if (!response.ok) throw new Error(`模型同步失败：HTTP ${response.status}`);
  const payload = await response.json() as Partial<ModelSyncResult> & { error?: string };
  const error = typeof payload.error === 'string' ? payload.error.trim() : '';
  return {
    count: typeof payload.count === 'number' ? payload.count : 0,
    refreshed: Boolean(payload.refreshed),
    error: error || undefined,
    attempts: payload.attempts,
    accountsTried: payload.accountsTried,
    accountId: payload.accountId,
    httpStatus: payload.httpStatus,
    bizCode: payload.bizCode,
  };
}

function requireTauri(feature: string) {
  if (!hasTauri()) throw new Error(`${feature}需要桌面端环境，浏览器预览中不可用`);
}

export async function startOAuth(): Promise<OAuthStartResponse> {
  requireTauri('浏览器认证');
  return invoke<OAuthStartResponse>('codebuddy_oauth_start');
}

export async function completeOAuth(loginId: string): Promise<OAuthCompleteResponse> {
  requireTauri('浏览器认证');
  return invoke<OAuthCompleteResponse>('codebuddy_oauth_complete', { loginId });
}

export async function cancelOAuth(loginId: string): Promise<void> {
  if (!hasTauri()) return;
  await invoke('codebuddy_oauth_cancel', { loginId });
}

export async function validateToken(accessToken: string): Promise<OAuthCompleteResponse> {
  requireTauri('Token 验证');
  return invoke<OAuthCompleteResponse>('codebuddy_validate_token', { accessToken });
}

export async function openExternal(url: string): Promise<void> {
  requireTauri('打开系统浏览器');
  await openUrl(url);
}

// ---------------------------------------------------------------------------
// Cursor 服务（cursor-bridge sidecar）
//
// bridge 的控制 API 走 HTTP，但端口只由后端知道（启动时由 bridge 的 ready 行
// 上报），前端因此不直接 fetch bridge，而是走下面这组 Tauri 命令。这样做的
// 直接收益：前端不必知道端口，也就不存在 CORS 与端口漂移问题——所有 HTTP 调用
// 都由 Rust 侧发出。
// ---------------------------------------------------------------------------

export async function getCursorBridgeStatus(): Promise<CursorBridgeStatus> {
  requireTauri('Cursor 服务');
  return invoke<CursorBridgeStatus>('cursor_bridge_status');
}

export async function startCursorBridge(): Promise<CursorBridgeStatus> {
  requireTauri('Cursor 服务');
  return invoke<CursorBridgeStatus>('cursor_bridge_start');
}

export async function stopCursorBridge(): Promise<CursorBridgeStatus> {
  requireTauri('Cursor 服务');
  return invoke<CursorBridgeStatus>('cursor_bridge_stop');
}

export async function initCursorBridgeCa(): Promise<CursorBridgeStatus> {
  requireTauri('Cursor 证书管理');
  return invoke<CursorBridgeStatus>('cursor_bridge_init_ca');
}

export async function getCursorBridgeInstallCommand(): Promise<string | null> {
  requireTauri('Cursor 证书管理');
  return invoke<string | null>('cursor_bridge_install_command');
}

export async function setCursorBridgeEnabled(enabled: boolean): Promise<CursorBridgeStatus> {
  requireTauri('Cursor 注入');
  return invoke<CursorBridgeStatus>('cursor_bridge_set_enabled', { enabled });
}

export async function saveCursorBridgeBindings(bindings: CursorBinding[]): Promise<CursorBridgeStatus> {
  requireTauri('Cursor 绑定');
  return invoke<CursorBridgeStatus>('cursor_bridge_save_bindings', { bindings });
}

export async function saveCursorBridgePreferences(preferences: CursorBridgePreferences): Promise<CursorBridgeStatus> {
  requireTauri('Cursor 设置');
  return invoke<CursorBridgeStatus>('cursor_bridge_save_preferences', { preferences });
}

/**
 * 检查 GitHub 上的最新发布。
 *
 * 失败（断网、限流、仓库改址）会抛错，界面必须把错误展示为「检测失败」而不是
 * 「已是最新」——否则用户会误以为更新功能正常。
 */
export async function checkForUpdate(): Promise<UpdateCheckResult> {
  requireTauri('检测更新');
  return invoke<UpdateCheckResult>('check_for_update');
}

export interface RefreshAllResponse {
  state: AppState;
  refreshed: number;
  failed: number;
  skipped: number;
}

export async function refreshAccountQuota(accountId: string): Promise<AppState> {
  requireTauri('刷新额度');
  return invokeState('refresh_account_quota', { accountId });
}

export async function refreshAllQuotas(): Promise<RefreshAllResponse> {
  requireTauri('刷新额度');
  const response = await invoke<{ state: Partial<AppState>; refreshed: number; failed: number; skipped: number }>('refresh_all_quotas');
  return {
    state: mergeState(response.state),
    refreshed: response.refreshed,
    failed: response.failed,
    skipped: response.skipped,
  };
}

// exportAccounts 将指定账号（含凭据 token）导出为 JSON，弹出系统「另存为」
// 对话框由用户选择保存位置。返回实际保存路径，用户取消时返回 null。
export async function exportAccounts(accountIds: string[], defaultFileName: string): Promise<string | null> {
  requireTauri('导出账号');
  return invoke<string | null>('export_accounts', { accountIds, defaultFileName });
}

export async function getCheckinStatus(accountId: string): Promise<CheckinStatusResponse> {
  requireTauri('签到');
  return invoke<CheckinStatusResponse>('codebuddy_checkin_status', { accountId });
}

export async function checkinAccount(accountId: string): Promise<CheckinResponse> {
  requireTauri('签到');
  return invoke<CheckinResponse>('codebuddy_checkin', { accountId });
}

export function resetLocalState() {
  localStorage.removeItem(STORAGE_KEY);
  localStorage.removeItem(CREDENTIALS_KEY);
  window.location.reload();
}

export { SERVICE_URL, CREDENTIALS_KEY };

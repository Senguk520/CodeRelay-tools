export type PageId = 'overview' | 'service' | 'keys' | 'logs' | 'accounts' | 'models' | 'cursor' | 'settings';
export type ThemeMode = 'light' | 'dark' | 'system';
export type ServiceScope = 'localhost' | 'lan';
export type RoutingStrategy = 'auto' | 'random' | 'single_account' | 'quota_high_first' | 'custom';
export type AccountStatus = 'available' | 'needs_auth' | 'cooling' | 'restricted' | 'disabled';

export interface Account {
  id: string;
  email: string;
  region: 'cn';
  plan: string;
  status: AccountStatus;
  quota: number;
  quotaTotal: number;
  lastUsed: number | null;
  failures: number;
  tags: string[];
  accessToken?: string;
  refreshToken?: string;
  uid?: string;
  enterpriseId?: string;
  domain?: string;
  lastCheckin?: number | null;
  checkinStreak?: number;
}

export interface ApiKey {
  id: string;
  name: string;
  key: string;
  enabled: boolean;
  accountIds: string[] | null;
  models: string[];
  createdAt: number;
  lastUsed: number | null;
}

export interface CheckinStatusResponse {
  todayCheckedIn: boolean;
  active: boolean;
  streakDays: number;
  dailyCredit: number;
  todayCredit?: number | null;
  nextStreakDay?: number | null;
  isStreakDay?: boolean | null;
  checkinDates?: string[] | null;
  streakBonusDays?: number | null;
  streakBonusCredit?: number | null;
}

export interface CheckinResponse {
  success: boolean;
  message?: string | null;
  reward?: unknown;
  credit?: number | null;
  streakDays?: number | null;
  isStreakDay?: boolean | null;
  nextCheckinIn?: number | null;
}

export interface ServiceConfig {
  enabled: boolean;
  port: number;
  bindHost: string;
  scope: ServiceScope;
  requestTimeoutMs: number;
  maxRetries: number;
  routingStrategy: RoutingStrategy;
  sessionAffinity: boolean;
  imageGenerationMode: 'enabled' | 'images_only' | 'disabled';
  debugLogs: boolean;
}

export interface RequestLog {
  requestId: string;
  timestamp: number;
  method: string;
  path: string;
  model: string;
  accountId: string;
  apiKeyId: string;
  status: number;
  success: boolean;
  latencyMs: number;
  inputTokens: number;
  outputTokens: number;
  credit: number;
  cacheHit: boolean;
  error?: string;
}

export interface HourBucket {
  label: string;
  hit: number;
  miss: number;
}

/** 单日聚合统计（也用作累计视图的数据结构）。 */
export interface DayStats {
  requestCount: number;
  totalTokens: number;
  cacheHitTokens: number;
  credit: number;
  successCount: number;
  failureCount: number;
  totalLatencyMs: number;
  byHour: HourBucket[];
}

export interface Stats {
  requestCount: number;
  totalTokens: number;
  cacheHitTokens: number;
  credit: number;
  averageLatencyMs: number;
  successCount: number;
  failureCount: number;
  byHour: HourBucket[];
  /** 按天聚合：本地日期(yyyy-MM-dd) -> 当日数据。 */
  byDay: Record<string, DayStats>;
  /** 累计所有天的总数据。 */
  lifetime: DayStats;
}

export interface OAuthStartResponse {
  loginId: string;
  verificationUri: string;
  expiresIn: number;
  intervalSeconds: number;
}

export interface OAuthCompleteResponse {
  email: string;
  uid?: string;
  enterpriseId?: string;
  accessToken: string;
  refreshToken?: string;
  expiresAt?: number;
  domain?: string;
}

export interface ModelInfo {
  id: string;
  object?: string;
  ownedBy?: string;
  created?: number;
  inputModalities?: string[];
  supportsImages?: boolean;
  supportsToolCall?: boolean;
  contextLength?: number;
  maxCompletionTokens?: number;
}

export interface AppState {
  config: ServiceConfig;
  accounts: Account[];
  keys: ApiKey[];
  logs: RequestLog[];
  stats: Stats;
  running: boolean;
  actualPort: number | null;
  lastError: string | null;
  /**
   * 只读派生值：局域网可连接地址（形如 `http://192.168.1.23:11435`，不含 `/v1`）。
   * 仅当访问范围为 `lan` 且反代成功解析到本机网卡地址时才有值；不参与持久化，
   * 也不会写进 state.json。
   */
  lanBaseUrl: string | null;
  /**
   * 只读派生值：cursor-bridge sidecar 当前监听的端口；bridge 未运行时为 null。
   * 与 lanBaseUrl 同样不参与持久化，也不会写进 state.json。
   */
  cursorBridgePort: number | null;
}

/**
 * Cursor 服务的一条「绑定」：把一个 API Key 和一个模型配对，使其出现在
 * Cursor 的模型选择器中。不含 relay 地址——该地址由后端在同步时从 relay 的
 * 实际端口派生，因此端口漂移不会在配置里留下过期 URL。
 */
export interface CursorBinding {
  id: string;
  keyId: string;
  modelId: string;
  /** 留空时后端回落为 modelId；Cursor 中显示的名称。 */
  displayName: string;
  remark: string;
  /** 空字符串表示不设置；否则为 one of low/medium/high/xhigh/max。 */
  reasoningEffort: string;
  extraParams?: Record<string, unknown> | null;
  contextWindowTokens?: number | null;
  maxOutputTokens?: number | null;
}

/** cursor-bridge 的偏好设置，由「设置 → Cursor」页维护，持久化在 CodeRelay 侧。 */
export interface CursorBridgePreferences {
  servicePort: number;
  proxyPort: number;
  proxyMode: 'default' | 'custom' | string;
  proxyAddress: string;
  proxyAuthEnabled: boolean;
  proxyUsername: string;
  /** 留空表示保留既有密码，不会清空。 */
  proxyPassword: string;
  /** 留空表示「直连」，即直接转发 Cursor 自己的提交请求。 */
  commitModelId: string;
  commitPrompt: string;
}

/** bridge 当前持有的一个模型行，供「Commit 提交代码模型」下拉使用。 */
export interface CursorBridgeModel {
  /** Commit 设置按 model_hash 索引模型，下拉必须提供该值。 */
  modelHash: string;
  modelId: string;
  displayName: string;
}

/** `cursor_bridge_status` 的返回。 */
export interface CursorBridgeStatus {
  running: boolean;
  port: number | null;
  /** missing / untrusted / ready / invalid / unknown。 */
  ca: string;
  /** disabled / enabled / degraded / unknown。 */
  integration: string;
  settingsApplied: boolean;
  /**
   * 用户显式开启注入的意图，由 CodeRelay 侧持久化并在重启后据此重新挂载。
   *
   * 与 `integration` 的区别：后者是 bridge 此刻的实际情况，前者是用户的选择。
   * UI 的开关跟这个值走，这样重启后开关不会因为 bridge 还没重挂而回弹成「关闭」。
   */
  takeoverRequested: boolean;
  configuredModels: number;
  /** bridge 当前持有的模型行，供 Commit 模型下拉使用。 */
  models: CursorBridgeModel[];
  bindings: CursorBinding[];
  preferences: CursorBridgePreferences;
  installCommand: string | null;
  /** 与安装命令配对的卸载命令：把根证书从信任库里撤下来。 */
  uninstallCommand: string | null;
  proxyUrl: string | null;
  /**
   * bridge 是否持有出站代理密码。
   *
   * `null` 表示 bridge 不可达、答案**未知**，而不是「没有密码」——把未知显示成
   * 「未设置」会让用户据此做出错误判断。密码本身永不下发。
   */
  hasProxyPassword: boolean | null;
  /**
   * 上一次为应用代理设置而关闭 Cursor 是否失败。
   *
   * 失败时页面需要明确告诉用户：注入可能还没生效，因为 Cursor 仍在用旧设置运行。
   * 它同时也是「账号库写入被拒绝」的信号。
   */
  cursorTerminateFailed: boolean;
  lastError: string | null;
  /** 因 API Key 缺失或停用而被跳过的绑定显示名。 */
  unresolvedBindings: string[];
  /** bridge 内置的 Commit 提示词，「恢复默认」需要它。 */
  commitDefaultPrompt: string | null;
}

export const defaultCursorBridgePreferences: CursorBridgePreferences = {
  servicePort: 0,
  proxyPort: 0,
  proxyMode: 'default',
  proxyAddress: '',
  proxyAuthEnabled: false,
  proxyUsername: '',
  proxyPassword: '',
  commitModelId: '',
  commitPrompt: '',
};

export const defaultCursorBridgeStatus: CursorBridgeStatus = {
  running: false,
  port: null,
  ca: 'unknown',
  integration: 'unknown',
  settingsApplied: false,
  takeoverRequested: false,
  configuredModels: 0,
  models: [],
  bindings: [],
  preferences: defaultCursorBridgePreferences,
  installCommand: null,
  uninstallCommand: null,
  proxyUrl: null,
  hasProxyPassword: null,
  cursorTerminateFailed: false,
  lastError: null,
  unresolvedBindings: [],
  commitDefaultPrompt: null,
};


/** 安装包下载地址的来源。`asset` 为 GitHub Release 附件，`releaseBody` 为说明正文里的历史链接。 */
export type InstallerSource = 'asset' | 'releaseBody';

/**
 * 更新检查结果。属于会话态数据，不写进 localStorage / state.json，
 * 每次检测都由后端重新下发。
 */
export interface UpdateCheckResult {
  /** 当前运行版本（来自后端编译期版本，无 `v` 前缀）。 */
  currentVersion: string;
  /** 最新发布版本（tag 去掉 `v` 前缀）。 */
  latestVersion: string;
  hasUpdate: boolean;
  /** 发布页地址，「打开发布页」按钮的目标。 */
  releaseUrl: string;
  releaseName?: string;
  /** 发布说明正文（Markdown 原文）。 */
  releaseNotes?: string;
  publishedAt?: string;
  prerelease: boolean;
  /** 安装包直链；解析不到时界面只提供「打开发布页」。 */
  installerUrl?: string;
  installerName?: string;
  installerSize?: number;
  installerSource?: InstallerSource;
}

export const defaultConfig: ServiceConfig = {
  enabled: false,
  port: 11435,
  bindHost: '127.0.0.1',
  scope: 'localhost',
  requestTimeoutMs: 120000,
  maxRetries: 2,
  routingStrategy: 'auto',
  sessionAffinity: true,
  imageGenerationMode: 'enabled',
  debugLogs: false,
};

function emptyByHour(): HourBucket[] {
  return Array.from({ length: 8 }, (_, index) => ({ label: String(index * 3).padStart(2, '0'), hit: 0, miss: 0 }));
}

/** 创建一个空的单日聚合统计。 */
export function emptyDayStats(): DayStats {
  return {
    requestCount: 0,
    totalTokens: 0,
    cacheHitTokens: 0,
    credit: 0,
    successCount: 0,
    failureCount: 0,
    totalLatencyMs: 0,
    byHour: emptyByHour(),
  };
}

export const defaultStats: Stats = {
  requestCount: 0,
  totalTokens: 0,
  cacheHitTokens: 0,
  credit: 0,
  averageLatencyMs: 0,
  successCount: 0,
  failureCount: 0,
  byHour: emptyByHour(),
  byDay: {},
  lifetime: emptyDayStats(),
};

export const defaultState: AppState = {
  config: defaultConfig,
  accounts: [],
  keys: [],
  logs: [],
  stats: defaultStats,
  running: false,
  actualPort: null,
  lastError: null,
  // 派生值：预览模式（无 Tauri）下没有真实网卡，保持空；正式运行时由后端下发。
  lanBaseUrl: null,
  // 派生值：预览模式没有 cursor-bridge 进程，恒为 null。
  cursorBridgePort: null,
};

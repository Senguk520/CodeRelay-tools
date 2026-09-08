export type PageId = 'overview' | 'service' | 'keys' | 'logs' | 'accounts' | 'models' | 'settings';
export type ThemeMode = 'light' | 'dark' | 'system';
export type ServiceScope = 'localhost' | 'lan';
export type RoutingStrategy = 'auto' | 'random' | 'single_account' | 'quota_high_first' | 'custom';
export type VisionMode = 'off' | 'routing' | 'preprocess' | 'agentic';
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
  visionToolEnabled: boolean;
  visionMode: VisionMode;
  visionModel: string;
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
  visionToolEnabled: true,
  visionMode: 'preprocess',
  visionModel: 'hy4-preview',
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
};

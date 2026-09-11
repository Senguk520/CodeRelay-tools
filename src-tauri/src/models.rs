use chrono::{Local, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 小时桶数量：8 个桶，每桶覆盖 3 小时（00/03/.../21），合计 24 小时。
pub const HOUR_BUCKETS: usize = 8;
/// 按天聚合数据的历史保留天数（滚动窗口）。
pub const BY_DAY_KEEP_DAYS: usize = 90;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub region: String,
    pub plan: String,
    pub status: String,
    pub quota: f64,
    pub quota_total: f64,
    pub last_used: Option<i64>,
    pub failures: u32,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enterprise_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checkin: Option<i64>,
    #[serde(default)]
    pub checkin_streak: u32,
    // 兼容旧版 state.json。新的保存流程会在落盘前清除此字段，凭据仅写入 credentials.json。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            id: String::new(),
            email: String::new(),
            region: "cn".to_string(),
            plan: "FREE".to_string(),
            status: "needs_auth".to_string(),
            quota: 0.0,
            quota_total: 0.0,
            last_used: None,
            failures: 0,
            tags: Vec::new(),
            uid: None,
            enterprise_id: None,
            domain: None,
            last_checkin: None,
            checkin_streak: 0,
            access_token: None,
            refresh_token: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountCredential {
    pub account_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
}

impl Default for AccountCredential {
    fn default() -> Self {
        Self {
            account_id: String::new(),
            access_token: String::new(),
            refresh_token: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub key: String,
    pub enabled: bool,
    pub account_ids: Option<Vec<String>>,
    pub models: Vec<String>,
    pub created_at: i64,
    pub last_used: Option<i64>,
}

impl Default for ApiKey {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            key: String::new(),
            enabled: true,
            account_ids: None,
            models: Vec::new(),
            created_at: 0,
            last_used: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServiceConfig {
    pub enabled: bool,
    pub port: u16,
    pub bind_host: String,
    pub scope: String,
    pub request_timeout_ms: u64,
    pub max_retries: u8,
    pub routing_strategy: String,
    pub session_affinity: bool,
    pub vision_tool_enabled: bool,
    pub vision_mode: String,
    pub vision_model: String,
    pub image_generation_mode: String,
    pub debug_logs: bool,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 11435,
            bind_host: "127.0.0.1".to_string(),
            scope: "localhost".to_string(),
            request_timeout_ms: 120_000,
            max_retries: 2,
            routing_strategy: "auto".to_string(),
            session_affinity: true,
            vision_tool_enabled: true,
            vision_mode: "preprocess".to_string(),
            vision_model: "hy4-preview".to_string(),
            image_generation_mode: "enabled".to_string(),
            debug_logs: false,
        }
    }
}

/// 生成空的小时桶（8 个桶，标签为 00/03/.../21）。
fn empty_by_hour() -> Vec<HourStats> {
    (0..HOUR_BUCKETS)
        .map(|bucket| HourStats {
            label: format!("{:02}", bucket * 3),
            hit: 0,
            miss: 0,
        })
        .collect()
}

/// 将 i64 差量累加到 u64，防止下溢/溢出。
fn add_i64_to_u64(base: u64, delta: i64) -> u64 {
    if delta >= 0 {
        base.saturating_add(delta as u64)
    } else {
        base.saturating_sub((-delta) as u64)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HourStats {
    pub label: String,
    pub hit: u64,
    pub miss: u64,
}

/// 单日聚合统计（也用作累计视图的数据结构）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DayStats {
    pub request_count: u64,
    pub total_tokens: u64,
    pub cache_hit_tokens: u64,
    pub credit: f64,
    pub success_count: u64,
    pub failure_count: u64,
    /// 当日所有成功/失败请求的延迟总和（用于计算平均延迟）。
    pub total_latency_ms: u64,
    pub by_hour: Vec<HourStats>,
}

impl DayStats {
    /// 保证 by_hour 为固定 8 桶，反序列化旧数据或空数据时兜底。
    pub fn ensure_by_hour(&mut self) {
        if self.by_hour.len() != HOUR_BUCKETS {
            self.by_hour = empty_by_hour();
        }
    }

    /// 依据本地时区计算时间戳所属的小时桶（0..8）。
    fn hour_bucket(ts_ms: i64) -> usize {
        let hour = Local
            .timestamp_millis_opt(ts_ms)
            .single()
            .map(|dt| dt.hour() as usize)
            .unwrap_or(0);
        (hour / 3).min(HOUR_BUCKETS - 1)
    }

    /// 累加一次"请求完成"：请求数、延迟、成败计数、小时桶。
    fn add_completed(&mut self, log: &RequestLog) {
        self.ensure_by_hour();
        self.request_count = self.request_count.saturating_add(1);
        self.total_latency_ms = self.total_latency_ms.saturating_add(log.latency_ms);
        if log.success {
            self.success_count = self.success_count.saturating_add(1);
        } else {
            self.failure_count = self.failure_count.saturating_add(1);
        }
        let bucket = Self::hour_bucket(log.timestamp);
        if log.cache_hit {
            self.by_hour[bucket].hit = self.by_hour[bucket].hit.saturating_add(1);
        } else {
            self.by_hour[bucket].miss = self.by_hour[bucket].miss.saturating_add(1);
        }
    }

    /// 累加一次"用量"差量：token、缓存命中 token、credit。
    fn add_usage_delta(&mut self, delta_tokens: i64, delta_cache_hit_tokens: i64, delta_credit: f64) {
        self.ensure_by_hour();
        self.total_tokens = add_i64_to_u64(self.total_tokens, delta_tokens);
        self.cache_hit_tokens = add_i64_to_u64(self.cache_hit_tokens, delta_cache_hit_tokens);
        self.credit += delta_credit;
    }

    /// 缓存命中状态在 usage 事件中才确定（晚于 request_completed 到达）。
    /// 完成时 cache_hit 恒为 false（小时桶先按 miss 计数），
    /// 命中状态变化时把小时桶中的一次请求从 miss 挪到 hit（或反向）。
    fn reclassify_hour_hit(&mut self, ts_ms: i64, hit: bool) {
        self.ensure_by_hour();
        let bucket = Self::hour_bucket(ts_ms);
        if hit {
            self.by_hour[bucket].miss = self.by_hour[bucket].miss.saturating_sub(1);
            self.by_hour[bucket].hit = self.by_hour[bucket].hit.saturating_add(1);
        } else {
            self.by_hour[bucket].hit = self.by_hour[bucket].hit.saturating_sub(1);
            self.by_hour[bucket].miss = self.by_hour[bucket].miss.saturating_add(1);
        }
    }

    /// 成败状态在 usage 事件中才最终确定（流式请求 HTTP 200 开流后仍可能失败）。
    /// 完成时按 HTTP 状态先记成功，usage 事件发现失败时把一次计数从成功挪到失败（或反向）。
    fn reclassify_success(&mut self, success: bool) {
        self.ensure_by_hour();
        if success {
            self.failure_count = self.failure_count.saturating_sub(1);
            self.success_count = self.success_count.saturating_add(1);
        } else {
            self.success_count = self.success_count.saturating_sub(1);
            self.failure_count = self.failure_count.saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageStats {
    // —— "今天"快照（平铺字段，前端默认渲染），由 by_day[今天] 同步而来 ——
    pub request_count: u64,
    pub total_tokens: u64,
    pub cache_hit_tokens: u64,
    pub credit: f64,
    pub average_latency_ms: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub by_hour: Vec<HourStats>,
    // —— 按天聚合：本地日期(yyyy-MM-dd) -> 当日数据 ——
    pub by_day: BTreeMap<String, DayStats>,
    // —— 累计所有天的总数据 ——
    pub lifetime: DayStats,
    // —— 旧版累计字段，仅用于从旧 state.json 迁移读取，序列化时不再写入 ——
    #[serde(default, skip_serializing)]
    pub lifetime_requests: u64,
    #[serde(default, skip_serializing)]
    pub lifetime_tokens: u64,
    #[serde(default, skip_serializing)]
    pub lifetime_cache_hit_tokens: u64,
    #[serde(default, skip_serializing)]
    pub lifetime_credit: f64,
    #[serde(default, skip_serializing)]
    pub lifetime_success: u64,
    #[serde(default, skip_serializing)]
    pub lifetime_failure: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RequestLog {
    pub request_id: String,
    pub timestamp: i64,
    pub method: String,
    pub path: String,
    pub model: String,
    pub account_id: String,
    pub api_key_id: String,
    pub status: u16,
    pub success: bool,
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub credit: f64,
    pub cache_hit: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppState {
    pub config: ServiceConfig,
    pub accounts: Vec<Account>,
    pub keys: Vec<ApiKey>,
    pub logs: Vec<RequestLog>,
    pub stats: UsageStats,
    pub running: bool,
    pub actual_port: Option<u16>,
    pub last_error: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            config: ServiceConfig::default(),
            accounts: Vec::new(),
            keys: Vec::new(),
            logs: Vec::new(),
            stats: UsageStats::default(),
            running: false,
            actual_port: None,
            last_error: None,
        }
    }
}

impl AppState {
    fn now_ms() -> i64 {
        Local::now().timestamp_millis()
    }

    /// 本地时区今天 0 点的时间戳（毫秒）。
    fn today_start_ms() -> i64 {
        let today = Local::now().date_naive();
        today
            .and_hms_opt(0, 0, 0)
            .and_then(|dt| Local.from_local_datetime(&dt).single())
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0)
    }

    /// 依据本地时区把毫秒时间戳映射为日期 key（yyyy-MM-dd）。
    pub fn local_date_key(ts_ms: i64) -> String {
        Local
            .timestamp_millis_opt(ts_ms)
            .single()
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    }

    /// 只保留时间戳属于今天（本地时区）的请求日志，用于跨天惰性清零。
    pub fn retain_today_logs(&mut self) {
        let start = Self::today_start_ms();
        self.logs.retain(|log| log.timestamp >= start);
    }

    /// 把 by_day[今天] 的聚合同步到平铺"今天"快照字段，供前端默认渲染。
    pub fn sync_today_snapshot(&mut self) {
        let key = Self::local_date_key(Self::now_ms());
        let mut today = self.stats.by_day.get(&key).cloned().unwrap_or_default();
        today.ensure_by_hour();
        self.stats.request_count = today.request_count;
        self.stats.total_tokens = today.total_tokens;
        self.stats.cache_hit_tokens = today.cache_hit_tokens;
        self.stats.credit = today.credit;
        self.stats.success_count = today.success_count;
        self.stats.failure_count = today.failure_count;
        self.stats.by_hour = today.by_hour;
        self.stats.average_latency_ms = if today.request_count > 0 {
            today.total_latency_ms / today.request_count
        } else {
            0
        };
    }

    /// 记录一次"请求完成"：累加请求数、延迟、成败、小时桶到 当日/累计，并同步今天快照。
    pub fn record_completed(&mut self, log: &RequestLog) {
        let key = Self::local_date_key(log.timestamp);
        self.stats.by_day.entry(key).or_default().add_completed(log);
        self.stats.lifetime.add_completed(log);
        self.prune_old_days();
        self.sync_today_snapshot();
    }

    /// 记录一次"用量"：按 (新值 - 旧值) 差量累加 token/缓存/credit 到 当日/累计，并同步今天快照。
    /// request_completed 先于 usage 到达：完成时 cache_hit 恒为 false（小时桶先记 miss），
    /// 因此命中状态变化时还需同步修正小时桶的 hit/miss 计数。
    pub fn record_usage(&mut self, log: &RequestLog, prev: &RequestLog) {
        let new_tokens = log.input_tokens.saturating_add(log.output_tokens) as i64;
        let prev_tokens = prev.input_tokens.saturating_add(prev.output_tokens) as i64;
        let delta_tokens = new_tokens - prev_tokens;
        let new_cache = if log.cache_hit { log.input_tokens } else { 0 } as i64;
        let prev_cache = if prev.cache_hit { prev.input_tokens } else { 0 } as i64;
        let delta_cache = new_cache - prev_cache;
        let delta_credit = log.credit - prev.credit;
        let key = Self::local_date_key(log.timestamp);
        let hit_changed = log.cache_hit != prev.cache_hit;
        if hit_changed {
            self.stats
                .by_day
                .entry(key.clone())
                .or_default()
                .reclassify_hour_hit(log.timestamp, log.cache_hit);
            self.stats
                .lifetime
                .reclassify_hour_hit(log.timestamp, log.cache_hit);
        }
        // usage 事件可能携带最终成败结果（流式请求中途失败时 request_completed 仍为 200）。
        // 成败变化时同步修正当日/累计的成功与失败计数。
        let success_changed = log.success != prev.success;
        if success_changed {
            self.stats
                .by_day
                .entry(key.clone())
                .or_default()
                .reclassify_success(log.success);
            self.stats
                .lifetime
                .reclassify_success(log.success);
        }
        if delta_tokens == 0
            && delta_cache == 0
            && delta_credit.abs() < f64::EPSILON
            && !hit_changed
            && !success_changed
        {
            return;
        }
        self.stats
            .by_day
            .entry(key)
            .or_default()
            .add_usage_delta(delta_tokens, delta_cache, delta_credit);
        self.stats
            .lifetime
            .add_usage_delta(delta_tokens, delta_cache, delta_credit);
        self.prune_old_days();
        self.sync_today_snapshot();
    }

    /// 滚动保留最近 BY_DAY_KEEP_DAYS 天的按天聚合数据。
    fn prune_old_days(&mut self) {
        let len = self.stats.by_day.len();
        if len > BY_DAY_KEEP_DAYS {
            let overflow = len - BY_DAY_KEEP_DAYS;
            let keys: Vec<String> = self.stats.by_day.keys().take(overflow).cloned().collect();
            for key in keys {
                self.stats.by_day.remove(&key);
            }
        }
    }

    /// 从旧版 lifetime_* 平铺字段迁移到新的 lifetime DayStats（一次性），并清零旧字段。
    pub fn migrate_legacy(&mut self) {
        let legacy_requests = self.stats.lifetime_requests;
        let legacy_tokens = self.stats.lifetime_tokens;
        let legacy_cache_hit = self.stats.lifetime_cache_hit_tokens;
        let legacy_credit = self.stats.lifetime_credit;
        let legacy_success = self.stats.lifetime_success;
        let legacy_failure = self.stats.lifetime_failure;
        if self.stats.lifetime.request_count == 0
            && self.stats.lifetime.total_tokens == 0
            && (legacy_requests > 0 || legacy_tokens > 0)
        {
            self.stats.lifetime.request_count = legacy_requests;
            self.stats.lifetime.total_tokens = legacy_tokens;
            self.stats.lifetime.cache_hit_tokens = legacy_cache_hit;
            self.stats.lifetime.credit = legacy_credit;
            self.stats.lifetime.success_count = legacy_success;
            self.stats.lifetime.failure_count = legacy_failure;
        }
        self.stats.lifetime_requests = 0;
        self.stats.lifetime_tokens = 0;
        self.stats.lifetime_cache_hit_tokens = 0;
        self.stats.lifetime_credit = 0.0;
        self.stats.lifetime_success = 0;
        self.stats.lifetime_failure = 0;
        self.stats.lifetime.ensure_by_hour();
        self.sync_today_snapshot();
    }

    pub fn sanitize_for_persistence(&mut self) {
        self.running = false;
        self.actual_port = None;
        for account in &mut self.accounts {
            account.access_token = None;
            account.refresh_token = None;
        }
    }
}

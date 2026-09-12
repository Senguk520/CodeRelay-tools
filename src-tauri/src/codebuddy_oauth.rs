use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const API_ENDPOINT: &str = "https://www.codebuddy.cn";
const API_PREFIX: &str = "/v2/plugin";
const BILLING_PREFIX: &str = "/v2/billing/meter";
const POLL_INTERVAL: Duration = Duration::from_millis(1500);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
// 计费网关校验 User-Agent 存在性，缺失会返回 403（code 10085）。
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36";
const PACKAGE_PRO_MONTH: &str = "TCACA_code_002_AkiJS3ZHF5";
const PACKAGE_PRO_YEAR: &str = "TCACA_code_003_FAnt7lcmRT";
const PACKAGE_ENTERPRISE: &str = "TCACA_code_enterprise";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthStartResponse {
    pub login_id: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthCompleteResponse {
    pub email: String,
    pub uid: Option<String>,
    pub enterprise_id: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub domain: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingLogin {
    login_id: String,
    state: String,
    expires_at: std::time::Instant,
    cancelled: bool,
}

fn pending_login() -> &'static Mutex<Option<PendingLogin>> {
    static VALUE: OnceLock<Mutex<Option<PendingLogin>>> = OnceLock::new();
    VALUE.get_or_init(|| Mutex::new(None))
}

fn client() -> Result<Client, String> {
    Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(30)).build().map_err(|error| format!("创建 CodeBuddy HTTP 客户端失败：{error}"))
}

fn decode_jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.get("exp").and_then(Value::as_i64)
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| value.get(*name).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string))
}

pub async fn start_login() -> Result<OAuthStartResponse, String> {
    let response = client()?.post(format!("{API_ENDPOINT}{API_PREFIX}/auth/state?platform=ide")).json(&json!({})).send().await.map_err(|error| format!("请求 CodeBuddy 登录状态失败：{error}"))?;
    let body: Value = response.json().await.map_err(|error| format!("解析 CodeBuddy 登录状态失败：{error}"))?;
    let data = body.get("data").ok_or_else(|| "登录状态响应缺少 data".to_string())?;
    let state = string_field(data, &["state"]).ok_or_else(|| "登录状态响应缺少 state".to_string())?;
    let login_id = format!("cb-{}", uuid::Uuid::new_v4());
    let verification_uri = string_field(data, &["authUrl", "auth_url", "url"]).unwrap_or_else(|| format!("{API_ENDPOINT}/login?state={state}"));
    *pending_login().lock().map_err(|_| "登录状态锁不可用".to_string())? = Some(PendingLogin { login_id: login_id.clone(), state, expires_at: std::time::Instant::now() + LOGIN_TIMEOUT, cancelled: false });
    Ok(OAuthStartResponse { login_id, verification_uri, expires_in: LOGIN_TIMEOUT.as_secs(), interval_seconds: POLL_INTERVAL.as_secs() + 1 })
}

pub async fn cancel_login(login_id: &str) -> Result<(), String> {
    let mut pending = pending_login().lock().map_err(|_| "登录状态锁不可用".to_string())?;
    if let Some(current) = pending.as_mut() {
        if current.login_id == login_id { current.cancelled = true; }
    }
    Ok(())
}

async fn fetch_account_info(client: &Client, access_token: &str, state: &str, domain: Option<&str>) -> Result<(Option<String>, String, Option<String>), String> {
    let mut request = client.get(format!("{API_ENDPOINT}{API_PREFIX}/login/account?state={state}")).bearer_auth(access_token);
    if let Some(domain) = domain.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-Domain", domain);
    }
    let response = request.send().await.map_err(|error| format!("请求 CodeBuddy 账号信息失败：{error}"))?;
    let body: Value = response.json().await.map_err(|error| format!("解析 CodeBuddy 账号信息失败：{error}"))?;
    let data = body.get("data").cloned().unwrap_or_else(|| json!({}));
    let uid = string_field(&data, &["uid", "userId", "user_id"]);
    let email = string_field(&data, &["email", "accountEmail", "account_email"])
        .or_else(|| string_field(&data, &["nickname"]))
        .or_else(|| uid.clone())
        .unwrap_or_default();
    let enterprise_id = string_field(&data, &["enterpriseId", "enterprise_id"]);
    Ok((uid, email, enterprise_id))
}

pub async fn complete_login(login_id: &str) -> Result<OAuthCompleteResponse, String> {
    let client = client()?;
    loop {
        let current = pending_login().lock().map_err(|_| "登录状态锁不可用".to_string())?.clone().ok_or_else(|| "没有待处理的 CodeBuddy 登录".to_string())?;
        if current.login_id != login_id { return Err("登录请求 ID 不匹配".to_string()); }
        if current.cancelled {
            *pending_login().lock().map_err(|_| "登录状态锁不可用".to_string())? = None;
            return Err("登录已取消".to_string());
        }
        if std::time::Instant::now() >= current.expires_at {
            *pending_login().lock().map_err(|_| "登录状态锁不可用".to_string())? = None;
            return Err("CodeBuddy 登录已超时，请重新发起认证".to_string());
        }
        let response = client.get(format!("{API_ENDPOINT}{API_PREFIX}/auth/token?state={}", current.state)).send().await;
        if let Ok(response) = response {
            if let Ok(body) = response.json::<Value>().await {
                let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
                if code == 0 || code == 200 {
                    let data = body.get("data").cloned().unwrap_or_else(|| json!({}));
                    let access_token = string_field(&data, &["accessToken", "access_token", "token"]).unwrap_or_default();
                    if !access_token.is_empty() {
                        let domain = string_field(&data, &["domain"]);
                        let (uid, email, enterprise_id) = match fetch_account_info(&client, &access_token, &current.state, domain.as_deref()).await {
                            Ok(info) => info,
                            Err(_) => (
                                string_field(&data, &["uid", "userId", "user_id"]),
                                string_field(&data, &["email", "accountEmail", "account_email"]).unwrap_or_default(),
                                string_field(&data, &["enterpriseId", "enterprise_id"]),
                            ),
                        };
                        let expires_at = data.get("expiresAt").or_else(|| data.get("expires_at")).and_then(Value::as_i64).or_else(|| decode_jwt_exp(&access_token));
                        let result = OAuthCompleteResponse { email, uid, enterprise_id, access_token, refresh_token: string_field(&data, &["refreshToken", "refresh_token"]), expires_at, domain };
                        *pending_login().lock().map_err(|_| "登录状态锁不可用".to_string())? = None;
                        return Ok(result);
                    }
                }
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

pub async fn validate_token(access_token: &str) -> Result<OAuthCompleteResponse, String> {
    let token = access_token.trim();
    if token.is_empty() { return Err("Token 不能为空".to_string()); }
    let response = client()?.get(format!("{API_ENDPOINT}{API_PREFIX}/accounts")).bearer_auth(token).send().await.map_err(|error| format!("验证 CodeBuddy Token 失败：{error}"))?;
    if !response.status().is_success() { return Err(format!("Token 验证失败：HTTP {}", response.status())); }
    let body: Value = response.json().await.map_err(|error| format!("解析 Token 验证结果失败：{error}"))?;
    let accounts = body.get("data").and_then(|data| data.get("accounts")).and_then(Value::as_array);
    let account = accounts.and_then(|items| items.iter().find(|item| item.get("lastLogin").and_then(Value::as_bool) == Some(true)).or_else(|| items.first())).cloned().unwrap_or_else(|| json!({}));
    Ok(OAuthCompleteResponse { email: string_field(&account, &["email", "accountEmail", "account_email"]).or_else(|| string_field(&account, &["nickname"])).unwrap_or_default(), uid: string_field(&account, &["uid", "userId", "user_id"]), enterprise_id: string_field(&account, &["enterpriseId", "enterprise_id"]), access_token: token.to_string(), refresh_token: None, expires_at: decode_jwt_exp(token), domain: string_field(&account, &["domain"]) })
}

#[tauri::command]
pub async fn codebuddy_oauth_start() -> Result<OAuthStartResponse, String> {
    start_login().await
}

#[tauri::command]
pub async fn codebuddy_oauth_complete(login_id: String) -> Result<OAuthCompleteResponse, String> {
    complete_login(&login_id).await
}

#[tauri::command]
pub async fn codebuddy_oauth_cancel(login_id: String) -> Result<(), String> {
    cancel_login(&login_id).await
}

#[tauri::command]
pub async fn codebuddy_validate_token(access_token: String) -> Result<OAuthCompleteResponse, String> {
    validate_token(&access_token).await
}

#[derive(Debug, Clone, Default)]
pub struct QuotaRefreshOutcome {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub domain: Option<String>,
    pub token_changed: bool,
    pub quota: f64,
    pub quota_total: f64,
    pub plan: String,
}

fn billing_request(client: &Client, path: &str, access_token: &str, uid: Option<&str>, enterprise_id: Option<&str>, domain: Option<&str>) -> reqwest::RequestBuilder {
    let mut request = client
        .post(format!("{API_ENDPOINT}{path}"))
        .header("Accept", "application/json, text/plain, */*")
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .bearer_auth(access_token)
        .header("Content-Type", "application/json");
    if let Some(uid) = uid.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-User-Id", uid);
    }
    if let Some(enterprise_id) = enterprise_id.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-Enterprise-Id", enterprise_id).header("X-Tenant-Id", enterprise_id);
    }
    if let Some(domain) = domain.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-Domain", domain);
    }
    request
}

/// 把传输层错误收敛为固定文案：不回显 URL、请求头与响应体，
/// 避免 Bearer Token 或查询参数顺着错误信息流入界面与持久化日志。
fn transport_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "网络请求超时".to_string()
    } else if error.is_connect() {
        "连接服务端失败".to_string()
    } else if error.is_redirect() {
        "重定向次数过多".to_string()
    } else if error.is_body() || error.is_decode() {
        "响应内容读取失败".to_string()
    } else {
        "网络请求失败".to_string()
    }
}

async fn send_billing(request: reqwest::RequestBuilder, body: Value, label: &str) -> Result<Value, String> {
    let response = request.json(&body).send().await.map_err(|error| format!("请求{label}失败：{}", transport_error(&error)))?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|error| format!("解析{label}响应失败：{error}"))?;
    if !status.is_success() {
        let message = string_field(&body, &["message", "msg"]).unwrap_or_else(|| "未知错误".to_string());
        return Err(format!("请求{label}失败（HTTP {}）：{message}", status.as_u16()));
    }
    if let Some(code) = body.get("code").and_then(Value::as_i64) {
        if code != 0 && code != 200 {
            let message = string_field(&body, &["message", "msg"]).unwrap_or_else(|| "未知错误".to_string());
            return Err(format!("请求{label}失败（code={code}）：{message}"));
        }
    }
    Ok(body)
}

fn billing_time_range() -> (String, String) {
    let now = chrono::Local::now();
    let begin = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let end = (now + chrono::Duration::days(365 * 101)).format("%Y-%m-%d %H:%M:%S").to_string();
    (begin, end)
}

async fn fetch_user_resource(client: &Client, access_token: &str, uid: Option<&str>, enterprise_id: Option<&str>, domain: Option<&str>) -> Result<Value, String> {
    let (begin, end) = billing_time_range();
    let body = json!({
        "PageNumber": 1,
        "PageSize": 100,
        "ProductCode": "p_tcaca",
        "Status": [0, 3],
        "PackageEndTimeRangeBegin": begin,
        "PackageEndTimeRangeEnd": end,
    });
    send_billing(billing_request(client, &format!("{BILLING_PREFIX}/get-user-resource"), access_token, uid, enterprise_id, domain), body, "账号额度").await
}

async fn fetch_payment_type(client: &Client, access_token: &str, uid: Option<&str>, enterprise_id: Option<&str>, domain: Option<&str>) -> Result<Value, String> {
    send_billing(billing_request(client, &format!("{BILLING_PREFIX}/get-payment-type"), access_token, uid, enterprise_id, domain), json!({}), "套餐类型").await
}

async fn fetch_enterprise_user_usage(client: &Client, access_token: &str, enterprise_id: &str, domain: Option<&str>) -> Result<Value, String> {
    let mut request = client
        .post(format!("{API_ENDPOINT}/billing/meter/get-enterprise-user-usage"))
        .bearer_auth(access_token)
        .header("x-enterprise-id", enterprise_id);
    if let Some(domain) = domain.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-Domain", domain);
    }
    send_billing(request, json!({}), "企业用量").await
}

async fn refresh_access_token(client: &Client, access_token: &str, refresh_token: &str, domain: Option<&str>) -> Result<Value, String> {
    let mut request = client
        .post(format!("{API_ENDPOINT}{API_PREFIX}/auth/token/refresh"))
        .bearer_auth(access_token)
        .header("X-Refresh-Token", refresh_token)
        .json(&json!({}));
    if let Some(domain) = domain.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-Domain", domain);
    }
    let response = request.send().await.map_err(|error| format!("刷新 Token 失败：{error}"))?;
    let body: Value = response.json().await.map_err(|error| format!("解析 Token 刷新响应失败：{error}"))?;
    let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 && code != 200 {
        let message = string_field(&body, &["message", "msg"]).unwrap_or_else(|| "未知错误".to_string());
        return Err(format!("刷新 Token 失败（code={code}）：{message}"));
    }
    Ok(body.get("data").cloned().unwrap_or_else(|| json!({})))
}

fn numeric_field(value: &Value, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| {
        value.get(*name).and_then(|item| {
            item.as_f64().or_else(|| item.as_str().and_then(|text| text.trim().parse::<f64>().ok()))
        })
    })
}

fn resource_accounts(user_resource: &Value) -> Vec<&Value> {
    user_resource
        .pointer("/data/Response/Data/Accounts")
        .and_then(Value::as_array)
        .map(|accounts| accounts.iter().collect())
        .unwrap_or_default()
}

fn wrap_enterprise_usage_as_resource(usage: &Value) -> Value {
    let data = usage.get("data").cloned().unwrap_or_else(|| json!({}));
    let credit = data.get("credit").and_then(Value::as_f64).unwrap_or(0.0);
    let limit = data.get("limitNum").and_then(Value::as_f64).unwrap_or(0.0);
    let remain = (limit - credit).max(0.0);
    json!({
        "data": { "Response": { "Data": { "Accounts": [{
            "PackageCode": PACKAGE_ENTERPRISE,
            "CycleCapacitySizePrecise": limit.to_string(),
            "CycleCapacityRemainPrecise": remain.to_string(),
        }] } } }
    })
}

fn derive_plan(user_resource: &Value, payment: Option<&Value>) -> String {
    if let Some(payment) = payment {
        let payment_type = payment.get("data").and_then(|value| {
            value.as_str().map(str::to_string).or_else(|| string_field(value, &["paymentType", "payment_type"]))
        });
        if let Some(payment_type) = payment_type.filter(|value| !value.trim().is_empty()) {
            return payment_type.to_uppercase();
        }
    }
    let accounts = resource_accounts(user_resource);
    if accounts.iter().any(|account| account.get("PackageCode").and_then(Value::as_str) == Some(PACKAGE_ENTERPRISE)) {
        return "ENTERPRISE".to_string();
    }
    if accounts.iter().any(|account| matches!(account.get("PackageCode").and_then(Value::as_str), Some(PACKAGE_PRO_MONTH) | Some(PACKAGE_PRO_YEAR))) {
        return "PRO".to_string();
    }
    "FREE".to_string()
}

fn parse_quota(user_resource: &Value) -> (f64, f64) {
    let mut total = 0.0;
    let mut remain = 0.0;
    for account in resource_accounts(user_resource) {
        total += numeric_field(account, &["CycleCapacitySizePrecise", "CycleCapacitySize", "CapacitySizePrecise", "CapacitySize"]).unwrap_or(0.0);
        remain += numeric_field(account, &["CycleCapacityRemainPrecise", "CycleCapacityRemain", "CapacityRemainPrecise", "CapacityRemain"]).unwrap_or(0.0);
    }
    (remain, total)
}

pub async fn refresh_quota(access_token: &str, refresh_token: Option<&str>, uid: Option<&str>, enterprise_id: Option<&str>, domain: Option<&str>) -> Result<QuotaRefreshOutcome, String> {
    let client = client()?;
    let mut outcome = QuotaRefreshOutcome {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.map(str::to_string),
        domain: domain.map(str::to_string),
        ..QuotaRefreshOutcome::default()
    };
    if let Some(refresh_token) = refresh_token.filter(|value| !value.trim().is_empty()) {
        if let Ok(data) = refresh_access_token(&client, &outcome.access_token, refresh_token, domain).await {
            if let Some(next_token) = string_field(&data, &["accessToken", "access_token"]) {
                outcome.access_token = next_token;
                outcome.token_changed = true;
            }
            if let Some(next_refresh) = string_field(&data, &["refreshToken", "refresh_token"]) {
                outcome.refresh_token = Some(next_refresh);
            }
            if let Some(next_domain) = string_field(&data, &["domain"]) {
                outcome.domain = Some(next_domain);
            }
        }
    }
    let mut user_resource = fetch_user_resource(&client, &outcome.access_token, uid, enterprise_id, outcome.domain.as_deref()).await?;
    if let Some(enterprise_id) = enterprise_id.filter(|value| !value.trim().is_empty()) {
        if resource_accounts(&user_resource).is_empty() {
            if let Ok(usage) = fetch_enterprise_user_usage(&client, &outcome.access_token, enterprise_id, outcome.domain.as_deref()).await {
                user_resource = wrap_enterprise_usage_as_resource(&usage);
            }
        }
    }
    let payment = fetch_payment_type(&client, &outcome.access_token, uid, enterprise_id, outcome.domain.as_deref()).await.ok();
    let (quota, quota_total) = parse_quota(&user_resource);
    outcome.quota = quota;
    outcome.quota_total = quota_total;
    outcome.plan = derive_plan(&user_resource, payment.as_ref());
    Ok(outcome)
}

// ===== 每日签到（CodeBuddy CN daily checkin）=====

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckinStatusResponse {
    pub today_checked_in: bool,
    pub active: bool,
    pub streak_days: i64,
    pub daily_credit: i64,
    pub today_credit: Option<i64>,
    pub next_streak_day: Option<i64>,
    pub is_streak_day: Option<bool>,
    pub checkin_dates: Option<Vec<String>>,
    pub streak_bonus_days: Option<i64>,
    pub streak_bonus_credit: Option<i64>,
}

impl Default for CheckinStatusResponse {
    fn default() -> Self {
        Self {
            today_checked_in: false,
            active: true,
            streak_days: 0,
            daily_credit: 0,
            today_credit: None,
            next_streak_day: None,
            is_streak_day: None,
            checkin_dates: None,
            streak_bonus_days: None,
            streak_bonus_credit: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckinResponse {
    pub success: bool,
    pub message: Option<String>,
    pub reward: Option<Value>,
    pub credit: Option<i64>,
    pub streak_days: Option<i64>,
    pub is_streak_day: Option<bool>,
    pub next_checkin_in: Option<i64>,
}

impl Default for CheckinResponse {
    fn default() -> Self {
        Self {
            success: false,
            message: None,
            reward: None,
            credit: None,
            streak_days: None,
            is_streak_day: None,
            next_checkin_in: None,
        }
    }
}

fn json_bool(value: &Value, snake: &str, camel: &str) -> Option<bool> {
    let raw = value.get(snake).or_else(|| value.get(camel))?;
    match raw {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => number.as_i64().map(|item| item != 0),
        Value::String(text) => Some(text != "0" && !text.is_empty()),
        _ => None,
    }
}

fn json_i64(value: &Value, snake: &str, camel: &str) -> Option<i64> {
    value
        .get(snake)
        .or_else(|| value.get(camel))
        .and_then(|item| item.as_i64().or_else(|| item.as_str().and_then(|text| text.trim().parse::<i64>().ok())))
}

fn checkin_request(client: &Client, path: &str, access_token: &str, uid: Option<&str>, enterprise_id: Option<&str>, domain: Option<&str>) -> reqwest::RequestBuilder {
    let mut request = client
        .post(format!("{API_ENDPOINT}{path}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .bearer_auth(access_token);
    if let Some(uid) = uid.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-User-Id", uid);
    }
    if let Some(enterprise_id) = enterprise_id.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-Enterprise-Id", enterprise_id).header("X-Tenant-Id", enterprise_id);
    }
    if let Some(domain) = domain.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-Domain", domain);
    }
    request
}

fn parse_checkin_status(data: &Value) -> CheckinStatusResponse {
    CheckinStatusResponse {
        today_checked_in: json_bool(data, "today_checked_in", "todayCheckedIn").unwrap_or(false),
        active: json_bool(data, "active", "Active").unwrap_or(true),
        streak_days: json_i64(data, "streak_days", "streakDays").unwrap_or(0),
        daily_credit: json_i64(data, "daily_credit", "dailyCredit").unwrap_or(0),
        today_credit: json_i64(data, "today_credit", "todayCredit"),
        next_streak_day: json_i64(data, "next_streak_day", "nextStreakDay"),
        is_streak_day: json_bool(data, "is_streak_day", "isStreakDay"),
        checkin_dates: data
            .get("checkin_dates")
            .or_else(|| data.get("checkinDates"))
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect()),
        streak_bonus_days: json_i64(data, "streak_bonus_days", "streakBonusDays"),
        streak_bonus_credit: json_i64(data, "streak_bonus_credit", "streakBonusCredit"),
    }
}

async fn fetch_checkin_status_from(path: &str, access_token: &str, uid: Option<&str>, enterprise_id: Option<&str>, domain: Option<&str>) -> Result<CheckinStatusResponse, String> {
    let client = client()?;
    let response = checkin_request(&client, path, access_token, uid, enterprise_id, domain)
        .json(&json!({}))
        .send()
        .await
        .map_err(|error| format!("请求签到状态失败：{error}"))?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|error| format!("解析签到状态响应失败：{error}"))?;
    if !status.is_success() {
        let message = string_field(&body, &["message", "msg"]).unwrap_or_else(|| "未知错误".to_string());
        return Err(format!("查询签到状态失败（HTTP {}）：{message}", status.as_u16()));
    }
    if let Some(code) = body.get("code").and_then(Value::as_i64) {
        if code != 0 && code != 200 {
            let message = string_field(&body, &["message", "msg"]).unwrap_or_else(|| "未知错误".to_string());
            return Err(format!("查询签到状态失败（code={code}）：{message}"));
        }
    }
    let data = body.get("data").cloned().unwrap_or_else(|| json!({}));
    Ok(parse_checkin_status(&data))
}

pub async fn get_checkin_status(access_token: &str, uid: Option<&str>, enterprise_id: Option<&str>, domain: Option<&str>) -> Result<CheckinStatusResponse, String> {
    match fetch_checkin_status_from("/v2/billing/meter/checkin-activity-status", access_token, uid, enterprise_id, domain).await {
        Ok(status) => Ok(status),
        Err(activity_err) => match fetch_checkin_status_from("/v2/billing/meter/checkin-status", access_token, uid, enterprise_id, domain).await {
            Ok(status) => Ok(status),
            Err(legacy_err) => Err(format!("查询签到状态失败：{activity_err}（回退接口：{legacy_err}）")),
        },
    }
}

pub async fn perform_checkin(access_token: &str, uid: Option<&str>, enterprise_id: Option<&str>, domain: Option<&str>) -> Result<CheckinResponse, String> {
    let client = client()?;
    let response = checkin_request(&client, "/v2/billing/meter/daily-checkin", access_token, uid, enterprise_id, domain)
        .json(&json!({}))
        .send()
        .await
        .map_err(|error| format!("请求签到失败：{error}"))?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|error| format!("解析签到响应失败：{error}"))?;
    if !status.is_success() {
        let message = string_field(&body, &["message", "msg"]).unwrap_or_else(|| "未知错误".to_string());
        return Err(format!("请求签到失败（HTTP {}）：{message}", status.as_u16()));
    }
    let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
    let api_msg = string_field(&body, &["message", "msg"]).unwrap_or_else(|| "未知错误".to_string());
    // 与官方一致：仅 code==0 视为成功；code!=0 为业务错误（如已签到），包装为 success=false 返回给前端展示。
    if code != 0 {
        return Ok(CheckinResponse { success: false, message: Some(api_msg), ..CheckinResponse::default() });
    }
    let data = body.get("data").cloned().unwrap_or_else(|| json!({}));
    Ok(CheckinResponse {
        success: data.get("success").and_then(Value::as_bool).unwrap_or(true),
        message: string_field(&data, &["message", "msg"]),
        reward: data.get("reward").cloned(),
        credit: json_i64(&data, "credit", "credit").or_else(|| json_i64(&data, "today_credit", "todayCredit")),
        streak_days: json_i64(&data, "streak_days", "streakDays"),
        is_streak_day: json_bool(&data, "is_streak_day", "isStreakDay"),
        next_checkin_in: json_i64(&data, "next_checkin_in", "nextCheckinIn"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quota_sums_precise_fields_across_accounts() {
        let resource = json!({
            "data": { "Response": { "Data": { "Accounts": [
                { "PackageCode": PACKAGE_PRO_MONTH, "CycleCapacitySizePrecise": "1000", "CycleCapacityRemainPrecise": "800.5" },
                { "PackageCode": "TCACA_code_009_0XmEQc2xOf", "CapacitySize": 500, "CapacityRemain": 125.5 }
            ] } } }
        });
        let (remain, total) = parse_quota(&resource);
        assert_eq!(total, 1500.0);
        assert_eq!(remain, 926.0);
    }

    #[test]
    fn derive_plan_prefers_payment_type_then_package_code() {
        let empty = json!({ "data": { "Response": { "Data": { "Accounts": [] } } } });
        let payment = json!({ "data": { "paymentType": "pro" } });
        assert_eq!(derive_plan(&empty, Some(&payment)), "PRO");
        let payment_string = json!({ "data": "free" });
        assert_eq!(derive_plan(&empty, Some(&payment_string)), "FREE");
        let pro_package = json!({ "data": { "Response": { "Data": { "Accounts": [{ "PackageCode": PACKAGE_PRO_YEAR }] } } } });
        assert_eq!(derive_plan(&pro_package, None), "PRO");
        let enterprise_package = json!({ "data": { "Response": { "Data": { "Accounts": [{ "PackageCode": PACKAGE_ENTERPRISE }] } } } });
        assert_eq!(derive_plan(&enterprise_package, None), "ENTERPRISE");
        assert_eq!(derive_plan(&empty, None), "FREE");
    }

    #[test]
    fn enterprise_usage_wraps_into_resource_shape() {
        let usage = json!({ "data": { "credit": 120.0, "limitNum": 500.0 } });
        let wrapped = wrap_enterprise_usage_as_resource(&usage);
        let (remain, total) = parse_quota(&wrapped);
        assert_eq!(total, 500.0);
        assert_eq!(remain, 380.0);
        assert_eq!(derive_plan(&wrapped, None), "ENTERPRISE");
    }
}

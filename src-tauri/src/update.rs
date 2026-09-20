// 更新检查：读取 GitHub Releases 最新发布，与当前编译版本比对，并把安装包
// 下载地址解析出来交给界面。
//
// 设计边界（刻意不做的事）：本模块只负责「发现新版本 + 给出下载地址」，不会
// 下载 zip、解压或运行安装包。安装由用户在浏览器里自行完成，这样就不必在程序
// 内处理进程占用、sidecar 停机与安装失败回滚这些问题。
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 更新检查的目标仓库，与 README 徽章、`package.json` 中的仓库地址一致。
const REPO: &str = "Senguk520/CodeRelay-tools";
/// 当前编译版本。唯一来源是 `Cargo.toml` 的 `[package] version`，该字段由
/// `scripts/sync-version.mjs` 从 `package.json` 同步，因此这里不再另立版本常量，
/// 避免后端比对口径与前端展示（`__APP_VERSION__`）出现偏差。
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// GitHub API 强制要求携带 User-Agent，缺失会直接返回 403。
const USER_AGENT: &str = "CodeRelay";

/// 安装包地址来自哪里。
///
/// `assets` 是 GitHub 的标准附件位，能给出稳定直链、文件名与体积；`releaseBody`
/// 是兼容历史发布方式的兜底——早期版本把 zip 写在 Release 说明正文里。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallerSource {
    Asset,
    ReleaseBody,
}

/// 交给界面的更新检查结果。
///
/// 这是会话态数据，不参与 `state.json` 持久化；界面每次检测重新获取即可。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    /// 当前运行版本（来自 `Cargo.toml`，无 `v` 前缀）。
    pub current_version: String,
    /// 最新发布版本（tag 去掉 `v` 前缀）。tag 无法解析时原样返回，便于界面展示。
    pub latest_version: String,
    /// 是否确实存在更新。为 `false` 时界面必须显示「已是最新」。
    pub has_update: bool,
    /// 发布页地址，即「打开发布页」按钮的目标。
    pub release_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,
    /// 发布说明正文（Markdown 原文），由界面决定如何展示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    pub prerelease: bool,
    /// 安装包直链。解析不到时为 `None`，此时界面只提供「打开发布页」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_source: Option<InstallerSource>,
}

/// GitHub Releases API 中本项目用得到的字段。
///
/// 只声明必需字段并对其余字段加 `default`：GitHub 偶尔增减字段时，多出来的字段
/// 会被忽略，缺失的字段也不会让整个响应解析失败。
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    #[serde(default)]
    browser_download_url: Option<String>,
    #[serde(default)]
    size: u64,
}

/// 解析完成的安装包信息，仅在 `build_result` 内部使用。
struct Installer {
    url: String,
    name: String,
    size: Option<u64>,
    source: InstallerSource,
}

/// 把 tag 解析成可比较的三段数字，例如 `v0.3.0` -> `(0, 3, 0)`、`0.3.0-rc.1` -> `(0, 3, 0)`。
///
/// 返回 `None` 表示这个 tag 不适合参与比对，调用方按「无更新」处理：
/// - 历史遗留的 `Bate_Version` 这类单段 tag（既没有 `.` 也没有数字开头）；
/// - 段内容不是数字的 tag。
///
/// 宁可漏报，也不要因为读不懂 tag 就让用户长期看到一个永远消不掉的升级提示。
fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let trimmed = value.trim().trim_start_matches(['v', 'V']);
    let segments: Vec<&str> = trimmed.split('.').collect();
    // 至少两段才认为是版本号，借此排除 `Bate_Version`、`2026-fix` 这类 tag。
    if segments.len() < 2 {
        return None;
    }
    let mut numbers = [0u64; 3];
    for (index, segment) in segments.iter().take(3).enumerate() {
        // 取前导数字，容忍 `0-rc1`、`0+build` 这类后缀。
        let digits: String = segment.chars().take_while(|item| item.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        numbers[index] = digits.parse().ok()?;
    }
    Some((numbers[0], numbers[1], numbers[2]))
}

/// 把传输层错误收敛为可读文案。GitHub 是公开接口，错误里不含凭据，
/// 但仍按项目既有习惯不把请求头、响应体带回界面。
fn transport_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "连接 GitHub 超时，请检查网络后重试".to_string()
    } else if error.is_connect() {
        "连接 GitHub 失败，请检查网络或代理设置".to_string()
    } else {
        format!("读取 GitHub 发布信息失败：{error}")
    }
}

/// 从 assets 中挑选安装包：优先文件名含 `setup` 的 zip（NSIS 安装包，体积更小
/// 且与当前发布方式一致），否则退回第一个 zip。没有可用 zip 时返回 `None`。
fn installer_from_assets(assets: &[GithubReleaseAsset]) -> Option<(String, String, u64)> {
    let zips: Vec<&GithubReleaseAsset> = assets
        .iter()
        .filter(|asset| asset.name.to_ascii_lowercase().ends_with(".zip") && asset.browser_download_url.is_some())
        .collect();
    let picked = zips
        .iter()
        .find(|asset| asset.name.to_ascii_lowercase().contains("setup"))
        .or_else(|| zips.first())?;
    Some((picked.browser_download_url.clone()?, picked.name.clone(), picked.size))
}

/// 正文兜底：从 Release 说明里提取 `user-attachments` 的 zip 直链。
///
/// 早期发布把安装包写在说明正文（形如 `https://github.com/user-attachments/files/<id>/<name>.zip`），
/// 该链接实测可匿名下载。这里手工扫描而不引入正则依赖：项目当前没有 `regex`，
/// 为一段固定前缀的匹配新增依赖并不划算。
///
/// 允许 URL 出现在 markdown 链接 `[文本](url)` 里，因此遇到 `)` 等分隔符即截断。
fn installer_from_body(body: &str) -> Option<(String, String)> {
    const MARKER: &str = "https://github.com/user-attachments/files/";
    let mut cursor = 0usize;
    let mut fallback: Option<(String, String)> = None;
    while let Some(offset) = body[cursor..].find(MARKER) {
        let start = cursor + offset;
        let rest = &body[start..];
        let end = rest.find(terminates_url).unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ';', ':']);
        // 从标记之后继续扫描，避免同一段链接被反复匹配。
        cursor = start + MARKER.len();
        let raw_name = url.rsplit('/').next().unwrap_or_default();
        let name = raw_name.split(['?', '#']).next().unwrap_or(raw_name);
        if !name.to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        if name.to_ascii_lowercase().contains("setup") {
            return Some((url.to_string(), name.to_string()));
        }
        if fallback.is_none() {
            fallback = Some((url.to_string(), name.to_string()));
        }
    }
    fallback
}

/// URL 的终止字符。正文里链接后面可能紧跟中文说明、换行或 markdown 括号。
fn terminates_url(item: char) -> bool {
    item.is_whitespace() || matches!(item, ')' | ']' | '"' | '\'' | '<' | '>' | ',')
}

/// 由 API 响应组装检查结果。抽成独立函数便于单独推理比对与解析逻辑。
fn build_result(release: GithubRelease, current_version: &str) -> UpdateCheckResult {
    let latest_version = release.tag_name.trim().trim_start_matches(['v', 'V']).to_string();
    // `/releases/latest` 只会返回已发布且非预发布的版本，因此无需再过滤 draft。
    let has_update = match (parse_version(current_version), parse_version(&release.tag_name)) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    };

    let installer = installer_from_assets(&release.assets)
        .map(|(url, name, size)| Installer { url, name, size: Some(size), source: InstallerSource::Asset })
        .or_else(|| {
            release
                .body
                .as_deref()
                .and_then(installer_from_body)
                .map(|(url, name)| Installer { url, name, size: None, source: InstallerSource::ReleaseBody })
        });

    // 空字符串一律归一为 `None`，避免界面出现「名称为空」的空行。
    let trimmed = |value: Option<String>| value.filter(|item| !item.trim().is_empty());

    UpdateCheckResult {
        current_version: current_version.to_string(),
        latest_version,
        has_update,
        release_url: release.html_url,
        release_name: trimmed(release.name),
        release_notes: trimmed(release.body),
        published_at: trimmed(release.published_at),
        prerelease: release.prerelease,
        installer_url: installer.as_ref().map(|item| item.url.clone()),
        installer_name: installer.as_ref().map(|item| item.name.clone()),
        installer_size: installer.as_ref().and_then(|item| item.size),
        installer_source: installer.map(|item| item.source),
    }
}

/// 检查 GitHub 上的最新发布。任何网络或解析失败都返回 `Err`，由界面展示为
/// 「检测失败」——绝不能把失败当成「已是最新」，否则用户会以为更新功能正常。
#[tauri::command]
pub async fn check_for_update() -> Result<UpdateCheckResult, String> {
    let client = Client::builder().user_agent(USER_AGENT).timeout(REQUEST_TIMEOUT).build().map_err(|error| format!("创建更新检查客户端失败：{error}"))?;
    let response = client
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| transport_error(&error))?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("该仓库还没有发布过 Release".to_string());
    }
    if !status.is_success() {
        // 403 通常是触发了 GitHub 的匿名请求限流。
        let hint = if status == reqwest::StatusCode::FORBIDDEN { "，可能是请求过于频繁被 GitHub 限流，请稍后重试" } else { "" };
        return Err(format!("读取 GitHub 发布信息失败（HTTP {}{hint}）", status.as_u16()));
    }

    let release: GithubRelease = response.json().await.map_err(|error| format!("解析 GitHub 发布信息失败：{error}"))?;
    Ok(build_result(release, CURRENT_VERSION))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, url: &str, size: u64) -> GithubReleaseAsset {
        GithubReleaseAsset { name: name.to_string(), browser_download_url: Some(url.to_string()), size }
    }

    fn release(tag: &str, body: Option<&str>, assets: Vec<GithubReleaseAsset>) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_string(),
            html_url: format!("https://github.com/Senguk520/CodeRelay-tools/releases/tag/{tag}"),
            name: None,
            body: body.map(str::to_string),
            published_at: None,
            prerelease: false,
            assets,
        }
    }

    #[test]
    fn parses_version_tags() {
        assert_eq!(parse_version("v0.3.0"), Some((0, 3, 0)));
        assert_eq!(parse_version("0.3.0"), Some((0, 3, 0)));
        assert_eq!(parse_version("V1.2.3"), Some((1, 2, 3)));
        // 取前导数字，容忍预发布与构建元数据后缀。
        assert_eq!(parse_version("0.3.0-rc.1"), Some((0, 3, 0)));
        assert_eq!(parse_version("0.3.0+build.5"), Some((0, 3, 0)));
        assert_eq!(parse_version("0.10"), Some((0, 10, 0)));
        // 读不懂的 tag 一律不参与比对，宁可漏报也不给出误导性的升级提示。
        assert_eq!(parse_version("Bate_Version"), None);
        assert_eq!(parse_version("0"), None);
        assert_eq!(parse_version("release-1"), None);
    }

    #[test]
    fn compares_versions_segment_wise() {
        // 0.10.0 必须大于 0.9.9；按字符串比较会得出相反结论。
        assert!(build_result(release("v0.10.0", None, vec![]), "0.9.9").has_update);
        assert!(!build_result(release("v0.9.0", None, vec![]), "0.10.0").has_update);
        assert!(!build_result(release("v0.3.0", None, vec![]), "0.3.0").has_update);
        assert!(build_result(release("v0.3.1", None, vec![]), "0.3.0").has_update);
    }

    #[test]
    fn unparsable_tag_never_reports_update() {
        let result = build_result(release("Bate_Version", None, vec![]), "0.3.0");
        assert!(!result.has_update);
        // 版本展示值仍原样回传，界面据此提示用户去发布页查看。
        assert_eq!(result.latest_version, "Bate_Version");
    }

    #[test]
    fn prefers_setup_zip_from_assets() {
        let result = build_result(
            release(
                "v0.4.0",
                Some("正文兜底链接：https://github.com/user-attachments/files/1/CodeRelay_0.4.0_x64-setup.zip"),
                vec![
                    asset("CodeRelay_0.4.0_x64_en-US.zip", "https://github.com/x/msi.zip", 18_000_000),
                    asset("CodeRelay_0.4.0_x64-setup.zip", "https://github.com/x/setup.zip", 13_000_000),
                ],
            ),
            "0.3.0",
        );
        // assets 优先：即使正文也有可用链接，也必须取 assets 的直链。
        assert_eq!(result.installer_url.as_deref(), Some("https://github.com/x/setup.zip"));
        assert_eq!(result.installer_name.as_deref(), Some("CodeRelay_0.4.0_x64-setup.zip"));
        assert_eq!(result.installer_size, Some(13_000_000));
        assert!(matches!(result.installer_source, Some(InstallerSource::Asset)));
    }

    #[test]
    fn falls_back_to_first_zip_asset() {
        let result = build_result(release("v0.4.0", None, vec![asset("CodeRelay_0.4.0_x64_en-US.zip", "https://github.com/x/msi.zip", 18_000_000)]), "0.3.0");
        assert_eq!(result.installer_url.as_deref(), Some("https://github.com/x/msi.zip"));
        assert!(matches!(result.installer_source, Some(InstallerSource::Asset)));
    }

    #[test]
    fn parses_installer_from_release_body() {
        // 与 v0.3.0 实际发布正文同构：两个 zip 链接，setup 在前。
        let body = "本次更新带来 **局域网接入**。\r\n\r\n| 文件 | 说明 |\r\n|---|---|\r\n| [CodeRelay_0.3.0_x64-setup.zip](https://github.com/user-attachments/files/32196740/CodeRelay_0.3.0_x64-setup.zip) | NSIS 安装包（推荐，约 12.8 MB） |\r\n| [CodeRelay_0.3.0_x64_en-US.zip](https://github.com/user-attachments/files/32196804/CodeRelay_0.3.0_x64_en-US.zip) | MSI 安装包 |\r\n";
        let result = build_result(release("v0.3.0", Some(body), vec![]), "0.2.0");
        assert_eq!(result.installer_url.as_deref(), Some("https://github.com/user-attachments/files/32196740/CodeRelay_0.3.0_x64-setup.zip"));
        assert_eq!(result.installer_name.as_deref(), Some("CodeRelay_0.3.0_x64-setup.zip"));
        // 正文链接拿不到体积，界面需按「—」展示而不是 0。
        assert_eq!(result.installer_size, None);
        assert!(matches!(result.installer_source, Some(InstallerSource::ReleaseBody)));
    }

    #[test]
    fn ignores_non_zip_links_in_body() {
        let body = "见 https://github.com/user-attachments/files/1/notes.txt 与 https://example.com/other.md";
        let result = build_result(release("v0.4.0", Some(body), vec![]), "0.3.0");
        assert!(result.installer_url.is_none());
        assert!(result.installer_source.is_none());
    }

    #[test]
    fn empty_metadata_is_normalized_to_none() {
        let mut item = release("v0.4.0", Some("   "), vec![]);
        item.name = Some(String::new());
        item.published_at = Some("  ".to_string());
        let result = build_result(item, "0.3.0");
        // 空字符串必须归一为 None，否则界面会出现「名称为空」的空行。
        assert!(result.release_name.is_none());
        assert!(result.release_notes.is_none());
        assert!(result.published_at.is_none());
    }
}

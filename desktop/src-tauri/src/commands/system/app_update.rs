use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

use crate::app_brand_display_name;

/// 所有更新源 100% 绑定到本仓库（origin）：yagodakevin-cyber/Beav。
pub(super) const APP_UPDATE_DOWNLOAD_PAGE_URL: &str = "https://github.com/yagodakevin-cyber/Beav/releases";
const APP_UPDATE_API_URL: &str = "https://api.github.com/repos/yagodakevin-cyber/Beav/releases/latest";
const APP_UPDATE_ALLOWED_ASSET_PREFIX: &str = "https://github.com/yagodakevin-cyber/Beav/releases/download/";
const APP_UPDATE_CHECKSUM_ASSET_NAME: &str = "SHA256SUMS.txt";
const APP_UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const APP_UPDATE_CHECK_MIN_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const APP_UPDATE_INSTALL_PROGRESS_EVENT: &str = "app:update-install-progress";

const UPDATE_ROOT_DIR_NAME: &str = "Beav";
const UPDATE_PENDING_MARKER_FILE: &str = "pending-update.json";
const UPDATE_BACKUP_INFO_FILE: &str = "backup-info.json";

#[derive(Default)]
struct AppUpdateCheckState {
    in_flight: bool,
    last_checked_at: Option<Instant>,
    last_notified_version: String,
}

static APP_UPDATE_CHECK_STATE: OnceLock<Mutex<AppUpdateCheckState>> = OnceLock::new();

#[derive(Default)]
struct AppUpdateInstallState {
    in_flight: bool,
}

static APP_UPDATE_INSTALL_STATE: OnceLock<Mutex<AppUpdateInstallState>> = OnceLock::new();

fn app_update_state() -> &'static Mutex<AppUpdateCheckState> {
    APP_UPDATE_CHECK_STATE.get_or_init(|| Mutex::new(AppUpdateCheckState::default()))
}

fn app_install_state() -> &'static Mutex<AppUpdateInstallState> {
    APP_UPDATE_INSTALL_STATE.get_or_init(|| Mutex::new(AppUpdateInstallState::default()))
}

fn app_update_debug_log_enabled() -> bool {
    std::env::var("REDBOX_APP_UPDATE_DEBUG").ok().as_deref() == Some("1")
}

fn normalize_version_tag(raw: &str) -> String {
    raw.trim()
        .trim_start_matches(|value| value == 'v' || value == 'V')
        .to_string()
}

fn parse_semver_like(input: &str) -> [u64; 4] {
    let normalized = normalize_version_tag(input);
    let base = normalized.split('-').next().unwrap_or_default();
    let mut parts = [0_u64; 4];
    for (index, item) in base.split('.').take(4).enumerate() {
        parts[index] = item.parse::<u64>().unwrap_or(0);
    }
    parts
}

fn compare_semver_like(current: &str, latest: &str) -> std::cmp::Ordering {
    parse_semver_like(current).cmp(&parse_semver_like(latest))
}

fn is_http_url(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.starts_with("https://") || normalized.starts_with("http://")
}

/// 资源 URL 必须严格指向本仓库的 Release 下载地址，任何第三方来源一律拒绝。
fn is_bounded_release_asset_url(value: &str) -> bool {
    let normalized = value.trim();
    if !normalized.starts_with(APP_UPDATE_ALLOWED_ASSET_PREFIX) {
        return false;
    }
    let remainder = &normalized[APP_UPDATE_ALLOWED_ASSET_PREFIX.len()..];
    !remainder.is_empty() && !remainder.contains("//") && !remainder.contains('?') && !remainder.contains('#')
}

pub(super) fn app_update_platform() -> Result<&'static str, String> {
    if cfg!(target_os = "windows") {
        Ok("windows")
    } else if cfg!(target_os = "macos") {
        Ok("macos")
    } else {
        Err("当前系统暂不支持自动更新检查".to_string())
    }
}

pub(super) fn app_update_arch() -> Result<&'static str, String> {
    if cfg!(target_arch = "x86_64") {
        Ok("x64")
    } else if cfg!(target_arch = "x86") {
        Ok("x86")
    } else if cfg!(target_arch = "aarch64") {
        Ok("arm64")
    } else {
        Err("当前 CPU 架构暂不支持自动更新检查".to_string())
    }
}

pub(super) fn app_supports_in_place_update() -> bool {
    cfg!(target_os = "windows")
}

fn is_dev_build() -> bool {
    cfg!(debug_assertions)
}

#[derive(Deserialize, Clone)]
struct GitHubReleaseAsset {
    #[serde(default)]
    name: String,
    #[serde(rename = "browser_download_url", default)]
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct GitHubReleaseResponse {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(rename = "published_at", default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(rename = "html_url", default)]
    html_url: Option<String>,
    #[serde(default)]
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Clone)]
struct ReleaseAsset {
    name: String,
    url: String,
    size: u64,
}

struct LatestAppUpdate {
    ready: bool,
    version: String,
    name: String,
    published_at: String,
    body: String,
    html_url: String,
    platform_asset: Option<ReleaseAsset>,
    checksum_asset: Option<ReleaseAsset>,
}

/// 从资产列表中选择当前平台的安装包（仅限正式 Release 内附带的安装器）。
fn select_platform_asset<'a>(assets: &'a [GitHubReleaseAsset]) -> Option<ReleaseAsset> {
    let pick = |predicate: &dyn Fn(&GitHubReleaseAsset) -> bool| -> Option<ReleaseAsset> {
        assets
            .iter()
            .filter(|asset| is_bounded_release_asset_url(&asset.browser_download_url))
            .filter(|asset| predicate(asset))
            .map(|asset| ReleaseAsset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size: asset.size,
            })
            .next()
    };

    if cfg!(target_os = "windows") {
        pick(&|asset| {
            let lower = asset.name.to_ascii_lowercase();
            lower.ends_with("-setup.exe") && (lower.contains("x64") || lower.contains("x86_64"))
        })
        .or_else(|| pick(&|asset| asset.name.to_ascii_lowercase().ends_with("-setup.exe")))
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            pick(&|asset| {
                let lower = asset.name.to_ascii_lowercase();
                lower.ends_with(".dmg") && (lower.contains("aarch64") || lower.contains("arm64"))
            })
        } else {
            pick(&|asset| {
                let lower = asset.name.to_ascii_lowercase();
                lower.ends_with(".dmg") && lower.contains("x86_64")
            })
        }
    } else {
        None
    }
}

fn select_checksum_asset(assets: &[GitHubReleaseAsset]) -> Option<ReleaseAsset> {
    assets
        .iter()
        .filter(|asset| is_bounded_release_asset_url(&asset.browser_download_url))
        .find(|asset| asset.name.eq_ignore_ascii_case(APP_UPDATE_CHECKSUM_ASSET_NAME))
        .map(|asset| ReleaseAsset {
            name: asset.name.clone(),
            url: asset.browser_download_url.clone(),
            size: asset.size,
        })
}

fn http_client(connect_only_timeout: bool) -> Result<reqwest::blocking::Client, String> {
    let mut user_agent = reqwest::header::HeaderMap::new();
    user_agent.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_str(&format!(
            "{}/{}",
            app_brand_display_name(),
            env!("CARGO_PKG_VERSION")
        ))
        .map_err(|error| error.to_string())?,
    );
    let mut builder = reqwest::blocking::Client::builder()
        .connect_timeout(APP_UPDATE_CHECK_TIMEOUT)
        .default_headers(user_agent);
    if connect_only_timeout {
        builder = builder.timeout(None);
    } else {
        builder = builder.timeout(APP_UPDATE_CHECK_TIMEOUT);
    }
    builder.build().map_err(|error| error.to_string())
}

/// 拉取本仓库最新正式 Release。GitHub `releases/latest` 天然排除 draft 与 pre-release，
/// 同时再对字段做显式校验，双重保证只认正式发布版本。
fn fetch_latest_app_update() -> Result<LatestAppUpdate, String> {
    let client = http_client(false)?;
    let response = client
        .get(APP_UPDATE_API_URL)
        .query(&[
            ("platform", app_update_platform()?),
            ("arch", app_update_arch()?),
            ("currentVersion", env!("CARGO_PKG_VERSION")),
        ])
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|error| format!("更新源连接失败：{error}"))?;

    let status = response.status();
    let data = response
        .json::<GitHubReleaseResponse>()
        .map_err(|error| format!("更新源响应解析失败：{error}"))?;

    if status == reqwest::StatusCode::NOT_FOUND {
        // 仓库尚无任何正式 Release：视为无可用更新。
        return Ok(LatestAppUpdate {
            ready: false,
            version: String::new(),
            name: String::new(),
            published_at: String::new(),
            body: String::new(),
            html_url: APP_UPDATE_DOWNLOAD_PAGE_URL.to_string(),
            platform_asset: None,
            checksum_asset: None,
        });
    }
    if !status.is_success() {
        return Err(format!("更新源请求失败：HTTP {}", status));
    }

    let version = normalize_version_tag(&data.tag_name);
    let platform_asset = select_platform_asset(&data.assets);
    let checksum_asset = select_checksum_asset(&data.assets);
    // 只有「非 draft + 非 pre-release + 有有效版本号 + 本平台有安装包 + 附带校验文件」
    // 的 Release 才认为是可安装的正式版本。
    let ready = !data.draft
        && !data.prerelease
        && !version.is_empty()
        && platform_asset.is_some()
        && checksum_asset.is_some();

    Ok(LatestAppUpdate {
        ready,
        version,
        name: data.name.unwrap_or_default(),
        published_at: data.published_at.unwrap_or_default(),
        body: data.body.unwrap_or_default(),
        html_url: data
            .html_url
            .filter(|url| is_http_url(url))
            .unwrap_or_else(|| APP_UPDATE_DOWNLOAD_PAGE_URL.to_string()),
        platform_asset,
        checksum_asset,
    })
}

fn maybe_emit_app_update_available(
    app: &AppHandle,
    payload: &Value,
    latest_version: &str,
    force_notify: bool,
) {
    let should_emit = {
        let Ok(mut state) = app_update_state().lock() else {
            return;
        };
        if !force_notify && state.last_notified_version == latest_version {
            false
        } else {
            state.last_notified_version = latest_version.to_string();
            true
        }
    };

    if should_emit {
        let _ = app.emit("app:update-available", payload.clone());
    }
}

#[tauri::command]
pub(crate) async fn app_check_update(app: AppHandle, force: Option<bool>) -> Result<Value, String> {
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || check_app_update(&app, force, force))
        .await
        .map_err(|error| format!("更新检查任务执行失败：{error}"))?
}

pub(super) fn check_app_update(
    app: &AppHandle,
    force: bool,
    force_notify: bool,
) -> Result<Value, String> {
    let now = Instant::now();
    {
        let mut state = app_update_state()
            .lock()
            .map_err(|_| "App update state lock is poisoned".to_string())?;
        if state.in_flight {
            return Ok(json!({
                "success": false,
                "hasUpdate": false,
                "inFlight": true,
                "message": "Update check already in flight",
            }));
        }
        if !force
            && state
                .last_checked_at
                .map(|last_checked_at| {
                    now.duration_since(last_checked_at) < APP_UPDATE_CHECK_MIN_INTERVAL
                })
                .unwrap_or(false)
        {
            return Ok(json!({
                "success": true,
                "hasUpdate": false,
                "throttled": true,
                "message": "Update check skipped due to interval throttling",
            }));
        }
        state.in_flight = true;
        state.last_checked_at = Some(now);
    }

    let result: Result<Value, String> = (|| {
        let current_version = normalize_version_tag(env!("CARGO_PKG_VERSION"));
        let latest = fetch_latest_app_update()?;
        let has_update = latest.ready
            && compare_semver_like(&current_version, &latest.version) == std::cmp::Ordering::Less;
        let notice = json!({
            "currentVersion": current_version,
            "latestVersion": latest.version.clone(),
            "htmlUrl": if latest.html_url.is_empty() {
                APP_UPDATE_DOWNLOAD_PAGE_URL.to_string()
            } else {
                latest.html_url.clone()
            },
            "name": latest.name.clone(),
            "publishedAt": latest.published_at.clone(),
            "body": latest.body.clone(),
            "installable": latest.ready && app_supports_in_place_update(),
        });

        if has_update {
            maybe_emit_app_update_available(app, &notice, &latest.version, force_notify);
        }

        Ok(json!({
            "success": true,
            "hasUpdate": has_update,
            "notice": notice,
        }))
    })();

    if let Ok(mut state) = app_update_state().lock() {
        state.in_flight = false;
    }

    match result {
        Ok(value) => Ok(value),
        Err(message) => {
            if app_update_debug_log_enabled() {
                eprintln!("[AppUpdate] check failed: {message}");
            }
            Ok(json!({
                "success": false,
                "hasUpdate": false,
                "message": message,
                "error": message,
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// 更新根目录 / 标记文件
// ---------------------------------------------------------------------------

fn updates_root() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|base| base.join(UPDATE_ROOT_DIR_NAME).join("updates"))
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "无法定位应用数据目录".to_string())
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let content = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    std::fs::write(path, content).map_err(|error| error.to_string())
}

fn read_json_file(path: &Path) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// 下载 / 校验
// ---------------------------------------------------------------------------

fn emit_install_progress(
    app: &AppHandle,
    version: &str,
    status: &str,
    downloaded: u64,
    content_length: Option<u64>,
    error: &str,
) {
    let _ = app.emit(
        APP_UPDATE_INSTALL_PROGRESS_EVENT,
        json!({
            "status": status,
            "version": version,
            "downloaded": downloaded,
            "contentLength": content_length,
            "error": error,
        }),
    );
}

fn hash_file_sha256(path: &Path) -> Result<String, String> {
    use sha2::Digest;
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("无法读取安装包：{error}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取安装包失败：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// 从 SHA256SUMS.txt 内容中解析出指定文件的校验值（兼容 `hash  name` 与 `hash *name`）。
fn checksum_entry_for(checksums: &str, file_name: &str) -> Option<String> {
    for line in checksums.lines() {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let entry_name = parts.next()?;
        let entry_name = entry_name.trim_start_matches('*');
        if entry_name.eq_ignore_ascii_case(file_name) && hash.len() == 64 {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn download_with_progress(
    app: &AppHandle,
    version: &str,
    url: &str,
    destination: &Path,
    expected_size: u64,
) -> Result<(), String> {
    let client = http_client(true)?;
    let mut response = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .send()
        .map_err(|error| format!("下载更新包失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载更新包失败：HTTP {}", response.status()));
    }

    let content_length = response
        .content_length()
        .filter(|length| *length > 0)
        .unwrap_or(expected_size);
    let content_length = if content_length > 0 {
        Some(content_length)
    } else {
        None
    };

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut file = std::fs::File::create(destination)
        .map_err(|error| format!("无法写入下载文件：{error}"))?;

    let mut downloaded: u64 = 0;
    let mut last_emit_at = Instant::now();
    let mut last_emit_bytes: u64 = 0;
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("下载中断：{error}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| format!("写入下载文件失败：{error}"))?;
        downloaded += read as u64;

        let should_emit = last_emit_at.elapsed() >= Duration::from_millis(300)
            || downloaded.saturating_sub(last_emit_bytes) >= 512 * 1024;
        if should_emit {
            emit_install_progress(
                app,
                version,
                "downloading",
                downloaded,
                content_length,
                "",
            );
            last_emit_at = Instant::now();
            last_emit_bytes = downloaded;
        }
    }
    file.sync_all().map_err(|error| error.to_string())?;
    emit_install_progress(app, version, "downloading", downloaded, content_length, "");

    if let Some(expected) = content_length {
        if downloaded != expected {
            return Err(format!(
                "下载不完整：已下载 {downloaded} 字节，预期 {expected} 字节"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 安装 / 回滚
// ---------------------------------------------------------------------------

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    let entries = std::fs::read_dir(source).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let entry_path = entry.path();
        let target_path = destination.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_recursive(&entry_path, &target_path)?;
        } else {
            std::fs::copy(&entry_path, &target_path)
                .map_err(|error| format!("备份文件失败（{}）：{error}", entry_path.display()))?;
        }
    }
    Ok(())
}

fn current_install_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("无法定位应用主程序：{error}"))?;
    exe.parent()
        .map(|path| path.to_path_buf())
        .ok_or_else(|| "无法定位应用安装目录".to_string())
}

#[cfg(target_os = "windows")]
const CREATE_DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "windows")]
fn spawn_detached(script: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("cmd")
        .arg("/C")
        .arg(script)
        .creation_flags(CREATE_DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("启动更新编排进程失败：{error}"))
}

#[cfg(not(target_os = "windows"))]
fn spawn_detached(_script: &str) -> Result<(), String> {
    Err("当前系统不支持自动安装更新".to_string())
}

/// 备份当前安装目录，供更新失败或新版本异常时回滚。
fn backup_current_install(current_version: &str) -> Result<PathBuf, String> {
    let install_dir = current_install_dir()?;
    let backup_dir = updates_root()?.join("backup").join(current_version);
    if backup_dir.exists() {
        std::fs::remove_dir_all(&backup_dir).map_err(|error| error.to_string())?;
    }
    copy_dir_recursive(&install_dir, &backup_dir)?;
    write_json_file(
        &updates_root()?.join(UPDATE_BACKUP_INFO_FILE),
        &json!({
            "version": current_version,
            "path": backup_dir.display().to_string(),
            "createdAt": now_timestamp(),
        }),
    )?;
    Ok(backup_dir)
}

fn latest_backup() -> Option<(String, PathBuf)> {
    let info = read_json_file(&updates_root().ok()?.join(UPDATE_BACKUP_INFO_FILE))?;
    let version = info.get("version")?.as_str()?.to_string();
    let path = PathBuf::from(info.get("path")?.as_str()?);
    if path.exists() {
        Some((version, path))
    } else {
        None
    }
}

fn read_pending_update() -> Option<Value> {
    read_json_file(&updates_root().ok()?.join(UPDATE_PENDING_MARKER_FILE))
}

/// 更新安装统一入口：命令与旧通道分发共用，始终返回 Ok(Value)。
fn perform_app_install(app: &AppHandle) -> Result<Value, String> {
    if !app_supports_in_place_update() {
        return Ok(json!({
            "success": false,
            "error": "当前系统不支持应用内自动安装，请前往 Releases 页面手动安装",
            "downloadUrl": APP_UPDATE_DOWNLOAD_PAGE_URL,
        }));
    }
    if is_dev_build() {
        return Ok(json!({
            "success": false,
            "error": "开发构建不支持应用内更新，正式安装包才会执行自动安装",
            "downloadUrl": APP_UPDATE_DOWNLOAD_PAGE_URL,
        }));
    }

    {
        let mut state = app_install_state()
            .lock()
            .map_err(|_| "App install state lock is poisoned".to_string())?;
        if state.in_flight {
            return Ok(json!({
                "success": false,
                "inFlight": true,
                "error": "更新已在进行中",
            }));
        }
        state.in_flight = true;
    }

    let value = match install_app_update_blocking(app) {
        Ok(value) => value,
        Err(message) => {
            emit_install_progress(app, "", "failed", 0, None, &message);
            json!({ "success": false, "error": message })
        }
    };

    if value.get("success").and_then(Value::as_bool) != Some(true) {
        if let Ok(mut state) = app_install_state().lock() {
            state.in_flight = false;
        }
    }
    Ok(value)
}

#[tauri::command]
pub(crate) async fn app_install_update(app: AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || perform_app_install(&app))
        .await
        .map_err(|error| format!("更新安装任务执行失败：{error}"))?
}

pub(super) fn install_update_command(app: &AppHandle) -> Result<Value, String> {
    perform_app_install(app)
}

fn install_app_update_blocking(app: &AppHandle) -> Result<Value, String> {
    let current_version = normalize_version_tag(env!("CARGO_PKG_VERSION"));
    emit_install_progress(app, current_version.as_str(), "checking", 0, None, "");

    let latest = fetch_latest_app_update()?;
    if !latest.ready {
        return Ok(json!({
            "success": false,
            "hasUpdate": false,
            "error": "当前没有可安装的正式版本",
        }));
    }
    if compare_semver_like(&current_version, &latest.version) != std::cmp::Ordering::Less {
        return Ok(json!({
            "success": false,
            "hasUpdate": false,
            "error": "当前已是最新版本",
        }));
    }

    let asset = latest
        .platform_asset
        .clone()
        .ok_or_else(|| "正式版本中未找到当前平台的安装包".to_string())?;
    let checksum_asset = latest
        .checksum_asset
        .clone()
        .ok_or_else(|| "正式版本缺少校验文件，拒绝安装".to_string())?;

    // 1. 下载安装包（带进度）
    let downloads_dir = updates_root()?.join("downloads");
    let installer_path = downloads_dir.join(&asset.name);
    emit_install_progress(
        app,
        &latest.version,
        "downloading",
        0,
        (asset.size > 0).then_some(asset.size),
        "",
    );
    download_with_progress(
        app,
        &latest.version,
        &asset.url,
        &installer_path,
        asset.size,
    )?;

    // 2. 下载仓库内校验文件并核对 SHA256（校验不通过立即删除，绝不进入安装环节）
    let client = http_client(true)?;
    let checksums = client
        .get(&checksum_asset.url)
        .send()
        .map_err(|error| format!("下载校验文件失败：{error}"))?
        .text()
        .map_err(|error| format!("读取校验文件失败：{error}"))?;
    let expected_sha256 = checksum_entry_for(&checksums, &asset.name)
        .ok_or_else(|| "校验文件中没有该安装包的记录，拒绝安装".to_string())?;
    let actual_sha256 = hash_file_sha256(&installer_path)?;
    if actual_sha256 != expected_sha256 {
        let _ = std::fs::remove_file(&installer_path);
        return Err("安装包校验失败（SHA256 不匹配），已放弃安装".to_string());
    }

    // 3. 备份当前安装目录（回滚保障）
    emit_install_progress(app, &latest.version, "installing", 0, None, "");
    let backup_dir = backup_current_install(&current_version)?;

    // 4. 写入更新标记（重启后用于校验更新结果）
    write_json_file(
        &updates_root()?.join(UPDATE_PENDING_MARKER_FILE),
        &json!({
            "targetVersion": latest.version,
            "previousVersion": current_version,
            "installer": installer_path.display().to_string(),
            "installerSha256": actual_sha256,
            "backupDir": backup_dir.display().to_string(),
            "startedAt": now_timestamp(),
        }),
    )?;

    // 5. 分离式编排：静默安装 → 自动重启应用。用户数据目录全程不触碰，进度不受影响。
    let install_dir = current_install_dir()?;
    let exe_path = std::env::current_exe().map_err(|error| error.to_string())?;
    let script = format!(
        "\"{}\" /S & start \"{}\" /D \"{}\" \"{}\"",
        installer_path.display(),
        app_brand_display_name(),
        install_dir.display(),
        exe_path.display(),
    );
    spawn_detached(&script)?;

    // 6. 留出事件推送窗口后退出，交给安装器完成替换并自动重启。
    std::thread::sleep(Duration::from_millis(800));
    emit_install_progress(app, &latest.version, "installed", 0, None, "");
    app.exit(0);

    #[allow(unreachable_code)]
    Ok(json!({
        "success": true,
        "message": "安装包已校验通过，应用将自动重启完成更新",
        "version": latest.version,
    }))
}

/// 回滚统一入口：命令与旧通道分发共用。恢复最近一次备份并自动重启。
fn perform_rollback(app: &AppHandle) -> Result<Value, String> {
    if !app_supports_in_place_update() {
        return Ok(json!({
            "success": false,
            "error": "当前系统不支持自动回滚",
        }));
    }
    if is_dev_build() {
        return Ok(json!({
            "success": false,
            "error": "开发构建不支持回滚操作",
        }));
    }

    let (backup_version, backup_dir) = match latest_backup() {
        Some(found) => found,
        None => {
            return Ok(json!({
                "success": false,
                "error": "没有可用的回滚备份",
            }));
        }
    };

    let install_dir = current_install_dir()?;
    let exe_path = std::env::current_exe().map_err(|error| error.to_string())?;
    let script = format!(
        "timeout /t 2 /nobreak >nul & robocopy \"{}\" \"{}\" /MIR /R:2 /W:2 /NFL /NDL /NJH /NJS & start \"{}\" /D \"{}\" \"{}\"",
        backup_dir.display(),
        install_dir.display(),
        app_brand_display_name(),
        install_dir.display(),
        exe_path.display(),
    );
    spawn_detached(&script)?;

    std::thread::sleep(Duration::from_millis(500));
    app.exit(0);

    #[allow(unreachable_code)]
    Ok(json!({
        "success": true,
        "message": format!("正在回滚到 v{backup_version}，应用将自动重启"),
        "version": backup_version,
    }))
}

#[tauri::command]
pub(crate) async fn app_rollback_update(app: AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || perform_rollback(&app))
        .await
        .map_err(|error| format!("回滚任务执行失败：{error}"))?
}

pub(super) fn rollback_update_command(app: &AppHandle) -> Result<Value, String> {
    perform_rollback(app)
}

#[tauri::command]
pub(crate) async fn app_get_update_status() -> Result<Value, String> {
    Ok(get_app_update_status())
}

pub(super) fn get_app_update_status_value() -> Value {
    get_app_update_status()
}

fn get_app_update_status() -> Value {
    let current_version = normalize_version_tag(env!("CARGO_PKG_VERSION"));
    let mut updated_to: Option<String> = None;
    let mut pending_target: Option<String> = None;

    if let Some(pending) = read_pending_update() {
        let target = pending
            .get("targetVersion")
            .and_then(Value::as_str)
            .map(normalize_version_tag)
            .unwrap_or_default();
        if !target.is_empty() {
            if compare_semver_like(&current_version, &target) != std::cmp::Ordering::Less {
                // 更新后校验：当前版本已达到目标版本 → 更新成功，清除标记。
                if let Ok(root) = updates_root() {
                    let _ = std::fs::remove_file(root.join(UPDATE_PENDING_MARKER_FILE));
                }
                updated_to = Some(target);
            } else {
                // 更新未完成（应用仍运行旧版本），保留标记供回滚。
                pending_target = Some(target);
            }
        }
    }

    let (backup_version, can_rollback) = match latest_backup() {
        Some((version, _)) if app_supports_in_place_update() && !is_dev_build() => {
            (Some(version), true)
        }
        Some((version, _)) => (Some(version), false),
        None => (None, false),
    };

    json!({
        "currentVersion": current_version,
        "updatedTo": updated_to,
        "pendingTarget": pending_target,
        "backupVersion": backup_version,
        "canRollback": can_rollback,
        "installable": app_supports_in_place_update() && !is_dev_build(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        checksum_entry_for, compare_semver_like, is_bounded_release_asset_url,
        normalize_version_tag, parse_semver_like, select_checksum_asset, select_platform_asset,
        GitHubReleaseAsset,
    };

    #[test]
    fn version_tag_normalization_removes_v_prefix_only() {
        assert_eq!(normalize_version_tag(" v1.2.3 "), "1.2.3");
        assert_eq!(normalize_version_tag("release-1.2.3"), "release-1.2.3");
    }

    #[test]
    fn semver_like_parser_ignores_prerelease_suffix() {
        assert_eq!(parse_semver_like("v1.2.3-beta.1"), [1, 2, 3, 0]);
    }

    #[test]
    fn semver_like_compare_uses_numeric_segments() {
        assert_eq!(
            compare_semver_like("1.9.0", "1.10.0"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_semver_like("2.5.0", "2.5.0"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_semver_like("2.5.1", "2.5.0"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn asset_urls_must_stay_within_own_repo() {
        assert!(is_bounded_release_asset_url(
            "https://github.com/yagodakevin-cyber/Beav/releases/download/v2.6.0/Beav_2.6.0_x64-setup.exe"
        ));
        assert!(!is_bounded_release_asset_url(
            "https://github.com/Jamailar/Beav/releases/download/v2.6.0/Beav_2.6.0_x64-setup.exe"
        ));
        assert!(!is_bounded_release_asset_url(
            "https://evil.example.com/Beav/releases/download/v2.6.0/setup.exe"
        ));
        assert!(!is_bounded_release_asset_url(
            "https://github.com/yagodakevin-cyber/Beav/releases/download/v2.6.0//../etc"
        ));
        assert!(!is_bounded_release_asset_url(
            "https://github.com/yagodakevin-cyber/Beav/releases/download/v2.6.0/setup.exe?x=1"
        ));
    }

    fn asset(name: &str) -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            name: name.to_string(),
            browser_download_url: format!(
                "https://github.com/yagodakevin-cyber/Beav/releases/download/v2.6.0/{name}"
            ),
            size: 1024,
        }
    }

    #[test]
    fn selects_windows_setup_installer() {
        let assets = vec![
            asset("Beav_2.6.0_aarch64.dmg"),
            asset("Beav_2.6.0_x64-setup.exe"),
            asset("Beav_2.6.0_x64-setup.exe.sig"),
        ];
        let selected = select_platform_asset(&assets).expect("expected setup asset");
        assert_eq!(selected.name, "Beav_2.6.0_x64-setup.exe");
    }

    #[test]
    fn selects_checksum_manifest() {
        let assets = vec![
            asset("Beav_2.6.0_x64-setup.exe"),
            asset("SHA256SUMS.txt"),
        ];
        let selected = select_checksum_asset(&assets).expect("expected checksum asset");
        assert_eq!(selected.name, "SHA256SUMS.txt");
    }

    #[test]
    fn checksum_parser_supports_space_and_binary_modes() {
        let hash_a = "a".repeat(64);
        let hash_b = "b".repeat(64);
        let checksums = format!(
            "{hash_a}  Beav_2.6.0_x64-setup.exe\n{hash_b} *Beav_2.6.0_x64-setup.exe\nshort  other.exe\n"
        );
        assert_eq!(
            checksum_entry_for(&checksums, "Beav_2.6.0_x64-setup.exe").expect("first entry"),
            hash_a
        );
        // 最后一行同名记录优先后写覆盖语义不强制；此处只验证可解析二进制模式。
        assert_eq!(
            checksum_entry_for(
                &format!("{hash_b} *Beav_2.6.0_x64-setup.exe\n"),
                "beav_2.6.0_x64-setup.exe"
            )
            .expect("binary mode entry"),
            hash_b
        );
        assert!(checksum_entry_for("short other.exe\n", "other.exe").is_none());
    }
}

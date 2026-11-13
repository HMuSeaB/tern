//! Claude Desktop 第三方推理（3P）接入。
//!
//! 与 Claude Code CLI 完全分开：Desktop 从 `Claude-3p/configLibrary` 读取
//! gateway profile，通过 `_meta.json` 选择生效项；CLI 则读 `~/.claude/settings.json`。
//! 这里只维护 tern 自己的 profile，并保留/恢复用户原有的 profile、启动模式和其他字段。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{AppError, Result};

const PROFILE_ID: &str = "00000000-0000-4000-8000-7465726e0001";
const PROFILE_NAME: &str = "tern";
const BACKUP_FILE: &str = "claude-desktop-backup.json";
const DEPLOYMENT_MODE: &str = "deploymentMode";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopStatus {
    /// tern 配置文件存在并且是当前应用的 profile。
    pub connected: bool,
    pub active: bool,
    pub token_configured: bool,
    pub base_url: Option<String>,
    pub models: Vec<String>,
    pub profile_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Backup {
    previous_applied_id: Option<String>,
    previous_profile: Option<Value>,
    normal_config_existed: bool,
    normal_deployment_mode: Option<Value>,
    normal_disable_chooser: Option<Value>,
    threep_config_existed: bool,
    threep_deployment_mode: Option<Value>,
    threep_disable_chooser: Option<Value>,
}

#[derive(Debug, Clone)]
struct Paths {
    normal_config: PathBuf,
    threep_config: PathBuf,
    library: PathBuf,
    profile: PathBuf,
    meta: PathBuf,
    backup: PathBuf,
}

impl Paths {
    fn resolve() -> Result<Self> {
        #[cfg(target_os = "windows")]
        let (normal_root, threep_root) = windows_paths()?;
        #[cfg(target_os = "macos")]
        let (normal_root, threep_root) = {
            let root = dirs::data_dir().ok_or(AppError::NoConfigDir)?;
            (root.join("Claude"), root.join("Claude-3p"))
        };
        #[cfg(target_os = "linux")]
        let (normal_root, threep_root) = {
            let root = dirs::config_dir().ok_or(AppError::NoConfigDir)?;
            (root.join("Claude"), root.join("Claude-3p"))
        };

        let config_dir = crate::config::config_path()?
            .parent()
            .map(Path::to_path_buf)
            .ok_or(AppError::NoConfigDir)?;
        let library = threep_root.join("configLibrary");
        Ok(Self {
            normal_config: normal_root.join("claude_desktop_config.json"),
            threep_config: threep_root.join("claude_desktop_config.json"),
            profile: library.join(format!("{PROFILE_ID}.json")),
            meta: library.join("_meta.json"),
            library,
            backup: config_dir.join(BACKUP_FILE),
        })
    }
}

#[cfg(target_os = "windows")]
fn windows_paths() -> Result<(PathBuf, PathBuf)> {
    let local = dirs::data_local_dir().ok_or(AppError::NoConfigDir)?;
    Ok(windows_paths_from(&local))
}

fn windows_paths_from(local: &Path) -> (PathBuf, PathBuf) {
    // Packaged/Store installs keep app data in the package sandbox. Prefer those paths
    // only when the package already has a Claude Desktop data directory.
    let packages_dir = local.join("Packages");
    if let Ok(entries) = std::fs::read_dir(&packages_dir) {
        let mut packages = entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("Claude_") || name.starts_with("AnthropicPBC.Claude_")
                    })
            })
            .collect::<Vec<_>>();
        packages.sort();
        for package in packages {
            let package_local = package.join("LocalCache/Local/Claude-3p");
            let package_roaming = package.join("LocalCache/Roaming/Claude");
            if package_local.exists() || package_roaming.exists() {
                return (package_roaming, package_local);
            }
        }
    }
    (local.join("Claude"), local.join("Claude-3p"))
}

#[tauri::command]
pub fn claude_desktop_status() -> Result<DesktopStatus> {
    status_at(&Paths::resolve()?)
}

#[tauri::command]
pub fn claude_desktop_connect(provider_id: String, models: Vec<String>) -> Result<DesktopStatus> {
    let paths = Paths::resolve()?;
    let gateway = crate::wire::GatewayEndpoint::from_agent()?;
    let config = crate::config::load(&crate::config::config_path()?)?;
    if !config
        .providers
        .iter()
        .any(|provider| provider.id == provider_id)
    {
        return Err(AppError::Config(format!("找不到供应商：{provider_id}")));
    }
    let models = models
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .map(|model| format!("{provider_id}/{model}"))
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err(AppError::Config("至少填写一个模型名".into()));
    }

    let existing_profile = read_json(&paths.profile)?;
    let existing_meta = read_json(&paths.meta)?;
    let backup = if paths.backup.exists() {
        read_backup(&paths.backup)?
    } else {
        Backup {
            previous_applied_id: existing_meta
                .get("appliedId")
                .and_then(Value::as_str)
                .map(str::to_string),
            previous_profile: paths.profile.exists().then_some(existing_profile.clone()),
            normal_config_existed: paths.normal_config.exists(),
            normal_deployment_mode: read_json(&paths.normal_config)?
                .get(DEPLOYMENT_MODE)
                .cloned(),
            normal_disable_chooser: read_json(&paths.normal_config)?
                .get("disableDeploymentModeChooser")
                .cloned(),
            threep_config_existed: paths.threep_config.exists(),
            threep_deployment_mode: read_json(&paths.threep_config)?
                .get(DEPLOYMENT_MODE)
                .cloned(),
            threep_disable_chooser: read_json(&paths.threep_config)?
                .get("disableDeploymentModeChooser")
                .cloned(),
        }
    };

    let token = config
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .unwrap_or("tern");
    let profile = json!({
        "inferenceProvider": "gateway",
        "inferenceCredentialKind": "static",
        "disableDeploymentModeChooser": true,
        "inferenceGatewayBaseUrl": format!("http://{}", gateway.listen),
        "inferenceGatewayApiKey": token,
        "inferenceGatewayAuthScheme": "bearer",
        "inferenceModels": models.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
    });

    write_json(
        &paths.backup,
        &serde_json::to_value(&backup).map_err(json_error)?,
    )?;
    std::fs::create_dir_all(&paths.library).map_err(|error| io_error(&paths.library, error))?;
    write_json(&paths.profile, &profile)?;
    let mut meta = object_or_empty(read_json(&paths.meta)?);
    activate_profile(&mut meta);
    write_json(&paths.meta, &Value::Object(meta))?;

    set_desktop_mode(&paths.normal_config)?;
    set_desktop_mode(&paths.threep_config)?;
    status_at(&paths)
}

#[tauri::command]
pub fn claude_desktop_disconnect() -> Result<DesktopStatus> {
    let paths = Paths::resolve()?;
    if !paths.profile.exists() && !paths.backup.exists() {
        return status_at(&paths);
    }
    let backup = if paths.backup.exists() {
        read_backup(&paths.backup)?
    } else {
        Backup::default()
    };
    let mut meta = object_or_empty(read_json(&paths.meta)?);
    deactivate_profile(&mut meta, backup.previous_applied_id.as_deref());
    if let Some(previous) = backup.previous_profile {
        write_json(&paths.profile, &previous)?;
    } else if paths.profile.exists() {
        std::fs::remove_file(&paths.profile).map_err(|error| io_error(&paths.profile, error))?;
    }
    if paths.meta.exists() {
        write_json(&paths.meta, &Value::Object(meta))?;
    }

    restore_desktop_mode(
        &paths.normal_config,
        backup.normal_config_existed,
        backup.normal_deployment_mode,
        backup.normal_disable_chooser,
    )?;
    restore_desktop_mode(
        &paths.threep_config,
        backup.threep_config_existed,
        backup.threep_deployment_mode,
        backup.threep_disable_chooser,
    )?;
    if paths.backup.exists() {
        std::fs::remove_file(&paths.backup).map_err(|error| io_error(&paths.backup, error))?;
    }
    status_at(&paths)
}

fn status_at(paths: &Paths) -> Result<DesktopStatus> {
    let profile = read_json(&paths.profile)?;
    let meta = read_json(&paths.meta)?;
    let active = meta.get("appliedId").and_then(Value::as_str) == Some(PROFILE_ID);
    let base_url = profile
        .get("inferenceGatewayBaseUrl")
        .and_then(Value::as_str)
        .map(str::to_string);
    let models = profile
        .get("inferenceModels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            model
                .as_str()
                .or_else(|| model.get("name").and_then(Value::as_str))
                .map(str::to_string)
        })
        .collect();
    let connected = active && base_url.is_some();
    let token_configured = crate::config::load(&crate::config::config_path()?)
        .ok()
        .and_then(|config| config.access_token)
        .is_some_and(|token| !token.trim().is_empty());
    Ok(DesktopStatus {
        connected,
        active,
        token_configured,
        base_url,
        models,
        profile_path: paths.profile.display().to_string(),
    })
}

fn set_desktop_mode(path: &Path) -> Result<()> {
    let mut root = object_or_empty(read_json(path)?);
    root.insert(DEPLOYMENT_MODE.into(), Value::String("3p".into()));
    // Older Desktop releases predate deploymentMode but honor this compatibility key.
    root.insert("disableDeploymentModeChooser".into(), Value::Bool(true));
    write_json(path, &Value::Object(root))
}

fn restore_desktop_mode(
    path: &Path,
    existed: bool,
    deployment_mode: Option<Value>,
    disable_chooser: Option<Value>,
) -> Result<()> {
    if !existed && !path.exists() {
        return Ok(());
    }
    let mut root = object_or_empty(read_json(path)?);
    restore_field(&mut root, DEPLOYMENT_MODE, deployment_mode);
    restore_field(&mut root, "disableDeploymentModeChooser", disable_chooser);
    if !existed && root.is_empty() {
        if path.exists() {
            std::fs::remove_file(path).map_err(|error| io_error(path, error))?;
        }
        return Ok(());
    }
    write_json(path, &Value::Object(root))
}

fn restore_field(root: &mut Map<String, Value>, key: &str, previous: Option<Value>) {
    match previous {
        Some(value) => {
            root.insert(key.into(), value);
        }
        None => {
            root.remove(key);
        }
    }
}

fn activate_profile(meta: &mut Map<String, Value>) {
    let entries = meta
        .entry("entries")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entries.is_array() {
        *entries = Value::Array(Vec::new());
    }
    let entries = entries.as_array_mut().expect("normalized to array");
    entries.retain(|entry| entry.get("id").and_then(Value::as_str) != Some(PROFILE_ID));
    entries.push(json!({"id": PROFILE_ID, "name": PROFILE_NAME}));
    meta.insert("appliedId".into(), Value::String(PROFILE_ID.into()));
}

fn deactivate_profile(meta: &mut Map<String, Value>, previous_applied_id: Option<&str>) {
    let was_active = meta.get("appliedId").and_then(Value::as_str) == Some(PROFILE_ID);
    if let Some(entries) = meta.get_mut("entries").and_then(Value::as_array_mut) {
        entries.retain(|entry| entry.get("id").and_then(Value::as_str) != Some(PROFILE_ID));
    }
    if was_active {
        match previous_applied_id {
            Some(id) if id != PROFILE_ID => {
                meta.insert("appliedId".into(), Value::String(id.to_string()));
            }
            _ => {
                meta.remove("appliedId");
            }
        }
    }
}

fn object_or_empty(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn read_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path).map_err(|error| io_error(path, error))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(text).map_err(json_error)
}

fn read_backup(path: &Path) -> Result<Backup> {
    serde_json::from_value(read_json(path)?).map_err(json_error)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_vec_pretty(value).map_err(json_error)?;
    std::fs::write(&temp, text).map_err(|error| io_error(&temp, error))?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|error| io_error(path, error))?;
    }
    std::fs::rename(&temp, path).map_err(|error| io_error(path, error))
}

fn io_error(path: &Path, error: std::io::Error) -> AppError {
    AppError::Config(format!("读写 {} 失败：{error}", path.display()))
}

fn json_error(error: serde_json::Error) -> AppError {
    AppError::Config(format!("Claude Desktop 配置格式错误：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(dir: &Path) -> Paths {
        let library = dir.join("Claude-3p/configLibrary");
        Paths {
            normal_config: dir.join("Claude/claude_desktop_config.json"),
            threep_config: dir.join("Claude-3p/claude_desktop_config.json"),
            profile: library.join(format!("{PROFILE_ID}.json")),
            meta: library.join("_meta.json"),
            library,
            backup: dir.join("tern/claude-desktop-backup.json"),
        }
    }

    #[test]
    fn windows_paths_use_documented_locations_and_detect_packaged_installs() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("Local");
        let (normal, threep) = windows_paths_from(&local);
        assert_eq!(normal, local.join("Claude"));
        assert_eq!(threep, local.join("Claude-3p"));

        let packaged_local = local.join("Packages/Claude_pzs8/LocalCache/Local/Claude-3p");
        let packaged_roaming = local.join("Packages/Claude_pzs8/LocalCache/Roaming/Claude");
        std::fs::create_dir_all(&packaged_local).unwrap();
        std::fs::create_dir_all(&packaged_roaming).unwrap();
        let (normal, threep) = windows_paths_from(&local);
        assert_eq!(normal, packaged_roaming);
        assert_eq!(threep, packaged_local);
    }

    #[test]
    fn json_writer_round_trips_objects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("configLibrary/profile.json");
        write_json(&path, &json!({"inferenceProvider":"gateway"})).unwrap();
        assert_eq!(read_json(&path).unwrap()["inferenceProvider"], "gateway");
    }

    #[test]
    fn desktop_status_reports_unconfigured_profile_without_exposing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(dir.path());
        std::fs::create_dir_all(&paths.library).unwrap();
        write_json(
            &paths.profile,
            &json!({
                "inferenceProvider":"gateway",
                "inferenceGatewayBaseUrl":"http://127.0.0.1:15800",
                "inferenceGatewayApiKey":"secret",
                "inferenceModels":[{"name":"deepseek/model"}]
            }),
        )
        .unwrap();
        write_json(&paths.meta, &json!({"appliedId":PROFILE_ID})).unwrap();
        let status = status_at(&paths).unwrap();
        assert!(status.connected);
        assert!(status.token_configured);
        assert_eq!(status.base_url.as_deref(), Some("http://127.0.0.1:15800"));
        assert_eq!(status.models, ["deepseek/model"]);
        let serialized = serde_json::to_string(&status).unwrap();
        assert!(!serialized.contains("secret"));
    }

    #[test]
    fn activating_tern_preserves_other_desktop_profiles() {
        let mut meta = object_or_empty(json!({
            "appliedId": "other-profile",
            "entries": [
                {"id":"other-profile","name":"Other"},
                {"id":PROFILE_ID,"name":"old tern"}
            ],
            "otherSetting": true
        }));
        activate_profile(&mut meta);
        let entries = meta["entries"].as_array().unwrap();
        assert_eq!(meta["appliedId"], PROFILE_ID);
        assert_eq!(meta["otherSetting"], true);
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["id"] == PROFILE_ID)
                .count(),
            1
        );
        assert_eq!(entries[0]["id"], "other-profile");
    }

    #[test]
    fn disconnect_restores_previous_profile_and_keeps_others() {
        let mut meta = object_or_empty(json!({
            "appliedId": PROFILE_ID,
            "entries": [
                {"id":"other-profile","name":"Other"},
                {"id":PROFILE_ID,"name":"tern"}
            ],
            "otherSetting": true
        }));
        deactivate_profile(&mut meta, Some("other-profile"));
        assert_eq!(meta["appliedId"], "other-profile");
        assert_eq!(meta["entries"].as_array().unwrap().len(), 1);
        assert_eq!(meta["otherSetting"], true);
    }

    #[test]
    fn deployment_mode_restore_preserves_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        write_json(
            &path,
            &json!({"userSetting":true,"deploymentMode":"1p","disableDeploymentModeChooser":false}),
        )
        .unwrap();
        set_desktop_mode(&path).unwrap();
        assert_eq!(
            read_json(&path).unwrap()["disableDeploymentModeChooser"],
            true
        );
        restore_desktop_mode(&path, true, Some(json!("1p")), Some(json!(false))).unwrap();
        let value = read_json(&path).unwrap();
        assert_eq!(value["userSetting"], true);
        assert_eq!(value[DEPLOYMENT_MODE], "1p");
        assert_eq!(value["disableDeploymentModeChooser"], false);
    }
}

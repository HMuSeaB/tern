//! User-installed external sidecars. Plugins run with the user's full OS privileges;
//! process separation is a lifecycle boundary, not a security sandbox.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::HeaderValue;
use serde::Deserialize;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::provider::{ApiFormat, PluginRef, ProviderAuth, ProviderSpec};
use crate::proxy::ProxyError;

const MANIFEST_FILE: &str = "plugin.toml";
const START_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const REAPER_INTERVAL: Duration = Duration::from_secs(60);
const TOKEN_HEADER: &str = "x-tern-plugin-token";
const CHILD_ENV_KEYS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "TEMP",
    "TMP",
    "HOME",
    "USERPROFILE",
];

#[derive(Debug, Clone, Deserialize)]
struct PluginManifest {
    id: String,
    #[allow(dead_code)]
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    api_format: ApiFormat,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    session_file: Option<PathBuf>,
}

impl PluginManifest {
    fn load(plugin_dir: &Path, expected_id: &str) -> Result<Self, ProxyError> {
        let path = plugin_dir.join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).map_err(|error| {
            plugin_error(format!("读取 manifest 失败 ({}): {error}", path.display()))
        })?;
        let manifest: Self = toml::from_str(&text)
            .map_err(|error| plugin_error(format!("manifest 格式无效: {error}")))?;
        validate_id(&manifest.id)?;
        if manifest.id != expected_id {
            return Err(plugin_error(format!(
                "manifest id {:?} 与供应商绑定的插件 {:?} 不一致",
                manifest.id, expected_id
            )));
        }
        if manifest.name.trim().is_empty() {
            return Err(plugin_error("manifest name 不能为空"));
        }
        if manifest.command.trim().is_empty() {
            return Err(plugin_error("manifest command 不能为空"));
        }
        if manifest.models.iter().any(|model| model.trim().is_empty()) {
            return Err(plugin_error("manifest models 不能包含空模型 ID"));
        }
        if manifest.session_file.as_ref().is_some_and(|path| {
            path.as_os_str().is_empty()
                || path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
        }) {
            return Err(plugin_error(
                "manifest session_file 必须是插件目录内的相对路径",
            ));
        }
        Ok(manifest)
    }
}

#[derive(Debug, Clone)]
pub struct PluginConnection {
    pub addr: SocketAddr,
    token: String,
}

impl PluginConnection {
    pub fn token_header(&self) -> Result<HeaderValue, ProxyError> {
        HeaderValue::from_str(&self.token)
            .map_err(|_| plugin_error("生成的本地认证令牌不是合法 HTTP header"))
    }
}

struct RunningPlugin {
    child: Child,
    connection: PluginConnection,
    provider_id: String,
    plugin_dir: PathBuf,
    last_used: Instant,
}

/// Owns sidecar child processes and their per-process local authentication tokens.
pub struct PluginSupervisor {
    root: PathBuf,
    client: reqwest::Client,
    running: Mutex<HashMap<String, RunningPlugin>>,
    reaper_started: AtomicBool,
    path_snapshot: Option<OsString>,
    pathext_snapshot: Option<OsString>,
    child_env: Vec<(OsString, OsString)>,
}

impl PluginSupervisor {
    /// Default installation root follows the platform's roaming/config directory.
    pub fn default_root() -> PathBuf {
        std::env::var_os("TERN_PLUGIN_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|dir| dir.join("tern").join("plugins")))
            .unwrap_or_else(|| PathBuf::from("plugins"))
    }

    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ProxyError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            // A health endpoint must not be able to redirect the local auth token.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_millis(300))
            .timeout(Duration::from_millis(500))
            .build()
            .map_err(|error| plugin_error(format!("创建本地健康检查客户端失败: {error}")))?;
        let child_env: Vec<(OsString, OsString)> = CHILD_ENV_KEYS
            .iter()
            .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
            .collect();
        let env_value = |key: &str| {
            child_env
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        Ok(Self {
            root: root.into(),
            client,
            running: Mutex::new(HashMap::new()),
            reaper_started: AtomicBool::new(false),
            path_snapshot: env_value("PATH"),
            pathext_snapshot: env_value("PATHEXT"),
            child_env,
        })
    }

    fn plugin_dir(&self, reference: &PluginRef) -> Result<PathBuf, ProxyError> {
        let dir = match reference.dir.as_deref().map(PathBuf::from) {
            Some(raw) if raw.is_absolute() => raw,
            Some(raw) => self.root.join(raw),
            None => self.root.join(&reference.plugin_id),
        };
        dir.canonicalize().map_err(|error| {
            plugin_error(format!(
                "插件目录不存在或不可访问 ({}): {error}",
                dir.display()
            ))
        })
    }

    /// Read model IDs declared by a plugin manifest without starting the process.
    pub fn manifest_models(&self, reference: &PluginRef) -> Result<Vec<String>, ProxyError> {
        let plugin_dir = self.plugin_dir(reference)?;
        let manifest = PluginManifest::load(&plugin_dir, &reference.plugin_id)?;
        resolve_command(
            &manifest.command,
            &plugin_dir,
            self.path_snapshot.as_deref(),
            self.pathext_snapshot.as_deref(),
        )?;
        Ok(manifest.models)
    }

    /// Validate plugin bindings without starting their processes.
    pub fn validate_providers<'a>(
        &self,
        providers: impl IntoIterator<Item = &'a ProviderSpec>,
    ) -> Result<(), ProxyError> {
        let mut plugin_ids = HashSet::new();
        let mut provider_ids = HashSet::new();
        for provider in providers {
            let Some(reference) = &provider.plugin else {
                continue;
            };
            validate_id(&reference.plugin_id)?;
            if !provider_ids.insert(provider.id.as_str()) {
                return Err(plugin_error(format!("供应商 id {:?} 重复", provider.id)));
            }
            if !plugin_ids.insert(reference.plugin_id.as_str()) {
                return Err(plugin_error(format!(
                    "插件 {} 被多个供应商绑定；v1 每个插件只能绑定一个供应商",
                    reference.plugin_id
                )));
            }
            if !matches!(provider.auth, ProviderAuth::None) {
                return Err(plugin_error(format!(
                    "插件供应商 {} 必须使用 auth: {{ type: \"none\" }}",
                    provider.id
                )));
            }
            let plugin_dir = self.plugin_dir(reference)?;
            let manifest = PluginManifest::load(&plugin_dir, &reference.plugin_id)?;
            if manifest.api_format != provider.effective_api_format() {
                return Err(plugin_error(format!(
                    "插件 {} 声明的 api_format ({}) 与供应商配置 ({}) 不一致",
                    manifest.id,
                    manifest.api_format,
                    provider.effective_api_format()
                )));
            }
            resolve_command(
                &manifest.command,
                &plugin_dir,
                self.path_snapshot.as_deref(),
                self.pathext_snapshot.as_deref(),
            )?;
            if let Some(session_file) = manifest.session_file {
                let session_path = plugin_dir.join(session_file);
                if session_path.exists() {
                    let metadata = fs::metadata(&session_path).map_err(|error| {
                        plugin_error(format!("读取插件 session_file 元数据失败: {error}"))
                    })?;
                    if !metadata.is_file() {
                        return Err(plugin_error("manifest session_file 必须指向普通文件"));
                    }
                    fs::File::open(&session_path).map_err(|error| {
                        plugin_error(format!("打开插件 session_file 失败: {error}"))
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Start on first use and return the loopback address and per-process token.
    pub async fn ensure_running(
        self: &Arc<Self>,
        reference: &PluginRef,
        provider: &ProviderSpec,
    ) -> Result<PluginConnection, ProxyError> {
        validate_id(&reference.plugin_id)?;
        if !matches!(provider.auth, ProviderAuth::None) {
            return Err(plugin_error(format!(
                "插件供应商 {} 必须使用 auth: {{ type: \"none\" }}，本地令牌由 supervisor 单独注入",
                provider.id
            )));
        }
        let plugin_dir = self.plugin_dir(reference)?;
        let manifest = PluginManifest::load(&plugin_dir, &reference.plugin_id)?;
        if manifest.api_format != provider.effective_api_format() {
            return Err(plugin_error(format!(
                "插件 {} 声明的 api_format ({}) 与供应商配置 ({}) 不一致",
                manifest.id,
                manifest.api_format,
                provider.effective_api_format()
            )));
        }
        let command = resolve_command(
            &manifest.command,
            &plugin_dir,
            self.path_snapshot.as_deref(),
            self.pathext_snapshot.as_deref(),
        )?;
        self.start_reaper();
        let mut running = self.running.lock().await;
        let mut remove_dead = false;
        if let Some(plugin) = running.get_mut(&reference.plugin_id) {
            if plugin.provider_id != provider.id || plugin.plugin_dir != plugin_dir {
                remove_dead = true;
            } else {
                match plugin.child.try_wait() {
                    Ok(None) => {
                        plugin.last_used = Instant::now();
                        return Ok(plugin.connection.clone());
                    }
                    Ok(Some(_)) | Err(_) => remove_dead = true,
                }
            }
        }
        if remove_dead {
            if let Some(mut plugin) = running.remove(&reference.plugin_id) {
                let _ = plugin.child.kill().await;
                let _ = plugin.child.wait().await;
            }
        }

        let (addr, port) = reserve_loopback_port()?;
        let token = uuid::Uuid::new_v4().simple().to_string();

        let mut child_command = Command::new(command);
        child_command
            .args(&manifest.args)
            .arg("--port")
            .arg(port.to_string())
            .current_dir(&plugin_dir)
            .env_clear()
            .env("TERN_PLUGIN_PORT", port.to_string())
            .env("TERN_PLUGIN_TOKEN", &token)
            .stdin(Stdio::null())
            // Arbitrary plugin output may contain secrets; do not persist it as logs.
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        child_command.envs(self.child_env.iter().cloned());
        let mut child = child_command
            .spawn()
            .map_err(|error| plugin_error(format!("启动插件 {} 失败: {error}", manifest.id)))?;
        let connection = PluginConnection { addr, token };
        if let Err(error) = self.wait_until_healthy(&mut child, &connection).await {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error);
        }

        running.insert(
            reference.plugin_id.clone(),
            RunningPlugin {
                child,
                connection: connection.clone(),
                provider_id: provider.id.clone(),
                plugin_dir,
                last_used: Instant::now(),
            },
        );
        log::info!("插件 {} 已就绪于 127.0.0.1:{}", reference.plugin_id, port);
        Ok(connection)
    }

    async fn wait_until_healthy(
        &self,
        child: &mut Child,
        connection: &PluginConnection,
    ) -> Result<(), ProxyError> {
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| plugin_error(format!("检查插件进程状态失败: {error}")))?
            {
                return Err(plugin_error(format!("插件启动后立即退出，状态: {status}")));
            }
            let health = format!("http://{}/health", connection.addr);
            if let Ok(response) = self
                .client
                .get(health)
                .header(TOKEN_HEADER, connection.token_header()?)
                .send()
                .await
            {
                if response.status() == reqwest::StatusCode::OK {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(plugin_error("插件 5 秒内未通过健康检查"));
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    fn start_reaper(self: &Arc<Self>) {
        if self
            .reaper_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REAPER_INTERVAL);
            loop {
                interval.tick().await;
                let Some(supervisor) = weak.upgrade() else {
                    break;
                };
                supervisor.reap_idle().await;
            }
        });
    }

    async fn reap_idle(&self) {
        let mut running = self.running.lock().await;
        let stale: Vec<String> = running
            .iter_mut()
            .filter_map(|(id, plugin)| match plugin.child.try_wait() {
                Ok(Some(_)) | Err(_) => Some(id.clone()),
                Ok(None) if plugin.last_used.elapsed() >= IDLE_TIMEOUT => Some(id.clone()),
                Ok(None) => None,
            })
            .collect();
        for id in stale {
            if let Some(mut plugin) = running.remove(&id) {
                log::info!("回收已退出或空闲插件 {id}");
                let _ = plugin.child.kill().await;
                let _ = plugin.child.wait().await;
            }
        }
    }

    /// Stop a running sidecar. Does nothing when it has not been started.
    pub async fn stop(&self, plugin_id: &str) {
        if let Some(mut plugin) = self.running.lock().await.remove(plugin_id) {
            let _ = plugin.child.kill().await;
            let _ = plugin.child.wait().await;
        }
    }

    /// Stop child processes no longer referenced after a provider config reload.
    pub async fn prune(&self, providers: &[ProviderSpec]) {
        let alive: HashMap<String, (String, PathBuf)> = providers
            .iter()
            .filter_map(|provider| {
                let reference = provider.plugin.as_ref()?;
                let plugin_dir = self.plugin_dir(reference).ok()?;
                Some((
                    reference.plugin_id.clone(),
                    (provider.id.clone(), plugin_dir),
                ))
            })
            .collect();
        let stale: Vec<String> = self
            .running
            .lock()
            .await
            .iter()
            .filter(|(id, plugin)| {
                !alive.get(*id).is_some_and(|(provider_id, plugin_dir)| {
                    provider_id == &plugin.provider_id && plugin_dir == &plugin.plugin_dir
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.stop(&id).await;
        }
    }

    pub async fn shutdown(&self) {
        let ids: Vec<String> = self.running.lock().await.keys().cloned().collect();
        for id in ids {
            self.stop(&id).await;
        }
    }
}

fn validate_id(id: &str) -> Result<(), ProxyError> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(plugin_error(
            "插件 id 只能包含 ASCII 字母、数字、_ 和 -，长度为 1..64",
        ))
    }
}

fn resolve_command(
    command: &str,
    plugin_dir: &Path,
    path_snapshot: Option<&std::ffi::OsStr>,
    pathext_snapshot: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, ProxyError> {
    let input = Path::new(command);
    let has_path = input.is_absolute() || command.contains('/') || command.contains('\\');
    let base = if has_path {
        if input.is_absolute() {
            input.to_path_buf()
        } else {
            plugin_dir.join(input)
        }
    } else {
        let path = path_snapshot.unwrap_or_default();
        let mut found = None;
        'dirs: for dir in std::env::split_paths(path) {
            for candidate in executable_candidates(&dir.join(input), pathext_snapshot) {
                if candidate.is_file() && is_executable(&candidate) {
                    found = Some(candidate);
                    break 'dirs;
                }
            }
        }
        found.ok_or_else(|| {
            plugin_error(format!(
                "找不到可执行文件 {:?}（裸命令仅按启动时 PATH 查找）",
                command
            ))
        })?
    };
    let resolved = executable_candidates(&base, pathext_snapshot)
        .into_iter()
        .find(|candidate| candidate.is_file() && is_executable(candidate))
        .ok_or_else(|| plugin_error(format!("插件 command 不是可执行文件: {}", base.display())))?;
    resolved
        .canonicalize()
        .map_err(|error| plugin_error(format!("解析插件 command 失败: {error}")))
}

#[cfg(windows)]
fn executable_candidates(
    candidate: &Path,
    pathext_snapshot: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    if candidate.extension().is_some() {
        return vec![candidate.to_path_buf()];
    }
    let mut candidates = vec![candidate.to_path_buf()];
    let extensions = pathext_snapshot
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for extension in extensions {
        let mut path = candidate.as_os_str().to_os_string();
        path.push(extension);
        candidates.push(PathBuf::from(path));
    }
    candidates
}

#[cfg(not(windows))]
fn executable_candidates(
    candidate: &Path,
    _pathext_snapshot: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    vec![candidate.to_path_buf()]
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn reserve_loopback_port() -> Result<(SocketAddr, u16), ProxyError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| plugin_error(format!("分配插件端口失败: {error}")))?;
    let addr = listener
        .local_addr()
        .map_err(|error| plugin_error(format!("读取插件端口失败: {error}")))?;
    drop(listener);
    Ok((addr, addr.port()))
}

fn plugin_error(message: impl Into<String>) -> ProxyError {
    ProxyError::ForwardFailed(format!("插件错误: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_requires_matching_id_and_valid_command() {
        let temp = tempfile::tempdir().unwrap();
        let command = std::env::current_exe().unwrap();
        fs::write(
            temp.path().join(MANIFEST_FILE),
            format!(
                "id = \"demo\"\nname = \"Demo\"\ncommand = {:?}\napi_format = \"openai_chat\"\nmodels = [\"m\"]\n",
                command.to_string_lossy()
            ),
        )
        .unwrap();
        let manifest = PluginManifest::load(temp.path(), "demo").unwrap();
        assert_eq!(manifest.models, ["m"]);
        assert!(PluginManifest::load(temp.path(), "other").is_err());
        let mut provider = ProviderSpec::new(
            "demo",
            "Demo",
            "http://127.0.0.1:0/v1",
            ApiFormat::OpenaiChat,
            ProviderAuth::None,
        );
        provider.plugin = Some(PluginRef {
            plugin_id: "demo".into(),
            dir: Some(temp.path().to_string_lossy().into_owned()),
        });
        PluginSupervisor::new(temp.path())
            .unwrap()
            .validate_providers([&provider])
            .unwrap();
        provider.auth = ProviderAuth::api_key("must-not-be-accepted");
        assert!(PluginSupervisor::new(temp.path())
            .unwrap()
            .validate_providers([&provider])
            .is_err());
    }

    #[test]
    fn plugin_ids_are_safe_as_directory_names() {
        for valid in ["workbuddy", "my_plugin-1", "a9"] {
            assert!(validate_id(valid).is_ok());
        }
        for invalid in ["", "../secret", "a/b", "has space", "é"] {
            assert!(validate_id(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn command_lookup_rejects_missing_paths() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve_command(
            "no-such-tern-plugin-executable",
            temp.path(),
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("PATHEXT").as_deref(),
        )
        .is_err());
    }
}

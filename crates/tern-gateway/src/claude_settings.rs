//! `~/.claude/settings.json` 的读改写。
//!
//! # 为什么收到同一个模块里
//!
//! 这份逻辑在 `tern-app` 里抄了三遍：`model.rs` 管模型档位、`wire.rs` 管接线、
//! `permissions.rs` 管权限放行。三份的 `read_json` 一字不差，`ensure_env` 行为相同只是
//! 写法不同，只有备份语义真的不一样（接线存"接线前的原值"、模型存"改之前的整份"）。
//!
//! `tern use` 要在 CLI 里写同一个文件——那是第四遍。`model.rs` 里原来写着
//! "真共用的那一天再把它们提到一个新模块里"，就是现在。
//!
//! 放在 `tern-gateway` 而不是新建 crate：`tern-cli` 和 `tern-app` 都已经依赖它，
//! 而它本来就托管着 Claude Code 相邻的关注点（`ccswitch_import` 读 cc-switch 的库、
//! `web_tools` 判第三方网关）。新文件不在 `proxy/` 里，不违反正字节一致的约束。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClaudeSettingsError {
    #[error("找不到用户主目录，定位不到 ~/.claude")]
    NoHomeDir,
    #[error("{0} 读取失败: {1}")]
    Read(String, std::io::Error),
    #[error("{0} 解析失败: {1}")]
    Parse(String, serde_json::Error),
    #[error("{0} 写入失败: {1}")]
    Write(String, std::io::Error),
    /// 序列化失败。和 Write 分开：那是两种完全不同的病因（前者是磁盘/权限，
    /// 后者是我们手里的值根本不符合 JSON），报错信息也该不同
    #[error("{0} 序列化失败: {1}")]
    Serialize(String, serde_json::Error),
    /// 文件形状和预期不符（比如 `env` 被写成了数组）。这种要挡：
    /// 默默替换会丢用户的东西。
    #[error("{0}")]
    Shape(String),
}

pub type Result<T> = std::result::Result<T, ClaudeSettingsError>;

/// 落盘前留一份备份。
///
/// 三处调用方的语义确实不同，所以这是个枚举而不是一个 bool——用 bool 的那天
/// 总有一处会悄悄用错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backup {
    /// 不留。调用方自己有别的保存途径，或这次改动本就是可重放的
    Skip,
    /// 备份成 `<原文件名><后缀>`，每次写都刷新成"写之前那份"
    EachWrite(&'static str),
    /// 同上，但只在备份还不存在时写——保留"用户改之前"那份，不被后续改动覆盖
    FirstOnly(&'static str),
}

impl Backup {
    fn suffix(self) -> Option<&'static str> {
        match self {
            Self::Skip => None,
            Self::EachWrite(suffix) | Self::FirstOnly(suffix) => Some(suffix),
        }
    }

    fn keep_first_only(self) -> bool {
        matches!(self, Self::FirstOnly(_))
    }
}

/// Claude Code 的家目录。`CLAUDE_CONFIG_DIR` 优先，其次 `~/.claude`。
///
/// 环境变量优先是 Claude Code 自己的规矩，不是我们发明的：它自己也是这么找配置的。
/// 不认的话，把配置挪到别处的用户会在我们这儿看到"未接线"，而他明明接好了。
pub fn claude_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir()
        .map(|home| home.join(".claude"))
        .ok_or(ClaudeSettingsError::NoHomeDir)
}

/// 只认 `settings.json`：用户级就读这一个文件。
pub fn settings_path(claude_dir: &Path) -> PathBuf {
    claude_dir.join("settings.json")
}

/// 读一份 JSON 配置文件。容忍 BOM 和空文件，不存在就是空对象。
///
/// 两个容忍都不是洁癖：PowerShell 5 的 `Set-Content -Encoding utf8` 会写 BOM，而
/// `serde_json` 不认；空文件则是用户手清空内容后的常见状态。
pub fn read_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|source| ClaudeSettingsError::Read(path.display().to_string(), source))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(text)
        .map_err(|source| ClaudeSettingsError::Parse(path.display().to_string(), source))
}

/// 拿到底层 JSON 里 `env` 那个对象，没有就建一个。
///
/// `env` 已存在但不是对象时**报错**而不是覆盖：那说明有人（用户或别的工具）把它写成了
/// 别的形状，默默替换会丢东西。
pub fn ensure_env(root: &mut Map<String, Value>) -> Result<&mut Map<String, Value>> {
    let env = root
        .entry("env".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !env.is_object() {
        return Err(ClaudeSettingsError::Shape(
            "settings.json 的 env 已存在且不是对象".into(),
        ));
    }
    env.as_object_mut()
        .ok_or_else(|| ClaudeSettingsError::Shape("settings.json 的 env 已存在且不是对象".into()))
}

/// 读-改-写的最后一步：备份 + 落盘。
///
/// 备份名拼在全名后面（`settings.json.tern-bak`）而不是用 `with_extension`：
/// 后者会把扩展名顶掉，变成 `settings.tern-bak`——用户认不出那是什么的备份。
pub fn write_json(path: &Path, root: &Value, backup: Backup) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| {
                ClaudeSettingsError::Write(parent.display().to_string(), source)
            })?;
        }
    }
    if let Some(suffix) = backup.suffix() {
        if path.exists() {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            let target = PathBuf::from(name);
            let stale = backup.keep_first_only() && target.exists();
            if !stale {
                std::fs::copy(path, &target).map_err(|source| {
                    ClaudeSettingsError::Write(target.display().to_string(), source)
                })?;
            }
        }
    }
    let text = serde_json::to_string_pretty(root)
        .map_err(|source| ClaudeSettingsError::Serialize(path.display().to_string(), source))?;
    std::fs::write(path, text + "\n")
        .map_err(|source| ClaudeSettingsError::Write(path.display().to_string(), source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn missing_file_reads_as_empty_object() {
        let dir = temp_dir();
        let path = dir.path().join("settings.json");
        let value = read_json(&path).unwrap();
        assert!(value.is_object());
        assert!(value.as_object().unwrap().is_empty());
    }

    /// PowerShell 5 的 `Set-Content -Encoding utf8` 会写 BOM，serde_json 不认。
    /// 不容忍的话用户的 settings.json 在我们这儿永远是"解析失败"。
    #[test]
    fn tolerates_utf8_bom() {
        let dir = temp_dir();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "\u{feff}{\"env\":{}}").unwrap();
        let value = read_json(&path).unwrap();
        assert!(value.pointer("/env").is_some());
    }

    /// 用户手清空内容后的常见状态，不该报错
    #[test]
    fn empty_file_reads_as_empty_object() {
        let dir = temp_dir();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "   \n").unwrap();
        assert!(read_json(&path).unwrap().is_object());
    }

    #[test]
    fn broken_json_reports_the_path() {
        let dir = temp_dir();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        let error = read_json(&path).unwrap_err();
        assert!(
            error.to_string().contains("settings.json"),
            "报错要指出是哪个文件: {error}"
        );
    }

    /// `env` 被写成数组时挡下来。默默替换会丢用户的东西——那是他自己写的配置
    #[test]
    fn env_of_the_wrong_shape_is_refused_not_replaced() {
        let mut root = Map::new();
        root.insert("env".to_string(), json!(["not", "an", "object"]));
        assert!(ensure_env(&mut root).is_err());
    }

    #[test]
    fn env_is_created_when_absent() {
        let mut root = Map::new();
        assert!(ensure_env(&mut root).unwrap().is_empty());
    }

    #[test]
    fn each_write_backup_tracks_the_previous_contents() {
        let dir = temp_dir();
        let path = dir.path().join("settings.json");
        write_json(&path, &json!({"env": {"A": "1"}}), Backup::Skip).unwrap();

        write_json(
            &path,
            &json!({"env": {"A": "2"}}),
            Backup::EachWrite(".tern-bak"),
        )
        .unwrap();
        let backup = read_json(&dir.path().join("settings.json.tern-bak")).unwrap();
        assert_eq!(backup.pointer("/env/A"), Some(&json!("1")));

        // 再写一次，备份跟着变成"上一次"，不是"最初那次"
        write_json(
            &path,
            &json!({"env": {"A": "3"}}),
            Backup::EachWrite(".tern-bak"),
        )
        .unwrap();
        let backup = read_json(&dir.path().join("settings.json.tern-bak")).unwrap();
        assert_eq!(backup.pointer("/env/A"), Some(&json!("2")));
    }

    /// 接线那份备份要的是"用户改之前"，不能被后续改动覆盖
    #[test]
    fn first_only_backup_keeps_the_original() {
        let dir = temp_dir();
        let path = dir.path().join("settings.json");
        write_json(&path, &json!({"env": {"A": "1"}}), Backup::Skip).unwrap();

        for value in ["2", "3"] {
            write_json(
                &path,
                &json!({"env": {"A": value}}),
                Backup::FirstOnly(".tern-bak"),
            )
            .unwrap();
        }
        let backup = read_json(&dir.path().join("settings.json.tern-bak")).unwrap();
        assert_eq!(backup.pointer("/env/A"), Some(&json!("1")));
    }

    /// 备份名拼在全名后面，不是顶掉扩展名：`settings.tern-bak` 用户认不出是什么
    #[test]
    fn backup_name_keeps_the_original_filename() {
        let dir = temp_dir();
        let path = dir.path().join("settings.json");
        // 先有一份，备份只在"写之前已经有文件"时发生
        write_json(&path, &json!({"env": {}}), Backup::Skip).unwrap();
        write_json(
            &path,
            &json!({"env": {"A": "1"}}),
            Backup::EachWrite(".tern-bak"),
        )
        .unwrap();
        assert!(
            dir.path().join("settings.json.tern-bak").exists(),
            "备份名该拼在全名后面"
        );
        assert!(
            !dir.path().join("settings.tern-bak").exists(),
            "不该把扩展名顶掉"
        );
    }

    #[test]
    fn write_creates_missing_parent_dirs() {
        let dir = temp_dir();
        let path = dir
            .path()
            .join("nested")
            .join("deeper")
            .join("settings.json");
        write_json(&path, &json!({"env": {}}), Backup::Skip).unwrap();
        assert!(path.exists());
    }

    /// 落盘带尾换行：没有它有些编辑器和 diff 工具会显示"最后一行没有换行"
    #[test]
    fn written_file_ends_with_a_newline() {
        let dir = temp_dir();
        let path = dir.path().join("settings.json");
        write_json(&path, &json!({"env": {}}), Backup::Skip).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
    }
}

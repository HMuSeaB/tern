//! 选模型：把用户挑的那个模型名写进 `~/.claude/settings.json`。
//!
//! # 为什么模型名要落在 settings.json 而不是 tern.json
//!
//! tern 是网关，它只回答"这个模型名该发给哪家"。**用哪个模型是客户端的事**——
//! Claude Code 从 `env.ANTHROPIC_MODEL` 读自己该用什么，所以选了模型就得写那儿。
//! 写进 `tern.json` 的话 Claude Code 根本不会去看，用户会以为设了其实没设。
//!
//! # 为什么写四个键而不是一个
//!
//! Claude Code 按档位分别取模型：主对话 `ANTHROPIC_MODEL`，后台小活走
//! `ANTHROPIC_DEFAULT_HAIKU_MODEL`，Sonnet / Opus 各有一个。只设主对话的话，
//! 遇到 `/model haiku` 或后台任务仍会用内置默认名——那些名字在第三方站上多半
//! 不存在，于是"明明选了模型，个别请求还是 404"。
//!
//! 所以这里把选中的模型名写进四个键。用户在 Settings 里手工覆盖某一个仍然有效，
//! 我们只在**当前没有值**时才补。
//!
//! # 和接线共用一套口径
//!
//! `wire.rs` 已经为 `ANTHROPIC_BASE_URL` 做了一件同样的事：读-改-写、容忍 BOM、
//! 写前备份、只碰自己负责的键。这里照搬那套规矩——同一个文件，两处写法分叉会让
//! 备份策略和 BOM 处理出现不一致，而那正是最难查的一类问题。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{AppError, Result};

/// Claude Code 读模型名的四个档位。顺序即优先级：
/// 主对话 / Haiku / Sonnet / Opus。
const MODEL_KEYS: [&str; 4] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
];

/// 当前 Claude Code 在用的模型名。没有配过就返回 None。
pub fn current_model(claude_dir: &Path) -> Option<String> {
    let settings = read_settings(&claude_dir.join("settings.json")).ok()?;
    let env = settings.get("env").and_then(Value::as_object)?;
    for key in MODEL_KEYS {
        if let Some(model) = env.get(key).and_then(Value::as_str) {
            let model = model.trim();
            if !model.is_empty() {
                return Some(model.to_string());
            }
        }
    }
    None
}

/// 把 `model` 写进四个键。返回实际改动过的键名，前端可以拿去说"设了哪个"。
///
/// 已经有值的键**不动**：用户在 Settings 里给 Opus 单独指定过别的模型时，
/// 一刀切覆盖会把那个选择抹掉。只在空着的时候补。
pub fn set_model(claude_dir: &Path, model: &str) -> Result<Vec<String>> {
    let model = model.trim();
    if model.is_empty() {
        return Err(AppError::Config("模型名不能为空".into()));
    }
    let path = claude_dir.join("settings.json");
    let mut root = read_settings(&path)?;
    let Value::Object(ref mut obj) = root else {
        return Err(AppError::Config("settings.json 顶层不是对象".into()));
    };
    let env = ensure_env(obj)?;

    let old_main = env
        .get("ANTHROPIC_MODEL")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut changed = Vec::new();

    // 1. 主模型键始终以用户选中的为准
    let current_main = env
        .get("ANTHROPIC_MODEL")
        .and_then(Value::as_str)
        .map(str::trim);
    if current_main != Some(model) {
        env.insert(
            "ANTHROPIC_MODEL".to_string(),
            Value::String(model.to_string()),
        );
        changed.push("ANTHROPIC_MODEL".to_string());
    }

    // 2. 其余档位键：为空或原值曾跟随旧主模型批量设置的，跟随更新；
    //    用户单独定制过的（与旧主模型不一致）予以保留
    for key in [
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
    ] {
        let current_val = env.get(key).and_then(Value::as_str).map(str::trim);

        let should_update = match current_val {
            None => true,
            Some(v) if v.is_empty() => true,
            Some(v) => old_main.as_deref() == Some(v) && v != model,
        };

        if should_update {
            env.insert(key.to_string(), Value::String(model.to_string()));
            changed.push(key.to_string());
        }
    }

    if changed.is_empty() {
        return Ok(changed);
    }
    write_settings(&path, &root)?;
    Ok(changed)
}

/// 清掉这四个键，让 Claude Code 回到它自己的默认。
///
/// 只删**我们写过的**：也就是值和传入模型名一致的那些。用户自己填的别的值
/// 不该被"重置"抹掉。
pub fn clear_model(claude_dir: &Path, model: &str) -> Result<Vec<String>> {
    let model = model.trim();
    if model.is_empty() {
        return Err(AppError::Config("模型名不能为空".into()));
    }
    let path = claude_dir.join("settings.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut root = read_settings(&path)?;
    let Value::Object(ref mut obj) = root else {
        return Ok(Vec::new());
    };
    let Some(env) = obj.get_mut("env").and_then(Value::as_object_mut) else {
        return Ok(Vec::new());
    };

    let mut changed = Vec::new();
    for key in MODEL_KEYS {
        let ours = env
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| value.trim() == model);
        if ours {
            env.remove(key);
            changed.push(key.to_string());
        }
    }
    if changed.is_empty() {
        return Ok(changed);
    }
    write_settings(&path, &root)?;
    Ok(changed)
}

/// Claude Code 的家目录。与 `permissions::claude_dir` 同一口径，
/// 见那边的注释（`CLAUDE_CONFIG_DIR` 优先，其次 `~/.claude`）。
pub fn claude_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir()
        .map(|home| home.join(".claude"))
        .ok_or_else(|| AppError::Config("找不到用户主目录".into()))
}

// ---- 下面几个是 wire.rs 同一套做法的私有副本 ----
// 刻意不挪成 pub：那边是接线的实现细节，这边借的是"同一套规矩"而不是"同一段代码"。
// 真共用的那一天再把它们提到一个新模块里。

fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| AppError::Config(e.to_string()))?;
    // BOM：PowerShell 5 的 Set-Content -Encoding utf8 会写，serde_json 不认
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(text)
        .map_err(|e| AppError::Config(format!("{} 解析失败: {e}", path.display())))
}

/// 拿到底层 JSON 里 `env` 那个对象，没有就建一个。
///
/// `env` 已存在但不是对象时**报错**而不是覆盖：那说明有人把它写成了别的形状，
/// 默默替换会丢东西。
fn ensure_env(root: &mut Map<String, Value>) -> Result<&mut Map<String, Value>> {
    let env = root
        .entry("env".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    env.as_object_mut()
        .ok_or_else(|| AppError::Config("settings.json 的 env 不是对象".into()))
}

fn write_settings(path: &Path, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::Config(e.to_string()))?;
    }
    // 和接线共用一个备份文件名会很理想，但两处的备份语义不同（接线存的是
    // "接线前的原值"，这里存的是"改模型前的整份"），分开更不容易混淆
    let backup = path.with_extension("json.tern-model-bak");
    if path.exists() {
        let _ = std::fs::copy(path, &backup);
    }
    let text = serde_json::to_string_pretty(root).map_err(|e| AppError::Config(e.to_string()))?;
    std::fs::write(path, text + "\n").map_err(|e| AppError::Config(e.to_string()))
}

// ---------------------------------------------------------------------------
// tauri 命令
// ---------------------------------------------------------------------------

/// 当前模型名。前端拿来高亮下拉里选中项。
#[tauri::command]
pub fn claude_model() -> Result<Option<String>> {
    Ok(current_model(&claude_dir()?))
}

/// 设模型。写进 settings.json 的四个档位键（已有人值的除外）。
#[tauri::command]
pub fn set_claude_model(model: String) -> Result<Vec<String>> {
    set_model(&claude_dir()?, &model)
}

/// 清模型。只删值和 `model` 一致的键——用户另填的不会被抹掉。
#[tauri::command]
pub fn clear_claude_model() -> Result<Vec<String>> {
    let dir = claude_dir()?;
    let Some(current) = current_model(&dir) else {
        return Ok(Vec::new());
    };
    clear_model(&dir, &current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().to_path_buf();
        (d, path)
    }

    fn write(dir: &Path, json: &str) {
        std::fs::write(dir.join("settings.json"), json).unwrap();
    }

    fn read(dir: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap()
    }

    /// 四个档位都要设上。只设主对话的话，后台小活仍会用内置默认名，
    /// 而那个名字在第三方站上多半不存在——"明明选了模型，个别请求还是 404"
    /// 就是这么来的。
    #[test]
    fn setting_a_model_fills_all_four_tiers() {
        let (_g, dir) = dir();
        let changed = set_model(&dir, "step-3.5-flash-2603").unwrap();
        assert_eq!(changed.len(), 4, "四个键都该被写");

        let env = read(&dir)["env"].as_object().unwrap().clone();
        for key in MODEL_KEYS {
            assert_eq!(env[key], "step-3.5-flash-2603", "{key}");
        }
    }

    /// 已经有值的键不许覆盖。用户在 Settings 里给 Opus 单独指定过模型时，
    /// 一刀切会把那个选择抹掉——那是静默丢失用户配置。
    #[test]
    fn an_existing_value_is_never_overwritten() {
        let (_g, dir) = dir();
        write(
            &dir,
            r#"{"env":{"ANTHROPIC_DEFAULT_OPUS_MODEL":"my-own-opus"}}"#,
        );
        set_model(&dir, "step-3.5-flash").unwrap();

        let env = read(&dir)["env"].as_object().unwrap().clone();
        assert_eq!(
            env["ANTHROPIC_DEFAULT_OPUS_MODEL"], "my-own-opus",
            "用户的值不能被覆盖"
        );
        assert_eq!(env["ANTHROPIC_MODEL"], "step-3.5-flash");
        assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "step-3.5-flash");
    }

    /// 空值算没设：用户留了个空串时应当被补上。
    #[test]
    fn a_blank_value_counts_as_unset() {
        let (_g, dir) = dir();
        write(&dir, r#"{"env":{"ANTHROPIC_MODEL":"   "}}"#);
        set_model(&dir, "step-3.5-flash").unwrap();
        assert_eq!(read(&dir)["env"]["ANTHROPIC_MODEL"], "step-3.5-flash");
    }

    /// 模型名周围的空白要去掉再写。粘过来的模型名带个换行很常见，
    /// 带着它写进 settings.json 会让 Claude Code 永远匹配不上。
    #[test]
    fn the_model_name_is_trimmed() {
        let (_g, dir) = dir();
        set_model(&dir, "  step-3.5-flash\n").unwrap();
        assert_eq!(read(&dir)["env"]["ANTHROPIC_MODEL"], "step-3.5-flash");
    }

    /// 空模型名要拒。空串写进去等于把四个键全设成空，比不设更难查。
    #[test]
    fn a_blank_model_is_refused() {
        let (_g, dir) = dir();
        assert!(set_model(&dir, "   ").is_err());
        assert!(!dir.join("settings.json").exists(), "不该留下文件");
    }

    /// 读当前模型：按档位顺序取第一个非空的。
    #[test]
    fn the_current_model_is_the_first_occupied_tier() {
        let (_g, dir) = dir();
        assert_eq!(current_model(&dir), None, "没有文件时是 None");
        write(
            &dir,
            r#"{"env":{"ANTHROPIC_DEFAULT_HAIKU_MODEL":"h","ANTHROPIC_MODEL":"m"}}"#,
        );
        assert_eq!(current_model(&dir).as_deref(), Some("m"));
    }

    /// 清理只删**和当前模型同名**的键。用户在 Opus 上另填的值必须留着。
    #[test]
    fn clearing_only_removes_the_keys_that_match() {
        let (_g, dir) = dir();
        write(
            &dir,
            r#"{"env":{"ANTHROPIC_MODEL":"m","ANTHROPIC_DEFAULT_OPUS_MODEL":"other"}}"#,
        );
        let removed = clear_model(&dir, "m").unwrap();
        assert_eq!(removed, vec!["ANTHROPIC_MODEL"]);
        let env = read(&dir)["env"].as_object().unwrap().clone();
        assert_eq!(
            env["ANTHROPIC_DEFAULT_OPUS_MODEL"], "other",
            "别人的值要留住"
        );
        assert!(env.get("ANTHROPIC_MODEL").is_none());
    }

    /// 文件不存在时清理是无操作，不该报错也不该建文件。
    #[test]
    fn clearing_without_a_file_is_a_noop() {
        let (_g, dir) = dir();
        assert!(clear_model(&dir, "m").unwrap().is_empty());
        assert!(!dir.join("settings.json").exists());
    }

    /// 容忍 BOM。PowerShell 写出来的文件 serde_json 不认，
    /// 不剥 BOM 会让整个命令在"读不了配置"上失败。
    #[test]
    fn a_bom_is_tolerated() {
        let (_g, dir) = dir();
        std::fs::write(
            dir.join("settings.json"),
            "\u{feff}{\"env\":{\"ANTHROPIC_MODEL\":\"old\"}}",
        )
        .unwrap();
        assert_eq!(current_model(&dir).as_deref(), Some("old"));
    }

    /// 只碰 env 里的模型键，用户别的字段一个字都不能动。
    /// 这个文件里可能有 includeCoAuthoredBy、apiKeyHelper 之类，
    /// 写坏一个都是难查的问题。
    #[test]
    fn unrelated_fields_survive_a_write() {
        let (_g, dir) = dir();
        write(
            &dir,
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:15800","OTHER":"keep"},
                "includeCoAuthoredBy":false,"permissions":{"allow":["Bash(ls:*)"]}}"#,
        );
        set_model(&dir, "step-3.5-flash").unwrap();

        let root = read(&dir);
        assert_eq!(root["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:15800");
        assert_eq!(root["env"]["OTHER"], "keep");
        assert_eq!(root["includeCoAuthoredBy"], false);
        assert_eq!(root["permissions"]["allow"][0], "Bash(ls:*)");
    }
}

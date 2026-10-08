//! Claude Code 权限的读取与「一键放行」。
//!
//! # 为什么要这个
//!
//! auto 模式下，任何它评估不了的工具调用都会被一律挡掉——不是它在拒绝用户，
//! 是它判不了时按最保守处理。用户主观上"全放行"，配置文件里并没有这句话。
//! 让用户自己手改 JSON 不现实（他连 `.claude` 在哪都不知道）。
//!
//! 所以在面板里摆几个开关，点一下就把对应规则写进去。用户看到的是开关，
//! 不是 `Bash(cargo:*)` 这种语法。
//!
//! # 为什么是 `settings.json` 而不是 `settings.local.json`
//!
//! 原计划写 `settings.local.json`（"Claude Code 留给自己用的，没别的工具会碰"），
//! 实测证伪了：用户级根本不读这个文件。
//!
//! 判据（临时 `CLAUDE_CONFIG_DIR` 隔离实验，不碰真实配置）：
//! - 对照组：`settings.json` 的 `env.ANTHROPIC_BASE_URL` 指向死端口 →
//!   `claude -p` 报 ConnectionRefused，说明该文件确实被读取
//! - 实验组：`settings.json` 仍指死端口、`settings.local.json` 指活端口 →
//!   依旧 ConnectionRefused，用的是 settings.json 的值
//! - 二进制里那串 `legacy settings.local.json` / `Transform failed against ...`
//!   是**项目级** `.claude/settings.local.json` 的迁移路径，跟用户级无关；
//!   用户级跑一次也没有 transform 出新文件
//!
//! # cc-switch 会把它冲掉
//!
//! `~/.claude/settings.json` 被 cc-switch 托管（`env` 里的 `PROXY_MANAGED` 是证据）：
//! 每次切供应商，它都用供应商的 `settings_config` 整份重写
//! （`sanitize_claude_settings_for_live` 只剥内部字段，其余原样覆盖）。
//! 所以写进去的 `permissions.allow` 会在下次切换时消失。
//!
//! 这里的对策是**如实显示**：每次开面板都重新读文件，开关状态跟着文件走，
//! 被冲掉了用户看见的是"关着"，再点一次即可。不假装持久——
//! 真正的持久要等 tern 常驻后才能做（见 ROADMAP 的托盘/守护计划）。
//!
//! # 安全边界
//!
//! - 只增不删：放行是往 `permissions.allow` 里加一条，绝不主动删用户已有的规则
//! - 不碰 deny：deny 是用户明确要禁的东西（比如 WebSearch），动它是反用户意图
//! - 读失败不当成空：读不出来就报错，不拿"看起来是空的"去覆盖，
//!   那会把用户辛辛苦苦配的东西一次清光
//! - 首次改动前留一份 `.tern-bak`：读-改-写本身是安全的（只在数组尾部加一项），
//!   但这份保险几乎不花钱

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{AppError, Result};

/// 可放行的权限项。`rule` 是写进 allow 的 Claude Code 规则语法，
/// 其余字段给 UI 显示。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPreset {
    /// 稳定 id，前端用它记住开关状态
    pub id: String,
    /// 给人看的名字，如「cargo 构建」
    pub label: String,
    /// 说明点下去会发生什么
    pub detail: String,
    /// 写进 permissions.allow 的规则
    pub rule: String,
    /// 是否已在 allow 里
    pub enabled: bool,
    /// 是否被 deny 挡着。被 deny 的放行是无效的，置灰并说明
    pub denied: bool,
}

/// 一组预设。**故意不放"全部放行"**：那个开关等于关掉整个权限系统，
/// 一旦误点没有任何挽回余地，逐项点能让人看清每一项的代价。
pub fn permission_presets() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![
        (
            "cargo",
            "cargo 构建",
            "允许 cargo build / test / clippy / run 等所有子命令",
            "Bash(cargo:*)",
        ),
        (
            "pnpm",
            "pnpm / npm",
            "允许 pnpm、npm 装依赖与跑脚本",
            "Bash(pnpm:*)",
        ),
        ("node", "node", "允许直接跑 node 脚本", "Bash(node:*)"),
        (
            "git",
            "git",
            "允许 git status / add / commit / push 等只读与写操作",
            "Bash(git:*)",
        ),
        (
            "tern-files",
            "tern 项目文件",
            "允许读写 D:\\4rchive\\Code\\tern 下的文件",
            "Edit(//D:/4rchive/Code/tern/**)",
        ),
    ]
}

/// Claude Code 配置目录：`~/.claude`。可被 `CLAUDE_CONFIG_DIR` 覆盖（多账户）。
///
/// `wire` 也要用同一个目录：权限和接线动的是同一份 settings.json，
/// 分成两个函数必然会有哪天改到一个不读另一个读的地方。
pub fn claude_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir()
        .map(|home| home.join(".claude"))
        .ok_or(AppError::NoConfigDir)
}

/// 只认 `settings.json`：用户级就读这一个文件（见模块顶部的实测记录）。
fn settings_path(claude_dir: &std::path::Path) -> std::path::PathBuf {
    claude_dir.join("settings.json")
}

/// 读一份 JSON 配置文件。不存在时返回空对象——调用方会往里头加东西。
fn read_json(path: &std::path::Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| AppError::Config(e.to_string()))?;
    // 容忍 BOM：PowerShell 5 的 Set-Content -Encoding utf8 会写，serde_json 不认
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(text)
        .map_err(|e| AppError::Config(format!("{} 解析失败: {e}", path.display())))
}

/// 列出所有预设及当前状态。文件不存在时全部视为未放行。
///
/// 目录由参数传进来而不是自己读环境变量：测试直接给临时目录，
/// 不用去动进程级环境变量——那个是所有线程共享的，并行测试会互相踩。
#[tauri::command]
pub fn list_permissions() -> Result<Vec<PermissionPreset>> {
    list_at(&claude_dir()?)
}

fn list_at(claude_dir: &std::path::Path) -> Result<Vec<PermissionPreset>> {
    let settings = read_json(&settings_path(claude_dir))?;
    let allow = string_array(settings.pointer("/permissions/allow"));
    let deny = string_array(settings.pointer("/permissions/deny"));

    Ok(permission_presets()
        .into_iter()
        .map(|(id, label, detail, rule)| PermissionPreset {
            id: id.to_string(),
            label: label.to_string(),
            detail: detail.to_string(),
            rule: rule.to_string(),
            enabled: allow.iter().any(|r| r == rule),
            denied: deny.iter().any(|r| r == rule),
        })
        .collect())
}

/// 开一条放行。
#[tauri::command]
pub fn allow_permission(rule: String) -> Result<Vec<PermissionPreset>> {
    write_allow_at(&claude_dir()?, &rule, true)
}

/// 关一条放行。
#[tauri::command]
pub fn revoke_permission(rule: String) -> Result<Vec<PermissionPreset>> {
    write_allow_at(&claude_dir()?, &rule, false)
}

/// 只改这一条，其余原样写回。
///
/// 最容易出的错是"重新序列化整个文件"：注释一定会丢（serde_json 不认识注释），
/// 键顺序也会被按字母排（没开 `preserve_order`）。所以这里只在用户真的点了开关
/// 时才写，不每次开面板都碰文件——用户自己写的注释能多活一会儿是一会儿。
/// **值**是完整保留的：用户原有的放行规则、model、theme 一个都不会少。
fn write_allow_at(
    claude_dir: &std::path::Path,
    rule: &str,
    add: bool,
) -> Result<Vec<PermissionPreset>> {
    let path = settings_path(claude_dir);
    let mut root = read_json(&path)?;

    let permissions = root
        .as_object_mut()
        .ok_or_else(|| AppError::Config("配置文件顶层不是对象".into()))?
        .entry("permissions")
        .or_insert_with(|| Value::Object(Map::new()));

    if !permissions.is_object() {
        return Err(AppError::Config("permissions 已存在且不是对象".into()));
    }

    let allow = permissions
        .as_object_mut()
        .unwrap()
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()));

    let array = allow
        .as_array_mut()
        .ok_or_else(|| AppError::Config("permissions.allow 已存在且不是数组".into()))?;

    if add {
        if !array.iter().any(|v| v.as_str() == Some(rule)) {
            array.push(Value::String(rule.to_string()));
        }
    } else {
        array.retain(|v| v.as_str() != Some(rule));
    }

    let text = serde_json::to_string_pretty(&root).map_err(|e| AppError::Config(e.to_string()))?;
    // 配置目录理论上一定在（用户要跑 Claude Code 才有权限这回事），
    // 但真不在也别报错就完事：建出来再写，比弹一个"路径不存在"有用
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::Config(e.to_string()))?;
    }
    backup_once(&path)?;
    std::fs::write(&path, text + "\n").map_err(|e| AppError::Config(e.to_string()))?;

    list_at(claude_dir)
}

/// 取一个 JSON 指针处的字符串数组。指错位置或类型不对都返回空——
/// 读不出来不等于"没配"。
fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 改动前留一份滚动备份，已有就不覆盖。
///
/// 读-改-写本身是安全的（只在数组尾部加一项，其余值原样带走），这份备份是
/// 防"我对代码的理解有误"。故意只留一份：多份备份只会让人分不清哪份是哪个，
/// 而真正该回退的场景（ tern 写坏了）永远最近那一份才有用。
/// 失败不阻断写操作——备份是保险，不是前置条件。
fn backup_once(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".tern-bak");
    let backup = PathBuf::from(backup);
    if backup.exists() {
        return Ok(());
    }
    std::fs::copy(path, &backup).map_err(|e| AppError::Config(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试一个独立临时目录，绝不碰用户真实的 `~/.claude`。
    ///
    /// 目录由参数传给 `write_allow_at` / `list_at`，**不设环境变量**：
    /// `CLAUDE_CONFIG_DIR` 是进程级共享状态，并行测试里互相踩，
    /// 而且 TempDir 一被提前 drop，别的测试就开始往不存在的路径写。
    fn isolated_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    /// allow 和 deny 现在同住 `settings.json`，这就是实际形态
    fn settings(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("settings.json")
    }

    #[test]
    fn allows_a_rule_into_a_fresh_settings() {
        let (_guard, dir) = isolated_dir();

        let items = write_allow_at(&dir, "Bash(cargo:*)", true).unwrap();
        let cargo = items.iter().find(|p| p.rule == "Bash(cargo:*)").unwrap();
        assert!(cargo.enabled);
        // 别的预设不该被顺带打开
        assert!(items.iter().filter(|p| p.enabled).count() == 1);

        // 真的落盘了
        let text = std::fs::read_to_string(settings(&dir)).unwrap();
        assert!(text.contains("Bash(cargo:*)"), "{text}");
    }

    #[test]
    fn revoking_leaves_other_rules_alone() {
        let (_guard, dir) = isolated_dir();
        write_allow_at(&dir, "Bash(cargo:*)", true).unwrap();
        write_allow_at(&dir, "Bash(git:*)", true).unwrap();

        let items = write_allow_at(&dir, "Bash(cargo:*)", false).unwrap();
        let cargo = items.iter().find(|p| p.rule == "Bash(cargo:*)").unwrap();
        let git = items.iter().find(|p| p.rule == "Bash(git:*)").unwrap();
        assert!(!cargo.enabled, "放行被收回");
        assert!(git.enabled, "别的放行不该被牵连");
    }

    #[test]
    fn adding_the_same_rule_twice_does_not_duplicate() {
        let (_guard, dir) = isolated_dir();
        write_allow_at(&dir, "Bash(cargo:*)", true).unwrap();
        let items = write_allow_at(&dir, "Bash(cargo:*)", true).unwrap();

        let count = items.iter().filter(|p| p.enabled).count();
        assert_eq!(count, 1);

        // 文件里也该只有一条：重复规则只会让配置越来越难读
        let text = std::fs::read_to_string(settings(&dir)).unwrap();
        assert_eq!(text.matches("Bash(cargo:*)").count(), 1, "{text}");
    }

    #[test]
    fn existing_user_settings_survive_the_write() {
        let (_guard, dir) = isolated_dir();
        // cc-switch 写的 env、用户自己的字段，放行时都不能被抹掉。
        // 这是最要命的一条：settings.json 整个被 cc-switch 托管，
        // 我们只在 permissions.allow 尾部加一项，其余原样带回
        std::fs::write(
            settings(&dir),
            r#"{"model":"opus","includeCoAuthoredBy":false,
                 "env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:5000",
                        "ANTHROPIC_AUTH_TOKEN":"PROXY_MANAGED"},
                 "permissions":{"allow":["Bash(ls:*)"],"deny":["WebSearch"]}}"#,
        )
        .unwrap();

        write_allow_at(&dir, "Bash(cargo:*)", true).unwrap();

        let text = std::fs::read_to_string(settings(&dir)).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["model"], "opus", "无关字段被抹了");
        assert_eq!(json["includeCoAuthoredBy"], false, "无关字段被抹了");
        assert_eq!(
            json["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:5000",
            "cc-switch 的 env 被抹了"
        );
        assert_eq!(
            json["env"]["ANTHROPIC_AUTH_TOKEN"], "PROXY_MANAGED",
            "cc-switch 的 env 被抹了"
        );
        let allow = string_array(json.pointer("/permissions/allow"));
        assert!(
            allow.contains(&"Bash(ls:*)".to_string()),
            "用户原有放行被抹了"
        );
        assert!(allow.contains(&"Bash(cargo:*)".to_string()));
        // deny 一个字都不能动
        assert_eq!(
            string_array(json.pointer("/permissions/deny")),
            vec!["WebSearch"]
        );
    }

    #[test]
    fn tolerates_utf8_bom_and_empty_file() {
        let (_guard, dir) = isolated_dir();
        let path = settings(&dir);

        std::fs::write(&path, "\u{feff}{}\n").unwrap();
        let items = write_allow_at(&dir, "Bash(node:*)", true).unwrap();
        assert!(items.iter().any(|p| p.enabled && p.rule == "Bash(node:*)"));
        // BOM 不该被带回新文件
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.starts_with('\u{feff}'), "BOM 被写回了");
        std::fs::remove_file(&path).unwrap();

        std::fs::write(&path, "   \n").unwrap();
        let items = write_allow_at(&dir, "Bash(node:*)", true).unwrap();
        assert!(items.iter().any(|p| p.enabled && p.rule == "Bash(node:*)"));
    }

    /// 配置目录整个不存在时也该能建出来：用户可能第一次用 tern，
    /// `~/.claude` 都还没生成
    #[test]
    fn creates_the_config_dir_when_missing() {
        let (guard, dir) = isolated_dir();
        let nested = dir.join("claude-home").join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::remove_dir_all(&nested).unwrap();

        let items = write_allow_at(&nested, "Bash(cargo:*)", true).unwrap();
        assert!(items.iter().any(|p| p.enabled && p.rule == "Bash(cargo:*)"));
        assert!(nested.join("settings.json").exists());
        drop(guard);
    }

    #[test]
    fn rule_blocked_by_deny_is_marked_so_the_ui_can_grey_it_out() {
        let (_guard, dir) = isolated_dir();
        std::fs::write(
            settings(&dir),
            r#"{"permissions":{"deny":["Bash(git:*)"]}}"#,
        )
        .unwrap();

        let items = list_at(&dir).unwrap();
        let git = items.iter().find(|p| p.rule == "Bash(git:*)").unwrap();
        assert!(git.denied, "被 deny 的规则要标出来，放行是无效的");
        let cargo = items.iter().find(|p| p.rule == "Bash(cargo:*)").unwrap();
        assert!(!cargo.denied);
    }

    /// deny 挡着的时候，放行照样能写进 allow（deny 优先级更高，写了也不生效），
    /// 但 `denied` 必须同时为 true：UI 靠它置灰，否则用户以为放行成功了
    #[test]
    fn allowing_a_denied_rule_is_still_reported_as_denied() {
        let (_guard, dir) = isolated_dir();
        std::fs::write(
            settings(&dir),
            r#"{"permissions":{"deny":["Bash(git:*)"]}}"#,
        )
        .unwrap();

        let items = write_allow_at(&dir, "Bash(git:*)", true).unwrap();
        let git = items.iter().find(|p| p.rule == "Bash(git:*)").unwrap();
        assert!(git.denied, "被 deny 的规则要一直标出来，放行实际不生效");
    }

    #[test]
    fn refuses_to_write_when_allow_is_not_an_array() {
        let (_guard, dir) = isolated_dir();
        std::fs::write(
            settings(&dir),
            r#"{"permissions":{"allow":"Bash(cargo:*)"}}"#,
        )
        .unwrap();

        // 字符串而不是数组：不能默默替换成数组，那会丢掉用户原来的写法
        let error = write_allow_at(&dir, "Bash(git:*)", true).unwrap_err();
        assert!(error.to_string().contains("不是数组"), "{error}");
    }

    /// 备份只留一份，且内容等于改动前的原文
    #[test]
    fn keeps_a_single_backup_of_the_original() {
        let (_guard, dir) = isolated_dir();
        let path = settings(&dir);
        let original = r#"{"model":"opus","permissions":{"allow":[]}}"#;
        std::fs::write(&path, original).unwrap();

        write_allow_at(&dir, "Bash(cargo:*)", true).unwrap();
        write_allow_at(&dir, "Bash(git:*)", true).unwrap();

        let backup = std::fs::read_to_string({
            let mut s = path.as_os_str().to_os_string();
            s.push(".tern-bak");
            std::path::PathBuf::from(s)
        })
        .unwrap();
        // 备份停在第一次改动前，不被第二次改动覆盖
        assert_eq!(backup.trim(), original);
        assert!(!backup.contains("Bash(cargo:*)"), "备份被后续改动污染了");
    }

    #[test]
    fn presets_never_include_a_blanket_allow_all() {
        for (id, label, detail, rule) in permission_presets() {
            //  deliberately 没有"全部放行"：那个开关等于关掉整个权限系统，
            //  误点了无法挽回。逐项点能让人看清每一项的代价。
            assert!(
                rule.starts_with("Bash(") || rule.starts_with("Edit("),
                "{id}"
            );
            assert!(!rule.contains(":*:*") && rule != "*", "{id}");
            assert!(!label.is_empty() && !detail.is_empty(), "{id}");
        }
    }
}

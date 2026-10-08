//! 供应商分组。
//!
//! 两件事，一个模块：
//!
//! 1. **自定义文件夹注册表**：用户建的文件夹（名称、顺序、展开状态），以及
//!    "哪个供应商在哪个文件夹"的归属表。落在 `folders.json`。
//! 2. **按请求地址归组的键**：`normalize_url` / `group_key`，纯函数，不落盘。
//!
//! # 为什么单独存一份 `folders.json`，不塞进 `tern.json`
//!
//! `tern.json` 是网关配置，网关和 agent 都要读它；文件夹是纯 UI 概念，网关一个字
//! 都不关心。塞进去有三个代价：
//!
//! - 改分组要去 rewrite 网关配置，而 `server::write_config` 每次都备份一份 `.bak`，
//!   纯 UI 操作不该让用户的网关配置反复产生备份
//! - agent 会解析到无关字段，将来某天有人以为它能用来改路由
//! - 导入覆盖的是整个 `providers` 数组，塞进去的分组数据有没有被一起覆盖全看运气
//!
//! 面板的 `usage.db` 也不行：它是只读打开的（见 `db.rs`），记账的库不承担配置职责。
//!
//! # 为什么归属表按 provider id 而不是按文件夹名
//!
//! cc-switch 那边文件夹名是业务主键（重命名要连着改所有供应商的 `folder` 字段）。
//! tern 的 provider id 在导入时定下、之后不再变，拿 id 当键更稳：重命名只改注册表
//! 一处，归属表一个字都不用动。代价是重命名后归属表里存的是新名字，必须同步改写——
//! 见 [`folders_rename`]。
//!
//! # 注册表 JSON 的形状与 cc-switch 一致
//!
//! `folders` 数组逐字段对齐 cc-switch 的 `ProviderFolder`（camelCase、含 `sortIndex` /
//! `isExpanded`），这样两份数据可以互相看；归属表是 tern 自己的（cc-switch 把归属写进
//! 供应商 meta，这里集中在一处）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// 注册表文件名。放在 `tern.json` 旁边，`TERN_CONFIG` 换位置时跟着一起换。
const FOLDER_FILE_NAME: &str = "folders.json";
/// 写盘时先写这个再 rename。半份 JSON 会让下次读取按空处理——用户的分组静默消失。
const TEMP_SUFFIX: &str = "tmp";

/// 一个自定义文件夹。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFolder {
    /// 稳定标识，只作前端列表 key / 事件定位用。真正定位文件夹看 [`Self::name`]。
    pub id: String,
    /// 文件夹名。trim 后非空，且在注册表内唯一。
    pub name: String,
    /// 排序序号；None 表示用户没排过，排在有序号的后面且保持写入顺序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_index: Option<usize>,
    /// 展开状态。None 按展开处理——38 个供应商折着藏没有任何好处。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_expanded: Option<bool>,
}

impl ProviderFolder {
    /// 是否默认展开。缺省（None）视为展开。
    pub fn expanded(&self) -> bool {
        self.is_expanded.unwrap_or(true)
    }
}

/// `folders.json` 的整个内容。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderFile {
    /// 注册表。读取时已按 `sort_index` 稳定排序。
    #[serde(default)]
    pub folders: Vec<ProviderFolder>,
    /// provider id -> 文件夹名。
    ///
    /// 指向的名字允许不在 `folders` 里（从别处带来的"孤儿归属"）：界面上照样成组，
    /// 只是不能重命名/解散，直到 [`ensure_folder_names`] 把它补登记。
    #[serde(default)]
    pub assignments: BTreeMap<String, String>,
}

/// [`FolderFile::assign`] 的结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AssignOutcome {
    /// 归属实际改动了几条
    pub changed: usize,
    /// 注册表是否被追加了新条目（调用方据此决定要不要落库）
    pub registry_changed: bool,
}

impl FolderFile {
    /// 把 `ids` 归入 `folder`（`None` / 空串 = 移到未分组）。
    ///
    /// 目标文件夹不在注册表里时会补登记：不补的话界面上会出现一个**管不了的**文件夹
    /// （不能重命名、不能解散）。归组允许挑新名字，这条路径就是为它准备的。
    pub fn assign(&mut self, ids: &[String], folder: Option<&str>) -> AssignOutcome {
        let target = folder.map(str::trim).filter(|s| !s.is_empty());
        let mut outcome = AssignOutcome::default();

        for id in ids {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            match target {
                Some(name) => {
                    if self.assignments.get(id).map(String::as_str) != Some(name) {
                        self.assignments.insert(id.to_string(), name.to_string());
                        outcome.changed += 1;
                    }
                }
                None => {
                    if self.assignments.remove(id).is_some() {
                        outcome.changed += 1;
                    }
                }
            }
        }

        if let Some(name) = target {
            outcome.registry_changed =
                ensure_folder_names(&mut self.folders, &[name.to_string()]);
        }
        outcome
    }

    /// 新建文件夹。已存在（trim 后同名）时报错而不是静默成功——静默的话用户会以为
    /// 建了两个，实际还是原来那个。
    pub fn create(&mut self, name: &str) -> Result<()> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(AppError::Folders("文件夹名不能为空".to_string()));
        }
        if self.folders.iter().any(|f| f.name == trimmed) {
            return Err(AppError::Folders(format!(
                "文件夹「{trimmed}」已经存在"
            )));
        }
        self.folders.push(new_folder(trimmed));
        Ok(())
    }

    /// 重命名文件夹，返回被移动的供应商数。
    ///
    /// 旧名不在注册表里也照样改名：归属可能是从 cc-switch 导入带来的（注册表没跟着带），
    /// 界面上它已经是个能看见的组了，不让改名会把它锁死。只有"新名字已被占用"才拒绝——
    /// 那会让两个文件夹合并成一个名字。
    pub fn rename(&mut self, old_name: &str, new_name: &str) -> Result<usize> {
        let old = old_name.trim();
        let new = new_name.trim();
        // 空 / 同名：用户点开对话框又原样关掉是常态，不当错误
        if old.is_empty() || new.is_empty() || old == new {
            return Ok(0);
        }
        if self.folders.iter().any(|f| f.name == new) {
            return Err(AppError::Folders(format!(
                "文件夹「{new}」已经被占用了，换个名字"
            )));
        }

        let registered = rename_folder(&mut self.folders, old, new).is_some();
        let moved = reassign(self, old, new);
        // 注册表原本没有这个文件夹（孤儿组），但确实有供应商归在它下面：
        // 把新名字补登记，改完名用户才能继续重命名/解散它
        if !registered && moved > 0 {
            ensure_folder_names(&mut self.folders, &[new.to_string()]);
        }
        Ok(moved)
    }

    /// 解散文件夹：从注册表移除，并把归属它的供应商移到未分组。返回被移动的数量。
    ///
    /// 注册表里没有这个名字（孤儿组）时也照样清归属，否则界面上这个组会一直挂着，
    /// 且没法再解散第二次。
    pub fn delete(&mut self, name: &str) -> usize {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return 0;
        }
        self.folders.retain(|f| f.name != trimmed);
        let moved = reassign(self, trimmed, "");
        // 归属值清空后这个键没有意义，删掉而不是留一个空串
        self.assignments.retain(|_, v| !v.trim().is_empty());
        moved
    }

    /// 记下某个文件夹的展开 / 收起状态。
    pub fn set_expanded(&mut self, name: &str, expanded: bool) {
        for folder in &mut self.folders {
            if folder.name == name.trim() {
                folder.is_expanded = Some(expanded);
            }
        }
    }

    /// 按请求地址的域名根归组：**同一个域名根下 >= 2 个供应商**才建文件夹。
    ///
    /// # 为什么要求 >= 2
    ///
    /// 只出现一次的域名建一个只装一个供应商的文件夹，只会让文件夹列表更长而没有任何
    /// 分组收益——那种保持未分组更干净。cc-switch 的
    /// `derive_folder_names_from_providers` 是同一个判据。
    ///
    /// # 名字从域名抽，不是让用户起
    ///
    /// 域名只是初值。所以第一次用就能得到一批能看的组，之后用户可以随便改名——
    /// 归属表按 provider id 存，改名不影响任何归属。
    pub fn group_by_domain(&mut self, base_urls: &[(String, String)]) -> Vec<DomainGroup> {
        // 域名根 -> 落在它上面的供应商 id。用 Vec 保序去重，不用 HashSet 打乱顺序。
        let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, base_url) in base_urls {
            let Some(root) = domain_root_of(base_url) else {
                continue;
            };
            let bucket = buckets.entry(root).or_default();
            if !bucket.iter().any(|existing| existing == id) {
                bucket.push(id.clone());
            }
        }

        // 大组排前面：用户第一眼看到的是最有分组价值的那几个
        let mut groups: Vec<DomainGroup> = buckets
            .into_iter()
            .filter(|(_, ids)| ids.len() >= 2)
            .map(|(name, provider_ids)| DomainGroup {
                is_new: !self.folders.iter().any(|f| f.name == name),
                name,
                provider_ids,
            })
            .collect();
        groups.sort_by(|a, b| {
            b.provider_ids
                .len()
                .cmp(&a.provider_ids.len())
                .then_with(|| a.name.cmp(&b.name))
        });

        // 一次改完再让调用方落库：逐个写的话，中途失败会留下"一半归好了"的中间态
        for group in &groups {
            for id in &group.provider_ids {
                self.assignments.insert(id.clone(), group.name.clone());
            }
        }
        ensure_folder_names(
            &mut self.folders,
            &groups.iter().map(|g| g.name.clone()).collect::<Vec<_>>(),
        );
        groups
    }
}

/// 注册表文件路径：`tern.json` 旁边的 `folders.json`。
pub fn folders_path() -> Result<PathBuf> {
    Ok(crate::config::config_path()?
        .with_file_name(FOLDER_FILE_NAME))
}

/// 读注册表。**绝不返回错误**：文件不存在、解析失败、路径定位失败，一律按空表处理
/// 并只留一条日志。文件夹是纯 UI 辅助数据，不该因为它坏了就挡住整个供应商列表渲染。
pub fn read() -> FolderFile {
    let Ok(path) = folders_path() else {
        log::warn!("[folders] 定位不到注册表路径，按空表处理");
        return FolderFile::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return FolderFile::default();
    };
    // 容忍 BOM：和 config::parse 同一个理由，PowerShell 5 的 `Set-Content -Encoding utf8`
    // 会写出 BOM，serde_json 不认
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    match serde_json::from_str::<FolderFile>(text) {
        Ok(mut file) => {
            sort_folders(&mut file.folders);
            file
        }
        Err(error) => {
            log::warn!(
                "[folders] {} 解析失败，按空表处理: {error}",
                path.display()
            );
            FolderFile::default()
        }
    }
}

/// 写注册表。先写临时文件再 rename：直接覆盖原文件的话，进程在写一半被杀会留下
/// 半个 JSON，下次读只能按空处理——用户的分组静默丢失且看不出原因。
pub fn write(file: &FolderFile) -> Result<()> {
    let path = folders_path()?;
    let mut temp = path.clone().into_os_string();
    temp.push(format!(".{TEMP_SUFFIX}"));
    let temp = PathBuf::from(temp);

    let text = serde_json::to_string_pretty(file)?;
    std::fs::write(&temp, text + "\n")
        .map_err(|e| AppError::Folders(format!("写 {} 失败: {e}", temp.display())))?;
    std::fs::rename(&temp, &path)
        .map_err(|e| AppError::Folders(format!("替换 {} 失败: {e}", path.display())))?;
    Ok(())
}

/// 新建一个文件夹条目。id 用 uuid 而不是"当前长度"：重命名/解散会让长度反复重排，
/// 用长度当 id 会撞，撞了会让前端列表复用错位。
fn new_folder(name: &str) -> ProviderFolder {
    ProviderFolder {
        id: format!("folder_{}", uuid::Uuid::new_v4().simple()),
        name: name.to_string(),
        sort_index: None,
        is_expanded: Some(true),
    }
}

// ---------------------------------------------------------------------------
// 纯函数：注册表变换
//
// 这些都不碰磁盘，便于单测。语义与 cc-switch 的 `provider_folders.rs` 一致——两边
// 的注册表数据要能互相看，变换规则也得一致。
// ---------------------------------------------------------------------------

/// 按 `sort_index` 稳定排序；没有 `sort_index` 的排在后面，且它们之间保持写入顺序
/// （`sort_by_key` 是稳定排序），避免用户没排序过的文件夹每次刷新都跳。
pub fn sort_folders(folders: &mut [ProviderFolder]) {
    folders.sort_by_key(|f| f.sort_index.unwrap_or(usize::MAX));
}

/// 确保注册表覆盖到 `names` 里出现的每一个文件夹名。返回是否发生了变更
/// （调用方据此决定要不要落库，避免无意义写入）。
pub fn ensure_folder_names(folders: &mut Vec<ProviderFolder>, names: &[String]) -> bool {
    let mut changed = false;
    for name in names {
        // 归属值允许带首尾空格（用户手改过 json），注册表一律以 trim 后的名字为准
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 每次重新查一遍：上面的 push 可能刚把它加进来
        if folders.iter().any(|f| f.name == trimmed) {
            continue;
        }
        folders.push(new_folder(trimmed));
        changed = true;
    }
    changed
}

/// 重命名注册表条目，返回新名字。新名字已被占用、旧名字不存在、任一参数为空、
/// 新旧同名，都返回 `None` 表示无需变更。
pub fn rename_folder(
    folders: &mut [ProviderFolder],
    old_name: &str,
    new_name: &str,
) -> Option<String> {
    let old = old_name.trim();
    let new = new_name.trim();
    if old.is_empty() || new.is_empty() || old == new {
        return None;
    }
    if folders.iter().any(|f| f.name == new) {
        return None;
    }
    if !folders.iter().any(|f| f.name == old) {
        return None;
    }
    for folder in folders.iter_mut() {
        if folder.name == old {
            folder.name = new.to_string();
        }
    }
    Some(new.to_string())
}

/// 把归属表里 `old_name` 下的供应商全部改到 `new_name`，返回改动条数。
fn reassign(file: &mut FolderFile, old_name: &str, new_name: &str) -> usize {
    let mut moved = 0usize;
    for value in file.assignments.values_mut() {
        // 归属值可能带首尾空格；比对时 trim，写入时归一
        if value.trim() == old_name {
            *value = new_name.to_string();
            moved += 1;
        }
    }
    moved
}

// ---------------------------------------------------------------------------
// 纯函数：按请求地址归组
//
// 口径逐条对齐 cc-switch 的 `providerUrlUtils.normalizeUrl` / `getProviderGroupKey`。
// 同一批供应商在两边必须得到同一个分组键，否则用户换工具时会看到分组对不上。
// ---------------------------------------------------------------------------

/// 规范化 URL：
///
/// 1. 去首尾空白，空串归一为空
/// 2. 协议和 host 转小写，path / query / hash 的大小写原样保留
/// 3. 去掉末尾所有斜杠（`https://a/` 和 `https://a` 是同一个上游）
/// 4. 没写协议的按 `http://` 补全再解析，结果里再把补的协议去掉
///
/// 解析不出（畸形 URL）时退化成"去首尾空白 + 去末尾斜杠"，至少让
/// `https://a/` 和 `https://a` 仍能归到一起。
pub fn normalize_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let has_protocol = has_scheme(trimmed);
    // 补协议的版本只为解析服务，用完就丢。直接 String 而不是借一个临时量：
    // 借临时量的话它在 `let` 语句结束时就没了，而下面还要用
    let to_parse = if has_protocol {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    match split_url(&to_parse) {
        Some((scheme, authority, rest)) => {
            // `split_url` 切掉的是 "://"，这里得原样拼回去
            let normalized = format!(
                "{}://{}{}",
                scheme.to_ascii_lowercase(),
                authority.to_ascii_lowercase(),
                rest
            );
            let normalized = normalized.trim_end_matches('/');
            if has_protocol {
                normalized.to_string()
            } else {
                // 补的 http:// 是解析用的脚手架，不属于用户写的地址
                normalized
                    .strip_prefix("http://")
                    .unwrap_or(normalized)
                    .to_string()
            }
        }
        None => trimmed.trim_end_matches('/').to_string(),
    }
}

/// 归组键：规范化后整体转小写。空地址返回空串，前端据此决定"未配置地址"怎么显示。
///
/// 为什么整体小写而不是只小写 scheme+host：两边对同一个上游写的大小写可能不同
/// （`/Anthropic` vs `/anthropic`），归一后才能归到同一组。
pub fn group_key(url: &str) -> String {
    normalize_url(url).to_lowercase()
}

/// 是不是带协议的 URL（`scheme://`）。用于决定要不要补 `http://`。
fn has_scheme(text: &str) -> bool {
    let Some(index) = text.find("://") else {
        return false;
    };
    let scheme = &text[..index];
    let mut chars = scheme.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    // RFC 3986 的 scheme 只允许这几类字符
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// 把 URL 拆成 (scheme, authority, rest)。`rest` 以 `/`、`?` 或 `#` 开头（或为空）。
/// 拆不出（没有 `://`）返回 None。
fn split_url(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, after) = url.split_once("://")?;
    let end = after
        .find(['/', '?', '#'])
        .unwrap_or(after.len());
    Some((scheme, &after[..end], &after[end..]))
}

/// 从一个请求地址里抽"域名根"当文件夹名。返回 `None` 表示这个地址不该建组。
///
/// # 为什么本地地址一律跳过
///
/// 规则与 cc-switch 的 `domain_root_of` 一致：`localhost` / 字面 IP / IPv6 全是
/// 本地中转，不构成"服务商"，拿它们建组只会得到一堆叫 `127.0.0.1` 的文件夹。
pub fn domain_root_of(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let after_scheme = match trimmed.split_once("://") {
        Some((_, rest)) => rest,
        None => trimmed,
    };
    let host_port = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    // 去掉 userinfo：账号密码里出现 @ 是合法的（`http://user:pass@host/`）
    let host = host_port.rsplit('@').next().unwrap_or(host_port);
    // IPv6 字面量形如 `[::1]:8080`，先剥方括号；带端口的一并剥掉
    let host = host
        .strip_prefix('[')
        .map_or(host, |h| h.split(']').next().unwrap_or(host));
    let host = host.split(':').next().unwrap_or(host);

    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return None;
    }

    // 字面 IPv4：四段全数字
    let is_ipv4 = host.split('.').count() == 4
        && host
            .split('.')
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()));
    if is_ipv4 || host.contains(':') {
        return None;
    }

    let labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    match labels.len() {
        0 | 1 => None,
        2 => Some(labels.join(".")),
        _ => {
            // 处理 co.uk / com.cn 这类二级后缀：再多往前取一段，
            // 否则 "api.example.co.uk" 会退化成 "co.uk"，把不同服务商混到一起。
            // 判据用"倒数第二段短"（<=3 字符，如 co / com / net / gov）
            let take = if labels[labels.len() - 2].len() <= 3 {
                3
            } else {
                2
            };
            Some(labels[labels.len() - take..].join("."))
        }
    }
    .map(|root| root.to_lowercase())
}

// ---------------------------------------------------------------------------
// Tauri 命令
//
// 每个命令都是"读注册表 -> 改 -> 写注册表"一轮。面板是单窗口、分组改动的频率远低于
// 查询，所以不做增量缓存，也不上并发锁——Tauri 的命令已经在同一执行上下文里串行。
// ---------------------------------------------------------------------------

/// 全部文件夹（已排序）。空注册表返回空数组，不是错误。
#[tauri::command]
pub fn folders_list() -> Result<Vec<ProviderFolder>> {
    Ok(read().folders)
}

/// 新建文件夹。
#[tauri::command]
pub fn folders_create(name: String) -> Result<Vec<ProviderFolder>> {
    let mut file = read();
    file.create(&name)?;
    write(&file)?;
    Ok(file.folders)
}

/// 重命名文件夹。返回被移动的供应商数。
#[tauri::command]
pub fn folders_rename(old_name: String, new_name: String) -> Result<usize> {
    let mut file = read();
    let moved = file.rename(&old_name, &new_name)?;
    // 没有供应商跟着搬就只是一次注册表改名；其余情况照写。
    // rename 内部已经处理了"空 / 同名 / 新名被占用"，走到这里都是真改
    write(&file)?;
    Ok(moved)
}

/// 解散文件夹。返回被移回未分组的供应商数。
#[tauri::command]
pub fn folders_delete(name: String) -> Result<usize> {
    let mut file = read();
    let moved = file.delete(&name);
    write(&file)?;
    Ok(moved)
}

/// 把若干供应商归入 `folder`（`None` 或空串 = 移到未分组）。返回改动条数。
#[tauri::command]
pub fn folders_assign(ids: Vec<String>, folder: Option<String>) -> Result<usize> {
    let mut file = read();
    let outcome = file.assign(&ids, folder.as_deref());
    // 一条都没动就不落库：纯 UI 操作不该反复写盘
    if outcome.changed > 0 || outcome.registry_changed {
        write(&file)?;
    }
    Ok(outcome.changed)
}

/// 记下某个文件夹的展开/收起状态。默认展开，用户收起过才写 `false`。
#[tauri::command]
pub fn folders_set_expanded(name: String, expanded: bool) -> Result<Vec<ProviderFolder>> {
    let mut file = read();
    file.set_expanded(&name, expanded);
    write(&file)?;
    Ok(file.folders)
}

/// 一次"按域名归组"的结果。
#[derive(Debug, Serialize)]
pub struct DomainGroup {
    /// 文件夹名（域名根）
    pub name: String,
    /// 归到这个文件夹的供应商 id
    pub provider_ids: Vec<String>,
    /// 这个文件夹是不是这次新建的（false = 已经是注册表里的老文件夹）
    pub is_new: bool,
}

/// 按请求地址的域名根归组。判据见 [`FolderFile::group_by_domain`]。
#[tauri::command]
pub fn folders_group_by_domain() -> Result<Vec<DomainGroup>> {
    let config = crate::config::load(&crate::config::config_path()?)?;
    let base_urls = config
        .providers
        .iter()
        .map(|spec| (spec.id.clone(), spec.effective_base_url()))
        .collect::<Vec<_>>();

    let mut file = read();
    let groups = file.group_by_domain(&base_urls);
    if groups.is_empty() {
        // 没找到够格的域名就别动文件：一次空写会让 folders.json 从"不存在"变成
        // "存在但只有空数组",用户看到的就是一个平白无故出现的文件
        return Ok(groups);
    }
    write(&file)?;
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, sort_index: Option<usize>) -> ProviderFolder {
        ProviderFolder {
            id: format!("id_{name}"),
            name: name.to_string(),
            sort_index,
            is_expanded: Some(true),
        }
    }

    // ---- 注册表变换 ----

    #[test]
    fn ensure_folder_names_appends_missing_and_skips_duplicates() {
        let mut folders = vec![folder("NVIDIA", None)];
        let names = vec![
            "NVIDIA".to_string(),
            "OpenRouter".to_string(),
            "  ".to_string(),
            "OpenRouter".to_string(),
        ];
        assert!(ensure_folder_names(&mut folders, &names));
        assert_eq!(
            folders.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["NVIDIA", "OpenRouter"]
        );
        // 已经齐了就不该再报变更，否则调用方会做无意义写入
        assert!(!ensure_folder_names(
            &mut folders,
            &["NVIDIA".to_string(), "OpenRouter".to_string()]
        ));
    }

    #[test]
    fn ensure_folder_names_trims_and_ignores_blank_only() {
        let mut folders = Vec::new();
        assert!(!ensure_folder_names(&mut folders, &["   ".to_string()]));
        assert!(folders.is_empty(), "纯空白的名字不该建文件夹");
    }

    #[test]
    fn rename_folder_rejects_conflicts() {
        let mut folders = vec![folder("NVIDIA", None), folder("OpenRouter", None)];

        assert_eq!(
            rename_folder(&mut folders, "NVIDIA", "主力"),
            Some("主力".to_string())
        );
        assert_eq!(folders[0].name, "主力");

        // 目标名已存在 → 拒绝（否则两个文件夹会撞名）
        assert_eq!(rename_folder(&mut folders, "主力", "OpenRouter"), None);
        // 旧名不存在 → 拒绝
        assert_eq!(rename_folder(&mut folders, "不存在", "新名字"), None);
        // 空名 → 拒绝
        assert_eq!(rename_folder(&mut folders, "OpenRouter", "   "), None);
        // 新旧同名 → 拒绝
        assert_eq!(rename_folder(&mut folders, "OpenRouter", "OpenRouter"), None);
        assert_eq!(folders[1].name, "OpenRouter", "被拒绝的改名不该留下痕迹");
    }

    #[test]
    fn sort_folders_keeps_unsorted_entries_stable_at_end() {
        let mut folders = vec![
            folder("c", Some(3)),
            folder("unsorted1", None),
            folder("a", Some(1)),
            folder("unsorted2", None),
        ];
        sort_folders(&mut folders);
        assert_eq!(
            folders.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "c", "unsorted1", "unsorted2"]
        );
    }

    #[test]
    fn provider_folder_serializes_camel_case() {
        let json = serde_json::to_string(&folder("NVIDIA", Some(2))).unwrap();
        assert!(json.contains("\"sortIndex\":2"), "got: {json}");
        assert!(json.contains("\"isExpanded\":true"), "got: {json}");
        // 缺失字段不写出来：folders.json 是给人看的，空白字段只是噪声
        assert!(!json.contains("sort_index"), "got: {json}");
    }

    // ---- FolderFile 的增改 ----

    /// 带一个 id 的夹具。`ids` 用真实 uuid 没意义——这里只关心增删逻辑
    fn file_of(folders: &[&str], assignments: &[(&str, &str)]) -> FolderFile {
        FolderFile {
            folders: folders.iter().map(|name| folder(name, None)).collect(),
            assignments: assignments
                .iter()
                .map(|(id, name)| (id.to_string(), name.to_string()))
                .collect(),
        }
    }

    #[test]
    fn create_rejects_blank_and_duplicate_names() {
        let mut file = file_of(&["NVIDIA"], &[]);
        assert!(file.create("   ").is_err(), "空名不该建得出文件夹");
        assert!(file.create("NVIDIA").is_err(), "同名不该静默成功");
        assert_eq!(file.folders.len(), 1, "两次失败都不该留下痕迹");
        file.create("OpenRouter").unwrap();
        assert_eq!(file.folders.len(), 2);
    }

    #[test]
    fn assign_reports_what_actually_changed() {
        let mut file = file_of(&["NVIDIA"], &[("a", "OpenRouter")]);

        // 把 a 从 OpenRouter 挪到 NVIDIA：归属变了，注册表没变
        let outcome = file.assign(&["a".to_string()], Some("NVIDIA"));
        assert_eq!(
            outcome,
            AssignOutcome {
                changed: 1,
                registry_changed: false
            }
        );
        assert_eq!(file.assignments.get("a").map(String::as_str), Some("NVIDIA"));

        // 原地再点一次：已经在目标文件夹，不该算改动（否则每次都白写一次盘）
        let again = file.assign(&["a".to_string()], Some("NVIDIA"));
        assert_eq!(again.changed, 0);
        assert!(!again.registry_changed);
    }

    #[test]
    fn assign_registers_a_folder_that_does_not_exist_yet() {
        let mut file = file_of(&[], &[]);
        let outcome = file.assign(&["a".to_string()], Some("NVIDIA"));
        assert!(outcome.registry_changed, "新名字必须补登记");
        assert_eq!(file.folders.len(), 1, "不登记的话界面上会多个管不了的组");
        assert_eq!(file.folders[0].name, "NVIDIA");
    }

    #[test]
    fn assign_to_none_clears_the_assignment() {
        let mut file = file_of(&["NVIDIA"], &[("a", "NVIDIA"), ("b", "NVIDIA")]);
        let outcome = file.assign(&["a".to_string()], None);
        assert_eq!(outcome.changed, 1);
        assert!(!outcome.registry_changed, "移到未分组不需要建文件夹");
        assert!(!file.assignments.contains_key("a"));
        assert!(file.assignments.contains_key("b"), "只该动点名的那一个");

        // 从没归过组的供应商移到未分组：什么都没发生
        let missing = file.assign(&["zzz".to_string()], None);
        assert_eq!(missing.changed, 0);
    }

    #[test]
    fn assign_skips_blank_ids() {
        let mut file = file_of(&["NVIDIA"], &[]);
        let outcome = file.assign(&["  ".to_string(), String::new()], Some("NVIDIA"));
        assert_eq!(outcome.changed, 0, "空 id 不该占用一条归属");
    }

    #[test]
    fn rename_moves_every_provider_in_that_folder() {
        let mut file = file_of(&["NVIDIA", "OpenRouter"], &[("a", "NVIDIA"), ("b", "NVIDIA")]);
        assert_eq!(file.rename("NVIDIA", "Nvidia").unwrap(), 2);
        assert_eq!(file.assignments.get("a").map(String::as_str), Some("Nvidia"));
        assert_eq!(file.assignments.get("b").map(String::as_str), Some("Nvidia"));
        assert!(
            file.folders.iter().any(|f| f.name == "Nvidia"),
            "注册表也要跟着改，否则改名后这个组就管不了了"
        );
    }

    #[test]
    fn rename_rejects_a_name_already_taken() {
        let mut file = file_of(&["NVIDIA", "OpenRouter"], &[]);
        // 撞名必须拒绝：否则两个文件夹合成一个名字，归属全乱
        assert!(file.rename("NVIDIA", "OpenRouter").is_err());
    }

    /// 归属可以指向注册表里没有的名字（从别处带来的）。这种"孤儿组"也该能改名，
    /// 否则用户看着一个组却永远改不了它的名字。
    #[test]
    fn rename_adopts_an_orphan_group() {
        let mut file = file_of(&[], &[("a", "某个外地组")]);
        assert_eq!(file.rename("某个外地组", "新名字").unwrap(), 1);
        assert_eq!(file.assignments.get("a").map(String::as_str), Some("新名字"));
        assert!(
            file.folders.iter().any(|f| f.name == "新名字"),
            "改完名要补登记，否则这个组还是管不了"
        );
    }

    #[test]
    fn rename_is_a_no_op_for_blank_or_identical_names() {
        let mut file = file_of(&["NVIDIA"], &[("a", "NVIDIA")]);
        assert_eq!(file.rename("NVIDIA", "  ").unwrap(), 0);
        assert_eq!(file.rename("NVIDIA", "NVIDIA").unwrap(), 0);
        assert_eq!(file.assignments.get("a").map(String::as_str), Some("NVIDIA"));
    }

    #[test]
    fn delete_clears_the_registry_and_the_assignments() {
        let mut file = file_of(&["NVIDIA", "OpenRouter"], &[("a", "NVIDIA"), ("b", "OpenRouter")]);
        assert_eq!(file.delete("NVIDIA"), 1);
        assert!(!file.folders.iter().any(|f| f.name == "NVIDIA"));
        assert!(!file.assignments.contains_key("a"), "解散后供应商该回到未分组");
        assert!(file.assignments.contains_key("b"), "别动别的组");
    }

    /// 解散一个注册表里没有的孤儿组，也要把归属清掉——否则界面上这个组会一直挂着，
    /// 且用户没法再解散第二次
    #[test]
    fn delete_also_clears_orphan_groups() {
        let mut file = file_of(&[], &[("a", "幽灵组")]);
        assert_eq!(file.delete("幽灵组"), 1);
        assert!(file.assignments.is_empty());
    }

    #[test]
    fn delete_leaves_no_empty_valued_keys_behind() {
        let mut file = file_of(&["NVIDIA"], &[("a", "NVIDIA")]);
        file.delete("NVIDIA");
        assert!(
            !file.assignments.values().any(|v| v.trim().is_empty()),
            "归属值不该留成空串：读回来那是「已归属但归属为空」的歧义状态"
        );
    }

    #[test]
    fn set_expanded_only_touches_the_named_folder() {
        let mut file = file_of(&["NVIDIA", "OpenRouter"], &[]);
        file.set_expanded("NVIDIA", false);
        assert_eq!(file.folders[0].is_expanded, Some(false));
        assert_eq!(file.folders[1].is_expanded, Some(true), "别的组不该被带着改");
        // 不存在的名字静默无事：那是前端传来的过期 key
        file.set_expanded("不存在", false);
        assert_eq!(file.folders[1].is_expanded, Some(true));
    }

    // ---- 地址归组 ----

    #[test]
    fn normalize_url_lowercases_scheme_and_host_only() {
        assert_eq!(
            normalize_url("https://API.DeepSeek.com/Anthropic"),
            "https://api.deepseek.com/Anthropic",
            "path 的大小写是上游约定，不能动"
        );
    }

    #[test]
    fn normalize_url_strips_trailing_slashes_and_spaces() {
        assert_eq!(
            normalize_url("  https://a.example.com/anthropic///  "),
            "https://a.example.com/anthropic"
        );
    }

    #[test]
    fn normalize_url_fills_in_missing_protocol() {
        assert_eq!(
            normalize_url("api.deepseek.com/anthropic"),
            "api.deepseek.com/anthropic",
            "补的 http:// 只是解析脚手架，不该出现在结果里"
        );
        // 和写了协议的同一个地址必须归到同一组
        assert_eq!(group_key("api.deepseek.com/anthropic"), group_key("API.DeepSeek.com/anthropic"));
    }

    #[test]
    fn normalize_url_keeps_query_and_hash() {
        assert_eq!(
            normalize_url("https://a.example.com/v1?beta=1#frag"),
            "https://a.example.com/v1?beta=1#frag"
        );
    }

    #[test]
    fn normalize_url_falls_back_for_garbage() {
        assert_eq!(normalize_url(""), "");
        assert_eq!(normalize_url("   "), "");
        // 畸形 URL：至少去掉末尾斜杠，让 `https://a/` 和 `https://a` 还能归到一起
        assert_eq!(normalize_url("not a url at all///"), "not a url at all");
    }

    #[test]
    fn group_key_distinguishes_paths_on_the_same_host() {
        // 同一个 host 不同 path 是两个上游，不能因为 host 相同就合并
        assert_ne!(
            group_key("https://www.mcgrox.top/v1"),
            group_key("https://www.mcgrox.top/v2")
        );
    }

    // ---- 域名根 ----

    #[test]
    fn domain_root_of_extracts_registrable_domain() {
        assert_eq!(
            domain_root_of("https://api.deepseek.com/anthropic").as_deref(),
            Some("deepseek.com")
        );
        assert_eq!(
            domain_root_of("https://integrate.api.nvidia.com/v1").as_deref(),
            Some("nvidia.com"),
            "三段以上取后两段"
        );
        assert_eq!(
            domain_root_of("http://user:pass@api.example.io:8080/x").as_deref(),
            Some("example.io"),
            "userinfo 和端口都要剥掉"
        );
        assert_eq!(
            domain_root_of("https://WWW.Example.COM").as_deref(),
            Some("example.com")
        );
        // 二级后缀：往前多取一段，否则会退化成 "co.uk"
        assert_eq!(
            domain_root_of("https://api.example.co.uk").as_deref(),
            Some("example.co.uk")
        );
    }

    #[test]
    fn domain_root_of_rejects_local_and_malformed() {
        assert_eq!(domain_root_of("http://127.0.0.1:8045"), None);
        assert_eq!(domain_root_of("http://192.168.1.10/admin"), None);
        assert_eq!(domain_root_of("http://localhost:3000"), None);
        assert_eq!(domain_root_of("http://[::1]:8080"), None);
        assert_eq!(domain_root_of(""), None);
        assert_eq!(domain_root_of("   "), None);
        assert_eq!(domain_root_of("http://"), None);
        assert_eq!(domain_root_of("localhost"), None, "没写协议的 localhost 也是本地");
    }

    /// 真实数据里最常见的形状：裸域名（不带 path）。这是中转站的典型配置。
    /// 按域名归组：同域名两个以上才建组，单例域名保持未分组。
    #[test]
    fn group_by_domain_requires_two_providers_per_domain() {
        let mut file = file_of(&[], &[]);
        let urls = [
            ("a".to_string(), "https://integrate.api.nvidia.com/v1".to_string()),
            ("b".to_string(), "https://site2.nvidia.com".to_string()),
            ("c".to_string(), "https://api.deepseek.com/anthropic".to_string()),
        ];
        let groups = file.group_by_domain(&urls);

        assert_eq!(groups.len(), 1, "只有 nvidia.com 够格");
        assert_eq!(groups[0].name, "nvidia.com");
        assert_eq!(groups[0].provider_ids, vec!["a".to_string(), "b".to_string()]);
        assert!(groups[0].is_new, "注册表是空的，这两个组都算新建");
        assert_eq!(file.folders.len(), 1);
        assert_eq!(
            file.assignments.get("a").map(String::as_str),
            Some("nvidia.com")
        );
        assert!(
            !file.assignments.contains_key("c"),
            "域名只出现一次的供应商不该被强行归组"
        );
    }

    /// 本地地址一律不建组。用户的中转站全跑在 127.0.0.1 上，
    /// 拿它们建组只会得到一堆叫"127.0.0.1"的文件夹
    #[test]
    fn group_by_domain_skips_local_addresses() {
        let mut file = file_of(&[], &[]);
        let urls = [
            ("a".to_string(), "http://127.0.0.1:8045".to_string()),
            ("b".to_string(), "http://localhost:4000/v1".to_string()),
        ];
        assert!(file.group_by_domain(&urls).is_empty());
        assert!(file.folders.is_empty(), "没组可建就不该留下空文件夹");
    }

    /// 已经存在的文件夹标 is_new=false，前端据此决定要不要提示"新建了几个"
    #[test]
    fn group_by_domain_marks_existing_folders() {
        let mut file = file_of(&["nvidia.com"], &[]);
        let urls = [
            ("a".to_string(), "https://api.nvidia.com/v1".to_string()),
            ("b".to_string(), "https://other.nvidia.com".to_string()),
        ];
        let groups = file.group_by_domain(&urls);
        assert_eq!(groups.len(), 1);
        assert!(!groups[0].is_new, "注册表里已有这个名字");
        assert_eq!(file.folders.len(), 1, "不该重复登记");
    }

    #[test]
    fn domain_root_of_handles_bare_host() {
        assert_eq!(
            domain_root_of("https://www.jisuanyun01.com").as_deref(),
            Some("jisuanyun01.com")
        );
        assert_eq!(
            domain_root_of("https://www.mcgrox.top").as_deref(),
            Some("mcgrox.top")
        );
    }
}

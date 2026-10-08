//! 供应商的增删改，以及连通性测试。
//!
//! # 为什么非有不可
//!
//! 在此之前面板只能"看"和"切"：列表是导入或手改 `tern.json` 得来的，
//! 想从零加一个中转站必须退出界面去编辑 JSON。ROADMAP T+2 的验收标准就是
//! **不碰 `tern.json` 能从零加一个中转并切过去**——这一整个模块都是为了那条。
//!
//! # 三条不能破的规矩
//!
//! 1. **key 不回传**。前端要能编辑 key 就得看到原值，而把凭据搬进渲染进程
//!    等于把它交给了 webview。所以编辑表单里 key 永远是空输入框，不填 = 不改；
//!    要改 key 必须整段重填。这是有意的取舍：改 key 是多打一遍，key 漏进
//!    webview 是拿不回来。
//! 2. **写前备份**。这里写的是整个 `providers` 数组。备份成 `tern.json.bak`，
//!    手改过的东西不该无声消失。
//! 3. **删正在用的那个要先定好谁来接替**。`default_provider` 指向不存在的 id
//!    会让 `ModelRouter::new` 校验失败、网关起不来——用户看到的是"我删了个
//!    供应商然后全都不能用了"，比不删更糟。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::server::{summary_of, ConfigSummary};

/// 供应商表单。id / name / 地址 / 协议是前端直接填的，key 是**可选的**
/// （不填 = 保留原值，见本模块注释第 1 条）。
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderDraft {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_format: String,
    /// 空串 = 不改这个供应商的 key
    pub api_key: String,
    /// 成本倍率，十进制字符串。空串 = 不用倍率
    #[serde(default)]
    pub cost_multiplier: String,
}

/// 一个供应商的完整信息。字段与 `server::ProviderSummary` 对齐，
/// 多个 `is_default` 和 `cost_multiplier`——编辑表单要拿它们回填。
#[derive(Debug, Serialize, Clone)]
pub struct ProviderDetail {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_format: String,
    pub auth_kind: String,
    pub key_state: String,
    pub web_tools_at_risk: bool,
    pub is_default: bool,
    pub cost_multiplier: Option<String>,
}

/// 连通性测试的结果。
#[derive(Debug, Serialize, Clone)]
pub struct ProbeReport {
    /// 上游**通到了**。429 / 5xx 也算通：那说明地址和 key 都对，是上游在限流，
    /// 和"地址填错"（404）或"key 不对"（401）的修法完全不同
    pub reachable: bool,
    /// 拿到了什么状态码。连接失败时为空
    pub http_status: Option<u16>,
    /// 给人看的一句话。失败时尽量带上上游的原话——模型名不对、key 失效、
    /// 超额各自的修法都不一样，我们自己总结的常常帮不上忙
    pub message: String,
    /// 从上游读到的模型数。0 表示这个供应商不提供 `/models`
    pub models: usize,
    /// 实际打到的 URL。失败时用户要能看见"它试的是哪个地址"
    pub url: String,
    /// 耗时毫秒
    pub elapsed_ms: u128,
}

// ---------------------------------------------------------------------------
// 校验
// ---------------------------------------------------------------------------

/// 校验 id / 地址，返回统一的错误文字。
///
/// id 同时是 `provider/model` 路由的前缀，所以不能带 `/` 和空白：
/// `deepseek/v3` 会被路由层理解成"deepseek 这家下的 v3 模型"，
/// 而不是一个叫 `deepseek/v3` 的供应商。
///
/// 地址为空只对**编辑订阅登录**时放行：Copilot / Codex / xAI 的端点由
/// `effective_base_url()` 托管，填了也不生效（那是 cc-switch 的安全不变量，
/// 防止订阅 token 被发到误填的地址）。`existing_provider` 非空且是订阅时，
/// 空地址应当原样保留；新建一律要填。
pub fn validate_draft_only(draft: &ProviderDraft) -> std::result::Result<(), String> {
    let id = draft.id.trim();
    if id.is_empty() {
        return Err("id 不能为空".into());
    }
    if id.chars().any(char::is_whitespace) {
        return Err(format!("id 不能含空白：现在是 {id:?}"));
    }
    if id.contains('/') {
        return Err(format!("id 不能含 /：{id:?} 会被当成路由前缀"));
    }
    Ok(())
}

/// 地址的校验。分开而不是塞进 `validate_draft_only`：托管订阅的地址不可编辑，
/// 合在一起会让"能不能空"取决于一个不在这里的类型判断。
pub fn validate_url(draft: &ProviderDraft, allow_empty: bool) -> std::result::Result<(), String> {
    let url = draft.base_url.trim();
    if url.is_empty() {
        if allow_empty {
            return Ok(());
        }
        return Err("地址不能为空".into());
    }
    if !url.contains("://") {
        return Err(format!("地址要带协议，比如 https://{url}"));
    }
    Ok(())
}

pub fn validate_draft(draft: &ProviderDraft) -> std::result::Result<(), String> {
    validate_draft_only(draft)?;
    validate_url(draft, false)
}

/// 协议名 → `ApiFormat`。前端给的是 `api_format` 字符串
/// （和 cc-switch 的取值一致），解析不了就不认。
pub fn parse_api_format(text: &str) -> Option<tern_gateway::ApiFormat> {
    match text.trim() {
        "anthropic" => Some(tern_gateway::ApiFormat::Anthropic),
        "openai_chat" => Some(tern_gateway::ApiFormat::OpenaiChat),
        "openai_responses" => Some(tern_gateway::ApiFormat::OpenaiResponses),
        "gemini_native" => Some(tern_gateway::ApiFormat::GeminiNative),
        _ => None,
    }
}

/// 成本倍率要能解析成 Decimal 且为正，否则配置能存、计价时炸。
fn parse_multiplier(text: &str) -> std::result::Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<rust_decimal::Decimal>() {
        Ok(value) if value > rust_decimal::Decimal::ZERO => Ok(Some(value.to_string())),
        Ok(_) => Err("成本倍率要大于 0".into()),
        Err(error) => Err(format!("成本倍率不是数字：{error}")),
    }
}

// ---------------------------------------------------------------------------
// 增删改
// ---------------------------------------------------------------------------

pub fn detail_of(
    config: &tern_gateway::GatewayConfig,
    spec: &tern_gateway::ProviderSpec,
) -> ProviderDetail {
    ProviderDetail {
        id: spec.id.clone(),
        name: spec.name.clone(),
        // 给可编辑字段而不是 effective_ 值：订阅登录的地址被托管（写进去也不生效），
        // 拿托管值回填会让用户改一个不起作用的框
        base_url: spec.base_url.clone(),
        api_format: spec.effective_api_format().to_string(),
        auth_kind: auth_kind(&spec.auth).to_string(),
        key_state: key_state(&spec.auth).to_string(),
        web_tools_at_risk: matches!(
            tern_gateway::assess(spec),
            tern_gateway::WebToolsSupport::ThirdParty
        ),
        is_default: config.default_provider.as_deref() == Some(spec.id.as_str()),
        cost_multiplier: spec.cost_multiplier.clone(),
    }
}

/// 表单草稿 → `ProviderSpec`。
///
/// `existing` 非空时是编辑：key 空表示保留，非空表示整段替换。
/// 其余字段一律以表单为准——用户把显示名清空就是要清空。
pub fn spec_of(
    draft: &ProviderDraft,
    existing: Option<&tern_gateway::ProviderSpec>,
) -> std::result::Result<tern_gateway::ProviderSpec, String> {
    validate_draft_only(draft)?;
    // 托管订阅（Copilot / Codex / xAI）的端点由 effective_base_url() 说了算，
    // 表单里那个框是空且不可编辑的。别让它把"编辑订阅"变成不可能
    let managed = existing.is_some_and(|old| old.has_pinned_endpoint() || old.is_github_copilot());
    validate_url(draft, managed)?;
    let format = parse_api_format(&draft.api_format)
        .ok_or_else(|| format!("不认识的协议：{}", draft.api_format))?;

    let mut spec = match existing {
        // 编辑：从旧的出发，保住订阅认证和那些表单里没有的字段
        // （reasoning / prompt_cache_key 之类）。整段新建会静默丢掉它们
        Some(old) => old.clone(),
        None => tern_gateway::ProviderSpec::new(
            draft.id.trim().to_string(),
            draft.name.trim().to_string(),
            draft.base_url.trim().to_string(),
            format,
            tern_gateway::ProviderAuth::api_key(draft.api_key.trim().to_string()),
        ),
    };

    spec.id = draft.id.trim().to_string();
    spec.name = draft.name.trim().to_string();
    spec.base_url = draft.base_url.trim().to_string();
    spec.api_format = format;
    // 只在填了的时候换 key。空 input 表示"这次不动它"，见模块注释第 1 条
    if !draft.api_key.trim().is_empty() {
        if let tern_gateway::ProviderAuth::ApiKey { header, .. } = &spec.auth {
            let header = *header;
            spec.auth = tern_gateway::ProviderAuth::ApiKey {
                key: draft.api_key.trim().to_string(),
                header,
            };
        }
        // 订阅登录的 key 不在 tern 手里，表单里那个框填什么都不该生效
    }
    spec.cost_multiplier = parse_multiplier(&draft.cost_multiplier)?;
    Ok(spec)
}

fn auth_kind(auth: &tern_gateway::ProviderAuth) -> &'static str {
    match auth {
        tern_gateway::ProviderAuth::None => "none",
        tern_gateway::ProviderAuth::ApiKey { .. } => "api_key",
        tern_gateway::ProviderAuth::GoogleOauth { .. } => "google_oauth",
        tern_gateway::ProviderAuth::GithubCopilot { .. } => "github_copilot",
        tern_gateway::ProviderAuth::CodexOauth { .. } => "codex_oauth",
        tern_gateway::ProviderAuth::XaiOauth { .. } => "xai_oauth",
    }
}

fn key_state(auth: &tern_gateway::ProviderAuth) -> &'static str {
    match auth {
        tern_gateway::ProviderAuth::ApiKey { key, .. } => {
            let key = key.trim();
            if key.is_empty() {
                "empty"
            } else if key == crate::config::PLACEHOLDER_KEY {
                "placeholder"
            } else {
                "real"
            }
        }
        _ => "subscription",
    }
}

/// 把草稿写进配置。
///
/// 返回新的 [`ConfigSummary`]——前端改了列表要跟着刷，返回它省一次往返。
pub fn apply_draft(
    path: &std::path::Path,
    draft: &ProviderDraft,
    is_edit: bool,
) -> std::result::Result<ConfigSummary, String> {
    let mut config = crate::config::load(path).map_err(|e| e.to_string())?;
    let id = draft.id.trim();

    let existing: Option<tern_gateway::ProviderSpec> = if is_edit {
        Some(
            config
                .providers
                .iter()
                .find(|spec| spec.id == id)
                .cloned()
                .ok_or_else(|| format!("供应商 {id} 不在配置里"))?,
        )
    } else {
        // 新建撞 id 要拒：路由表用 id 当前缀，重复了 `provider/model` 指不定谁
        if config.providers.iter().any(|spec| spec.id == id) {
            return Err(format!("已经有 id 为 {id} 的供应商了"));
        }
        // 新建必须带 key。占位符也不行：那是 cc-switch 导入流程的哨兵值，
        // 存进去 key_state 会显示"占位符"，而用户明明刚填过
        let key = draft.api_key.trim();
        if key.is_empty() {
            return Err("API Key 不能为空".into());
        }
        if key == crate::config::PLACEHOLDER_KEY {
            return Err("填的是 cc-switch 的占位符，不是真 key".into());
        }
        None
    };

    let spec = spec_of(draft, existing.as_ref())?;
    if is_edit {
        let at = config
            .providers
            .iter()
            .position(|old| old.id == spec.id)
            // 上面刚 find 到过，换 id 的情况不会走到这里
            .unwrap_or(config.providers.len());
        if at < config.providers.len() {
            config.providers[at] = spec;
        } else {
            config.providers.push(spec);
        }
    } else {
        // 新供应商排到最后：用户接下来要点它、拉模型、切过去，
        // 插到列表最前面反而打断他刚看完的位置
        config.providers.push(spec);
    }

    write_config_at(path, &config)?;
    Ok(summary_of(path, config))
}

/// 删一个供应商。删的是 default 时按 `fallback_id` 换人，没得换就清空。
///
/// 单独一个函数而不是让前端先 `select_provider` 再删：两步之间断了
/// （切到 B 成功、删 A 失败）会留下"我以为换过了"的状态。
pub fn remove_provider(
    path: &std::path::Path,
    id: &str,
    fallback_id: Option<String>,
) -> std::result::Result<ConfigSummary, String> {
    let mut config = crate::config::load(path).map_err(|e| e.to_string())?;
    if !config.providers.iter().any(|spec| spec.id == id) {
        return Err(format!("供应商 {id} 不在配置里"));
    }

    let is_default = config.default_provider.as_deref() == Some(id);
    if is_default {
        // 换到 fallback 时它必须真的存在：一个指向空气的 default 会让网关起不来
        let next = fallback_id
            .filter(|next| next != id && config.providers.iter().any(|spec| &spec.id == next));
        config.default_provider = next;
        // 没有可换的，或没指定：清空。空 default 让不带前缀的模型名找不到路，
        // 但网关照样起得来——比留个悬空引用好
    }

    config.providers.retain(|spec| spec.id != id);
    write_config_at(path, &config)?;
    Ok(summary_of(path, config))
}

/// 走 `server::write_config` 而不是自己写一遍：两边写的是同一个文件，
/// 备份规则（`.bak` + pretty JSON）分叉会让有时新有时旧的备份混在一起。
fn write_config_at(
    path: &std::path::Path,
    config: &tern_gateway::GatewayConfig,
) -> std::result::Result<(), String> {
    crate::server::write_config(path, config).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 连通性测试
// ---------------------------------------------------------------------------

/// 探测用的模型名。
///
/// 随便挑一个"这个上游多半认识"的名字是做不到的——中转站的模型名千奇百怪。
/// 这里用 Claude 的常见名：它只影响错误信息里那句"model not found"的措辞，
/// 而连接失败、401、404 这些结论与模型名无关。
const PROBE_MODEL: &str = "claude-sonnet-4-6";

/// 连通性测试：向这个上游发一条最小的请求，看它怎么回。
///
/// # 为什么复用网关的协议转换而不是直接打 `/v1/models`
///
/// 中转站的"通"分好几个意思：地址对、key 对、还要**协议对**。
/// 一个只认 Chat 的上游，问 `/v1/models` 也许是 200，但你按 Anthropic 发消息
/// 会 404——那种供应商导入进来照样用不了。所以这里走
/// `adapter::prepare_request`，和真实流量同一条转换路径。
///
/// 顺手拉一份模型列表：它通不过上游就当 0 个模型，不影响连通性结论。
/// 很多中转站根本不开 `/models`，为它判"不通"是假阴性。
///
/// **阻塞**。调用方（tauri 命令）必须把它丢进 `spawn_blocking`，
/// 否则一个 20 秒的超时会把窗口操作一起拖住。
pub fn probe(spec: &tern_gateway::ProviderSpec) -> ProbeReport {
    let started = std::time::Instant::now();
    let base_url = spec.effective_base_url();
    let fail = |message: String| ProbeReport {
        reachable: false,
        http_status: None,
        message,
        models: 0,
        url: base_url.clone(),
        elapsed_ms: started.elapsed().as_millis(),
    };

    // 订阅登录的 token 在网关手里，这里拿不到：硬打只会得到 401，
    // 那和"配错了"长得一样。说清楚比给个假红灯好
    if tern_gateway::adapter::requires_managed_token(&spec.auth) {
        return ProbeReport {
            message: "这个供应商用订阅登录，token 由网关动态换取，这里测不了。\
                      启动网关后发一条真实请求即可验证。"
                .into(),
            ..fail(String::new())
        };
    }

    // 一律按 Claude Code 的口径发：这是终端用户的实际客户端，
    // 协议的错配（上游只认 Chat 却被当成 Anthropic 用）应当在这里暴露出来，
    // 而不是等用户真去用时才发现
    let body = json!({
        "model": PROBE_MODEL,
        "max_tokens": 1,
        "stream": false,
        "messages": [{ "role": "user", "content": "ping" }]
    });

    let prepared = match tern_gateway::adapter::prepare_request(
        spec,
        tern_gateway::ApiFormat::Anthropic,
        "/v1/messages",
        body,
        &tern_gateway::adapter::RequestContext::default(),
    ) {
        Ok(prepared) => prepared,
        Err(error) => return fail(format!("拼不出上游地址：{error}")),
    };
    let url = prepared.url.clone();

    let client = match reqwest::blocking::Client::builder()
        // 比正常转发短得多：用户点一下就盯着，转 120 秒只会让人觉得它死了
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(client) => client,
        Err(error) => return fail(format!("创建 HTTP 客户端失败：{error}")),
    };

    let mut request = client.post(&url).json(&prepared.body);
    if let Some(auth) = tern_gateway::adapter::resolve_auth(spec) {
        // 认证头按上游协议选，和网关同一套规则
        if let Ok(headers) = tern_gateway::adapter::auth_headers(&auth) {
            for (name, value) in headers {
                request = request.header(name, value);
            }
        }
    }
    if prepared.upstream_format == tern_gateway::ApiFormat::Anthropic {
        // 漏了它上游直接 400，而那个错误看起来像"配错了"
        request = request.header("anthropic-version", "2023-06-01");
    }

    // 模型列表顺手拉一份，失败不影响连通性结论
    let models = model_count(spec);

    let response = match request.send() {
        Ok(response) => response,
        Err(error) => {
            return ProbeReport {
                models,
                ..fail(format!("连不上：{error}"))
            };
        }
    };

    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();
    let detail = upstream_message(&body).unwrap_or_else(|| truncate(&body, 200));

    // 2xx 是通；4xx/5xx 也当"地址和 key 对上了"，因为限流、过载、模型名不对
    // 全在这一段里——那些是上游在回话，不是配错了。真正的"不通"是连不上，
    // 上面已经返回了。
    let (reachable, message) = if (200..300).contains(&status) {
        (
            true,
            "通了。这条请求按真实流量的路径转换后发出去，上游正常应答。".into(),
        )
    } else {
        (true, format!("上游有应答（HTTP {status}）：{detail}"))
    };

    ProbeReport {
        reachable,
        http_status: Some(status),
        message,
        models,
        url,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

/// 上游的模型数。没有 `/models` 或不给读就是 0，不当失败。
fn model_count(spec: &tern_gateway::ProviderSpec) -> usize {
    // 只对静态 key 的供应商问：订阅登录的 token 在网关手里，这里拿不到
    let tern_gateway::ProviderAuth::ApiKey { key, .. } = &spec.auth else {
        return 0;
    };
    let key = key.trim();
    if key.is_empty() || key == crate::config::PLACEHOLDER_KEY {
        return 0;
    }
    tern_gateway::models::fetch_models(&spec.effective_base_url(), key, spec.full_url, None)
        .map(|models| models.len())
        .unwrap_or(0)
}

/// 从 Anthropic / OpenAI 两种风格的错误体里挖 message。挖不到就退回原文截断。
fn upstream_message(body: &str) -> Option<String> {
    let json: Value = serde_json::from_str(body).ok()?;
    let message = json
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| json.pointer("/message").and_then(Value::as_str))?;
    Some(truncate(message, 200))
}

fn truncate(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}

// ---------------------------------------------------------------------------
// tauri 命令
// ---------------------------------------------------------------------------

/// 建或改一个供应商。
///
/// 前端传 `id` 而不传 `is_edit`：这个模块按"配置里有没有这个 id"自己判断。
/// 让前端说"我在编辑"会在两边状态不一致时（比如配置刚被导入覆盖）写错——
/// 而 Rust 侧看到的配置才是唯一的事实源。
#[tauri::command]
pub fn provider_save(draft: serde_json::Value) -> Result<ConfigSummary> {
    let draft: ProviderDraft = serde_json::from_value(draft)?;
    let is_edit = crate::config::load(&crate::config::config_path()?)
        .map(|config| {
            config
                .providers
                .iter()
                .any(|spec| spec.id == draft.id.trim())
        })
        .unwrap_or(false);
    let summary = crate::providers::apply_draft(&crate::config::config_path()?, &draft, is_edit)
        .map_err(crate::error::AppError::Config)?;

    // 网关还拿着旧配置在跑。不重起的话面板里是新的、实际路由用旧的，
    // 用户会以为加成功了，然后请求全部失败。
    // 失败不阻断增删改本身——文件已经落盘，那才是要紧的
    if let Err(error) = crate::server::restart_after_config_change() {
        log::warn!("[providers] 增删改后重起网关失败: {error}");
    }
    Ok(summary)
}

/// 删一个供应商。`fallback_id` 是"删的是 default 时换成谁"，
/// 前端通常传当前列表里的下一个。
#[tauri::command]
pub fn provider_remove(id: String, fallback_id: Option<String>) -> Result<ConfigSummary> {
    let summary =
        crate::providers::remove_provider(&crate::config::config_path()?, &id, fallback_id)
            .map_err(crate::error::AppError::Config)?;
    if let Err(error) = crate::server::restart_after_config_change() {
        log::warn!("[providers] 删除后重起网关失败: {error}");
    }
    Ok(summary)
}

/// 一个供应商的完整信息，供编辑表单回填。
///
/// **不含 key**：凭据不进渲染进程。表单里的 key 框永远是空的，不填 = 不改。
#[tauri::command]
pub fn provider_detail(id: String) -> Result<ProviderDetail> {
    let config = crate::config::load(&crate::config::config_path()?)?;
    let spec = config
        .providers
        .iter()
        .find(|spec| spec.id == id)
        .ok_or_else(|| crate::error::AppError::Config(format!("供应商 {id} 不在配置里")))?;
    Ok(crate::providers::detail_of(&config, spec))
}

/// 连通性测试。发一条按真实流量路径转换后的最小请求，看上游怎么回。
///
/// `async` + `spawn_blocking`：探测是阻塞的（最长 20 秒），
/// 直接跑会把窗口操作一起拖住。
#[tauri::command]
pub async fn provider_probe(id: String) -> Result<ProbeReport> {
    let spec = {
        let config = crate::config::load(&crate::config::config_path()?)?;
        config
            .providers
            .iter()
            .find(|spec| spec.id == id)
            .cloned()
            .ok_or_else(|| crate::error::AppError::Config(format!("供应商 {id} 不在配置里")))?
    };
    tauri::async_runtime::spawn_blocking(move || crate::providers::probe(&spec))
        .await
        .map_err(|e| crate::error::AppError::Config(format!("探测任务失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(id: &str, url: &str, key: &str) -> ProviderDraft {
        ProviderDraft {
            id: id.into(),
            name: id.into(),
            base_url: url.into(),
            api_format: "anthropic".into(),
            api_key: key.into(),
            cost_multiplier: String::new(),
        }
    }

    /// id 是路由前缀，带 / 会让 `provider/model` 指不定解析成谁。
    /// 这个必须在写入前拦住——写进去网关起得来，但路由是坏的
    #[test]
    fn an_id_with_a_slash_is_refused() {
        let draft = draft("deepseek/v3", "https://api.example.com", "sk-real");
        let error = validate_draft(&draft).unwrap_err();
        assert!(error.contains("路由前缀"), "{error}");
    }

    #[test]
    fn a_blank_id_or_url_is_refused() {
        assert!(validate_draft(&draft("  ", "https://x.com", "k")).is_err());
        assert!(validate_draft(&draft("a", "   ", "k")).is_err());
    }

    /// 没协议的头是用户最常犯的错（粘了 `api.deepseek.com` 就存）。
    /// 错误里要把补全后的样子给他，别只说一句"格式不对"
    #[test]
    fn a_url_without_a_scheme_is_refused_with_a_hint() {
        let error = validate_draft(&draft("a", "api.deepseek.com", "k")).unwrap_err();
        assert!(error.contains("https://api.deepseek.com"), "{error}");
    }

    #[test]
    fn protocol_names_map_to_formats() {
        assert_eq!(
            parse_api_format("anthropic"),
            Some(tern_gateway::ApiFormat::Anthropic)
        );
        assert_eq!(
            parse_api_format("openai_chat"),
            Some(tern_gateway::ApiFormat::OpenaiChat)
        );
        assert!(parse_api_format("nonsense").is_none());
    }

    /// 编辑时 key 留空 = 不动。填了才换。
    /// 反过来的话，每次改个显示名都会被静默清空 key
    #[test]
    fn editing_keeps_the_key_when_the_field_is_blank() {
        let existing = tern_gateway::ProviderSpec::new(
            "a",
            "A",
            "https://a.example.com/anthropic",
            tern_gateway::ApiFormat::Anthropic,
            tern_gateway::ProviderAuth::api_key("sk-original"),
        );
        let mut next = draft("a", "https://a.example.com/anthropic", "");
        next.name = "改过的名字".into();
        let spec = spec_of(&next, Some(&existing)).unwrap();
        assert_eq!(spec.name, "改过的名字");
        assert_eq!(
            spec.auth,
            tern_gateway::ProviderAuth::api_key("sk-original"),
            "key 没填就该保住"
        );

        // 填了就整段换
        let edited = spec_of(
            &draft("a", "https://a.example.com/anthropic", "sk-new"),
            Some(&existing),
        )
        .unwrap();
        assert_eq!(
            spec.auth,
            tern_gateway::ProviderAuth::api_key("sk-original")
        );
        assert_eq!(edited.auth, tern_gateway::ProviderAuth::api_key("sk-new"));
    }

    /// 订阅登录的 key 不在 tern 手里。表单里那个框填了也不该生效——
    /// 否则会把 Copilot 变成一个拿着假 bearer 的 api_key 供应商
    #[test]
    fn a_subscription_provider_never_takes_the_key_field() {
        let existing = tern_gateway::ProviderSpec::new(
            "cp",
            "Copilot",
            "",
            tern_gateway::ApiFormat::OpenaiChat,
            tern_gateway::ProviderAuth::GithubCopilot { account_id: None },
        );
        let mut next = draft("cp", "", "sk-whatever");
        next.api_format = "openai_chat".into();
        let spec = spec_of(&next, Some(&existing)).unwrap();
        assert!(
            matches!(spec.auth, tern_gateway::ProviderAuth::GithubCopilot { .. }),
            "{:?}",
            spec.auth
        );
    }

    /// 倍率给了乱七八糟的值必须拒。配置能存、计价时才炸的组合最难查
    #[test]
    fn a_nonsense_cost_multiplier_is_refused() {
        for bad in ["abc", "0", "-1"] {
            let mut next = draft("a", "https://x.com/anthropic", "sk-real");
            next.cost_multiplier = bad.into();
            assert!(spec_of(&next, None).is_err(), "{bad} 应当被拒");
        }
        let mut ok = draft("a", "https://x.com/anthropic", "sk-real");
        ok.cost_multiplier = "0.3".into();
        assert_eq!(
            spec_of(&ok, None).unwrap().cost_multiplier.as_deref(),
            Some("0.3")
        );
    }

    /// 新建撞 id 要拒：路由表用 id 当前缀，重复了 `provider/model` 指不定谁
    #[test]
    fn a_duplicate_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tern.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "listen": "127.0.0.1:15800",
                "providers": [{
                    "id": "a", "name": "A", "baseUrl": "https://a.example.com/anthropic",
                    "auth": { "type": "api_key", "key": "sk-real" }
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = apply_draft(
            &path,
            &draft("a", "https://b.example.com/anthropic", "sk-new"),
            false,
        )
        .unwrap_err();
        assert!(error.contains("已经有"), "{error}");
    }

    /// 新建必须带 key。空 key 存进去请求全失败，而用户以为自己配好了
    #[test]
    fn a_new_provider_needs_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tern.json");
        std::fs::write(&path, r#"{"listen":"127.0.0.1:15800","providers":[]}"#).unwrap();
        assert!(apply_draft(&path, &draft("a", "https://x.com/anthropic", "  "), false).is_err());
    }

    /// 删正在用的那个必须定好谁来接替。留着悬空的 default_provider
    /// 会让 ModelRouter::new 校验失败、网关起不来——
    /// 用户看到的是"删了个供应商，然后全都不能用了"
    #[test]
    fn deleting_the_default_provider_falls_back_or_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tern.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "listen": "127.0.0.1:15800",
                "defaultProvider": "a",
                "providers": [
                    { "id": "a", "name": "A", "baseUrl": "https://a.example.com/anthropic",
                      "auth": { "type": "api_key", "key": "sk-real" } },
                    { "id": "b", "name": "B", "baseUrl": "https://b.example.com/anthropic",
                      "auth": { "type": "api_key", "key": "sk-real" } }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let summary = remove_provider(&path, "a", Some("b".into())).unwrap();
        assert_eq!(summary.default_provider.as_deref(), Some("b"));
        assert_eq!(summary.providers.len(), 1);

        // 没得换就清空，而不是留着指向空气
        let empty = remove_provider(&path, "b", None).unwrap();
        assert_eq!(empty.default_provider, None);
        assert!(empty.providers.is_empty());
    }

    /// fallback 指向不存在的 id 要 ignored，不能当成"换好了"
    #[test]
    fn an_unknown_fallback_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tern.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "listen": "127.0.0.1:15800",
                "defaultProvider": "a",
                "providers": [{
                    "id": "a", "name": "A", "baseUrl": "https://a.example.com/anthropic",
                    "auth": { "type": "api_key", "key": "sk-real" }
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let summary = remove_provider(&path, "a", Some("ghost".into())).unwrap();
        assert_eq!(summary.default_provider, None, "悬空引用会让网关起不来");
    }

    /// 删不存在的供应商要报错，不能静默成功（前端会以为删掉了）
    #[test]
    fn deleting_an_unknown_provider_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tern.json");
        std::fs::write(&path, r#"{"listen":"127.0.0.1:15800","providers":[]}"#).unwrap();
        assert!(remove_provider(&path, "ghost", None).is_err());
    }

    /// 写前必须备份。这里整段重写 providers 数组，用户手改过的东西
    /// 不该无声消失
    #[test]
    fn writing_backs_up_the_old_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tern.json");
        let original = r#"{"listen":"127.0.0.1:15800","providers":[]}"#;
        std::fs::write(&path, original).unwrap();

        apply_draft(
            &path,
            &draft("a", "https://x.com/anthropic", "sk-real"),
            false,
        )
        .unwrap();
        let backup = path.with_extension("json.bak");
        assert!(backup.exists(), "写前必须备份");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
    }

    /// 错误体里挖 message：两种协议风格都要认。挖不到返回 None，
    /// 由调用方退回原文
    #[test]
    fn upstream_messages_are_extracted_from_both_styles() {
        assert_eq!(
            upstream_message(r#"{"error":{"message":"invalid api key"}}"#).as_deref(),
            Some("invalid api key")
        );
        assert_eq!(
            upstream_message(r#"{"message":"rate limited"}"#).as_deref(),
            Some("rate limited")
        );
        assert_eq!(upstream_message("<html>404</html>"), None);
    }
}

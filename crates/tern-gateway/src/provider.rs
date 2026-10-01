//! 供应商相关的共享类型。
//!
//! `CodexChatReasoningConfig` 原样取自 cc-switch `provider.rs`；
//! `ProviderSpec` 是 tern 自己的中立供应商定义，替代 cc-switch 里
//! "按 agent 存一份配置快照（settings_config）"的做法。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::proxy::providers::{CHATGPT_CODEX_BASE_URL, XAI_API_BASE_URL};

const GITHUB_COPILOT_BASE_URL: &str = "https://api.githubcopilot.com";

/// Codex Responses -> Chat Completions 的 reasoning 能力描述。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CodexChatReasoningConfig {
    #[serde(rename = "supportsThinking", skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,
    #[serde(rename = "supportsEffort", skip_serializing_if = "Option::is_none")]
    pub supports_effort: Option<bool>,
    #[serde(rename = "thinkingParam", skip_serializing_if = "Option::is_none")]
    pub thinking_param: Option<String>,
    #[serde(rename = "effortParam", skip_serializing_if = "Option::is_none")]
    pub effort_param: Option<String>,
    #[serde(rename = "effortValueMode", skip_serializing_if = "Option::is_none")]
    pub effort_value_mode: Option<String>,
    /// 声明性字段：标注上游 reasoning 的回传位置（reasoning_content / reasoning /
    /// reasoning_details / think_tags）。当前响应侧 `extract_reasoning_field_text`
    /// 靠穷举字段提取、并不读取本字段；保留作文档说明与未来按格式分发（如 think_tags）的预留。
    #[serde(rename = "outputFormat", skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
}

/// 协议格式：既描述上游供应商说什么，也描述客户端（agent）说什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    /// Anthropic Messages（`/v1/messages`）
    #[default]
    Anthropic,
    /// OpenAI Chat Completions（`/chat/completions`）
    OpenaiChat,
    /// OpenAI Responses（`/responses`）
    OpenaiResponses,
    /// Google Gemini Native（`models/*:generateContent`）
    GeminiNative,
}

impl ApiFormat {
    /// 与 cc-switch 的 `api_format` 字符串一致，方便对照上游代码与日志
    pub fn as_str(self) -> &'static str {
        match self {
            ApiFormat::Anthropic => "anthropic",
            ApiFormat::OpenaiChat => "openai_chat",
            ApiFormat::OpenaiResponses => "openai_responses",
            ApiFormat::GeminiNative => "gemini_native",
        }
    }
}

impl fmt::Display for ApiFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 静态 key 放在哪个请求头里
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyHeader {
    /// 按上游协议选：Anthropic → `x-api-key`，Gemini → `x-goog-api-key`，其余 → Bearer
    #[default]
    Auto,
    /// `Authorization: Bearer <key>`（多数中转站的 Anthropic 端点也接受）
    Bearer,
    /// `x-api-key: <key>`
    XApiKey,
    /// `x-goog-api-key: <key>`
    XGoogApiKey,
}

/// 供应商的认证方式
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderAuth {
    /// 本地服务（Ollama、LM Studio 等）不需要认证
    None,
    /// 静态 API Key
    ApiKey {
        key: String,
        #[serde(default)]
        header: KeyHeader,
    },
    /// Gemini CLI OAuth：`ya29.` access token，或 `oauth_creds.json` 的内容
    GoogleOauth { credentials: String },
    /// GitHub Copilot 订阅，token 由网关动态换取
    GithubCopilot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
    /// ChatGPT（Codex）订阅，token 由网关动态换取
    CodexOauth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
    /// xAI Grok 订阅，token 由网关动态换取
    XaiOauth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
}

impl ProviderAuth {
    /// 最常见的情况：一个 key，请求头按上游协议自动选
    pub fn api_key(key: impl Into<String>) -> Self {
        ProviderAuth::ApiKey {
            key: key.into(),
            header: KeyHeader::Auto,
        }
    }
}

/// 手写 Debug：ProviderSpec 会出现在日志里，key / 凭证必须遮蔽
impl fmt::Debug for ProviderAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderAuth::None => f.write_str("None"),
            ProviderAuth::ApiKey { key, header } => f
                .debug_struct("ApiKey")
                .field("key", &mask_secret(key))
                .field("header", header)
                .finish(),
            ProviderAuth::GoogleOauth { .. } => f
                .debug_struct("GoogleOauth")
                .field("credentials", &"<redacted>")
                .finish(),
            ProviderAuth::GithubCopilot { account_id } => f
                .debug_struct("GithubCopilot")
                .field("account_id", account_id)
                .finish(),
            ProviderAuth::CodexOauth { account_id } => f
                .debug_struct("CodexOauth")
                .field("account_id", account_id)
                .finish(),
            ProviderAuth::XaiOauth { account_id } => f
                .debug_struct("XaiOauth")
                .field("account_id", account_id)
                .finish(),
        }
    }
}

/// 前 4 后 4，其余用 `...` 代替；不足 8 位整体遮蔽（与 cc-switch `AuthInfo::masked_key` 一致）
fn mask_secret(secret: &str) -> String {
    let count = secret.chars().count();
    if count > 8 {
        let prefix: String = secret.chars().take(4).collect();
        let suffix: String = secret.chars().skip(count - 4).collect();
        format!("{prefix}...{suffix}")
    } else {
        "***".to_string()
    }
}

/// Responses → Chat 转换后是否附带 `prompt_cache_key`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheRouting {
    /// 只对已知支持的上游发送（api.openai.com、Kimi Coding）；
    /// 很多 OpenAI 兼容网关遇到不认识的字段会直接 400
    #[default]
    Auto,
    Enabled,
    Disabled,
}

/// 中立的供应商定义
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSpec {
    /// `provider/model` 里的前缀，如 `deepseek`
    pub id: String,
    /// 展示名；同时参与 reasoning 参数的平台推断（沿用 cc-switch 行为）
    pub name: String,
    /// 上游地址。OpenAI 系按 SDK 约定可带版本（`.../v1`），Anthropic 系不带
    pub base_url: String,
    /// 上游实际说的协议
    #[serde(default)]
    pub api_format: ApiFormat,
    pub auth: ProviderAuth,
    /// `base_url` 已经是完整端点，不再拼接路径
    #[serde(default)]
    pub full_url: bool,
    /// 显式指定 Chat 上游的 reasoning 参数，覆盖按名称 / 地址 / 模型的推断
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<CodexChatReasoningConfig>,
    /// 固定的 prompt_cache_key，优先于按会话生成的
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(default)]
    pub prompt_cache_routing: PromptCacheRouting,
    /// Responses → Anthropic 时强制的输出上限（Codex 不会把 model_max_output_tokens 发过来）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Codex OAuth 的 FAST 模式（`service_tier = "priority"`）
    #[serde(default)]
    pub codex_fast_mode: bool,
}

impl ProviderSpec {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_format: ApiFormat,
        auth: ProviderAuth,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            base_url: base_url.into(),
            api_format,
            auth,
            full_url: false,
            reasoning: None,
            prompt_cache_key: None,
            prompt_cache_routing: PromptCacheRouting::Auto,
            max_output_tokens: None,
            codex_fast_mode: false,
        }
    }

    pub fn is_codex_oauth(&self) -> bool {
        matches!(self.auth, ProviderAuth::CodexOauth { .. })
    }

    pub fn is_xai_oauth(&self) -> bool {
        matches!(self.auth, ProviderAuth::XaiOauth { .. })
    }

    /// cc-switch 还会按 base_url 含 `githubcopilot.com` 兜底判断（兼容旧数据）；
    /// tern 的认证方式是显式字段，只看 `auth`
    pub fn is_github_copilot(&self) -> bool {
        matches!(self.auth, ProviderAuth::GithubCopilot { .. })
    }

    /// Codex / xAI 订阅的端点是固定的
    pub fn has_pinned_endpoint(&self) -> bool {
        self.is_codex_oauth() || self.is_xai_oauth()
    }

    /// 实际使用的上游地址。
    ///
    /// 托管订阅忽略可编辑的 `base_url`：这是 cc-switch 的安全不变量，防止订阅
    /// token 被发到导入配置或误填的任意地址。
    pub fn effective_base_url(&self) -> String {
        if self.is_codex_oauth() {
            return CHATGPT_CODEX_BASE_URL.to_string();
        }
        if self.is_xai_oauth() {
            return XAI_API_BASE_URL.to_string();
        }
        let trimmed = self.base_url.trim().trim_end_matches('/');
        if trimmed.is_empty() && self.is_github_copilot() {
            return GITHUB_COPILOT_BASE_URL.to_string();
        }
        trimmed.to_string()
    }

    /// 实际使用的上游协议。
    ///
    /// - Codex / xAI 订阅只有 Responses 端点，可编辑的 `api_format` 不生效
    /// - Copilot 没有 Anthropic 端点：默认 Chat，显式选 Responses 时走 Responses
    ///   （cc-switch 会按模型厂商动态决定，这一步留给网关转发层）
    pub fn effective_api_format(&self) -> ApiFormat {
        if self.has_pinned_endpoint() {
            return ApiFormat::OpenaiResponses;
        }
        if self.is_github_copilot() {
            return match self.api_format {
                ApiFormat::OpenaiResponses => ApiFormat::OpenaiResponses,
                _ => ApiFormat::OpenaiChat,
            };
        }
        self.api_format
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_masks_api_key() {
        let auth = ProviderAuth::api_key("sk-secret-0123456789");
        let printed = format!("{auth:?}");
        assert!(!printed.contains("sk-secret-0123456789"), "{printed}");
        assert!(printed.contains("sk-s...6789"), "{printed}");

        let short = format!("{:?}", ProviderAuth::api_key("short"));
        assert!(!short.contains("short"), "{short}");
    }

    #[test]
    fn debug_output_hides_google_credentials() {
        let auth = ProviderAuth::GoogleOauth {
            credentials: r#"{"access_token":"ya29.secret"}"#.to_string(),
        };
        assert!(!format!("{auth:?}").contains("ya29"));
    }

    #[test]
    fn managed_subscriptions_ignore_editable_base_url_and_format() {
        let mut spec = ProviderSpec::new(
            "grok",
            "Grok",
            "https://attacker.example/anthropic",
            ApiFormat::Anthropic,
            ProviderAuth::XaiOauth { account_id: None },
        );
        assert_eq!(spec.effective_base_url(), XAI_API_BASE_URL);
        assert_eq!(spec.effective_api_format(), ApiFormat::OpenaiResponses);

        spec.auth = ProviderAuth::CodexOauth { account_id: None };
        assert_eq!(spec.effective_base_url(), CHATGPT_CODEX_BASE_URL);
        assert_eq!(spec.effective_api_format(), ApiFormat::OpenaiResponses);
    }

    #[test]
    fn copilot_never_uses_anthropic_format() {
        let mut spec = ProviderSpec::new(
            "copilot",
            "Copilot",
            "",
            ApiFormat::Anthropic,
            ProviderAuth::GithubCopilot { account_id: None },
        );
        assert_eq!(spec.effective_api_format(), ApiFormat::OpenaiChat);
        assert_eq!(spec.effective_base_url(), GITHUB_COPILOT_BASE_URL);

        spec.api_format = ApiFormat::OpenaiResponses;
        assert_eq!(spec.effective_api_format(), ApiFormat::OpenaiResponses);
    }

    #[test]
    fn spec_round_trips_through_json() {
        let json = serde_json::json!({
            "id": "deepseek",
            "name": "DeepSeek",
            "baseUrl": "https://api.deepseek.com/anthropic",
            "apiFormat": "anthropic",
            "auth": { "type": "api_key", "key": "sk-test" }
        });
        let spec: ProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.auth, ProviderAuth::api_key("sk-test"));
        assert_eq!(spec.prompt_cache_routing, PromptCacheRouting::Auto);
        assert!(!spec.full_url);

        let back: ProviderSpec =
            serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
    }
}

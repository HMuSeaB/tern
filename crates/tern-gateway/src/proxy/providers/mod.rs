//! Provider Adapters Module（裁剪版，取自 cc-switch）
//!
//! 只声明纯转换 / 认证模块。adapter、claude、codex、gemini 适配器依赖
//! cc-switch 的 `Provider.settings_config` 快照结构，第二阶段改写为
//! 基于中立 `ProviderSpec` 的版本后再加回。

mod auth;
pub(crate) mod codex_chat_common;
pub mod codex_chat_history;
pub mod codex_oauth_auth;
pub(crate) mod codex_responses_sse;
pub mod copilot_auth;
pub mod copilot_model_map;
pub(crate) mod gemini_schema;
pub mod gemini_shadow;
pub mod models;
pub(crate) mod reasoning_bridge;
pub mod streaming;
pub mod streaming_codex_anthropic;
pub mod streaming_codex_chat;
pub mod streaming_gemini;
pub mod streaming_responses;
pub mod transform;
pub mod transform_codex_anthropic;
pub mod transform_codex_chat;
pub mod transform_codex_responses_namespace;
pub mod transform_codex_responses_xai_sanitize;
pub mod transform_gemini;
pub mod transform_responses;
pub mod xai_oauth_auth;

pub const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const XAI_API_BASE_URL: &str = "https://api.x.ai/v1";

pub use auth::{AuthInfo, AuthStrategy};

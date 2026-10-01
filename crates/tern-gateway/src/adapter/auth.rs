//! 认证：`ProviderAuth` → `AuthInfo` → 请求头。
//!
//! 请求头规则合并自 cc-switch `ClaudeAdapter::get_auth_headers` 与
//! `GeminiAdapter::get_auth_headers`；Gemini OAuth 凭证解析取自 `GeminiAdapter`。

use http::{HeaderName, HeaderValue};

use crate::provider::{ApiFormat, KeyHeader, ProviderAuth, ProviderSpec};
use crate::proxy::providers::copilot_auth::{
    COPILOT_API_VERSION, COPILOT_EDITOR_VERSION, COPILOT_INTEGRATION_ID, COPILOT_PLUGIN_VERSION,
    COPILOT_USER_AGENT,
};
use crate::proxy::providers::{AuthInfo, AuthStrategy};
use crate::proxy::ProxyError;

// ChatGPT Codex 后端按 originator+version 组合做模型 cohort 路由：非官方身份会把
// gpt-5.6-luna 解析到未部署的内部引擎（HTTP 404 Model not found，openai/codex#31967）。
// 两个头必须成对发送，缺一即 404；version 需 ≥ 目标模型 catalog 的
// minimal_client_version（luna=0.144.0），新模型抬门槛时同步 bump。
const CODEX_OAUTH_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_OAUTH_CLIENT_VERSION: &str = "0.144.1";

/// 订阅类认证的 token 由转发层动态换取后填入 `AuthInfo.api_key`
pub fn requires_managed_token(auth: &ProviderAuth) -> bool {
    matches!(
        auth,
        ProviderAuth::GithubCopilot { .. }
            | ProviderAuth::CodexOauth { .. }
            | ProviderAuth::XaiOauth { .. }
    )
}

/// 解析供应商的认证信息。`None` 表示不需要认证（本地服务）。
///
/// 订阅类返回空 key 的占位 `AuthInfo`，转发层换到 token 后写入 `api_key`。
pub fn resolve_auth(spec: &ProviderSpec) -> Option<AuthInfo> {
    match &spec.auth {
        ProviderAuth::None => None,
        ProviderAuth::ApiKey { key, header } => {
            let key = key.trim();
            if key.is_empty() {
                log::warn!("[Auth] 供应商 {} 的 API Key 为空", spec.id);
                return None;
            }
            let strategy = match header {
                KeyHeader::Bearer => AuthStrategy::Bearer,
                KeyHeader::XApiKey => AuthStrategy::Anthropic,
                KeyHeader::XGoogApiKey => AuthStrategy::Google,
                KeyHeader::Auto => match spec.effective_api_format() {
                    ApiFormat::Anthropic => AuthStrategy::Anthropic,
                    ApiFormat::GeminiNative => AuthStrategy::Google,
                    ApiFormat::OpenaiChat | ApiFormat::OpenaiResponses => AuthStrategy::Bearer,
                },
            };
            Some(AuthInfo::new(key.to_string(), strategy))
        }
        ProviderAuth::GoogleOauth { credentials } => {
            let raw = credentials.trim().to_string();
            match parse_google_oauth_credentials(&raw) {
                Some(creds) if !creds.access_token.is_empty() => {
                    Some(AuthInfo::with_access_token(raw, creds.access_token))
                }
                // refresh_token-only JSON 或 access_token 为空：不能暴露空 bearer
                // （否则发出 `Authorization: Bearer ` 必然 401）。tern 目前也不做
                // refresh_token 换取，退化为把原值当 bearer，并明确提示用户刷新。
                Some(_) => {
                    log::warn!(
                        "[Gemini OAuth] 供应商 {} 缺少可用的 access_token，请求大概率 401。\
                         请用 gemini CLI 刷新 ~/.gemini/oauth_creds.json",
                        spec.id
                    );
                    Some(AuthInfo::new(raw, AuthStrategy::GoogleOAuth))
                }
                None => Some(AuthInfo::new(raw, AuthStrategy::GoogleOAuth)),
            }
        }
        ProviderAuth::GithubCopilot { .. } => {
            Some(AuthInfo::new(String::new(), AuthStrategy::GitHubCopilot))
        }
        ProviderAuth::CodexOauth { .. } => {
            Some(AuthInfo::new(String::new(), AuthStrategy::CodexOAuth))
        }
        ProviderAuth::XaiOauth { .. } => Some(AuthInfo::new(String::new(), AuthStrategy::XaiOAuth)),
    }
}

/// 生成认证请求头。
///
/// 订阅类的 `api_key` 必须已由转发层填入真实 token，否则返回 `AuthError`，
/// 避免把空的 `Authorization: Bearer ` 发出去。
/// `anthropic-version` 由转发层统一处理（透传客户端值或补默认值）。
pub fn auth_headers(auth: &AuthInfo) -> Result<Vec<(HeaderName, HeaderValue)>, ProxyError> {
    let is_managed = matches!(
        auth.strategy,
        AuthStrategy::GitHubCopilot | AuthStrategy::CodexOAuth | AuthStrategy::XaiOAuth
    );
    if is_managed && auth.api_key.trim().is_empty() {
        return Err(ProxyError::AuthError(
            "订阅 token 尚未获取，请先在 tern 中登录".to_string(),
        ));
    }

    let bearer = format!("Bearer {}", auth.api_key);
    Ok(match auth.strategy {
        AuthStrategy::Anthropic => vec![(HeaderName::from_static("x-api-key"), hv(&auth.api_key)?)],
        AuthStrategy::ClaudeAuth | AuthStrategy::Bearer | AuthStrategy::XaiOAuth => {
            vec![(HeaderName::from_static("authorization"), hv(&bearer)?)]
        }
        AuthStrategy::Google => vec![(
            HeaderName::from_static("x-goog-api-key"),
            hv(&auth.api_key)?,
        )],
        AuthStrategy::GoogleOAuth => {
            let token = auth.access_token.as_ref().unwrap_or(&auth.api_key);
            vec![
                (
                    HeaderName::from_static("authorization"),
                    hv(&format!("Bearer {token}"))?,
                ),
                (
                    HeaderName::from_static("x-goog-api-client"),
                    HeaderValue::from_static("GeminiCLI/1.0"),
                ),
            ]
        }
        // ChatGPT-Account-Id 依赖运行时选中的账号，由转发层追加
        AuthStrategy::CodexOAuth => vec![
            (HeaderName::from_static("authorization"), hv(&bearer)?),
            (
                HeaderName::from_static("originator"),
                HeaderValue::from_static(CODEX_OAUTH_ORIGINATOR),
            ),
            (
                HeaderName::from_static("version"),
                HeaderValue::from_static(CODEX_OAUTH_CLIENT_VERSION),
            ),
        ],
        AuthStrategy::GitHubCopilot => {
            let request_id = uuid::Uuid::new_v4().to_string();
            vec![
                (HeaderName::from_static("authorization"), hv(&bearer)?),
                (
                    HeaderName::from_static("editor-version"),
                    HeaderValue::from_static(COPILOT_EDITOR_VERSION),
                ),
                (
                    HeaderName::from_static("editor-plugin-version"),
                    HeaderValue::from_static(COPILOT_PLUGIN_VERSION),
                ),
                (
                    HeaderName::from_static("copilot-integration-id"),
                    HeaderValue::from_static(COPILOT_INTEGRATION_ID),
                ),
                (
                    HeaderName::from_static("user-agent"),
                    HeaderValue::from_static(COPILOT_USER_AGENT),
                ),
                (
                    HeaderName::from_static("x-github-api-version"),
                    HeaderValue::from_static(COPILOT_API_VERSION),
                ),
                (
                    HeaderName::from_static("openai-intent"),
                    HeaderValue::from_static("conversation-agent"),
                ),
                (
                    HeaderName::from_static("x-initiator"),
                    HeaderValue::from_static("user"),
                ),
                (
                    HeaderName::from_static("x-interaction-type"),
                    HeaderValue::from_static("conversation-agent"),
                ),
                // x-interaction-id 只在有会话时注入，由转发层追加
                (
                    HeaderName::from_static("x-vscode-user-agent-library-version"),
                    HeaderValue::from_static("electron-fetch"),
                ),
                (HeaderName::from_static("x-request-id"), hv(&request_id)?),
                (HeaderName::from_static("x-agent-task-id"), hv(&request_id)?),
            ]
        }
    })
}

/// 用户粘贴的 key 里可能有 CR/LF 等非法字符，返回 AuthError 而不是 panic
fn hv(value: &str) -> Result<HeaderValue, ProxyError> {
    HeaderValue::from_str(value)
        .map_err(|e| ProxyError::AuthError(format!("invalid auth header value: {e}")))
}

/// Gemini OAuth 凭证（取自 cc-switch `gemini.rs`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleOAuthCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

/// 解析裸 `ya29.` access token 或 `oauth_creds.json` 内容。
///
/// 从 oauth_creds.json 复制时常带前导换行，统一先 trim。
pub fn parse_google_oauth_credentials(raw: &str) -> Option<GoogleOAuthCredentials> {
    let raw = raw.trim();

    if raw.starts_with("ya29.") {
        return Some(GoogleOAuthCredentials {
            access_token: raw.to_string(),
            refresh_token: None,
            client_id: None,
            client_secret: None,
        });
    }

    if !raw.starts_with('{') {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(raw).ok()?;
    let field = |name: &str| json.get(name).and_then(|v| v.as_str()).map(str::to_string);

    let access_token = field("access_token").unwrap_or_default();
    let refresh_token = field("refresh_token");
    if access_token.is_empty() && refresh_token.is_none() {
        return None;
    }

    Some(GoogleOAuthCredentials {
        access_token,
        refresh_token,
        client_id: field("client_id"),
        client_secret: field("client_secret"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(format: ApiFormat, auth: ProviderAuth) -> ProviderSpec {
        ProviderSpec::new("test", "Test", "https://api.example.com", format, auth)
    }

    fn header_pairs(auth: &AuthInfo) -> Vec<(String, String)> {
        auth_headers(auth)
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect()
    }

    #[test]
    fn auto_key_header_follows_upstream_format() {
        let cases = [
            (ApiFormat::Anthropic, AuthStrategy::Anthropic),
            (ApiFormat::OpenaiChat, AuthStrategy::Bearer),
            (ApiFormat::OpenaiResponses, AuthStrategy::Bearer),
            (ApiFormat::GeminiNative, AuthStrategy::Google),
        ];
        for (format, expected) in cases {
            let auth = resolve_auth(&spec(format, ProviderAuth::api_key(" sk-test \n"))).unwrap();
            assert_eq!(auth.strategy, expected, "{format}");
            assert_eq!(auth.api_key, "sk-test");
        }
    }

    #[test]
    fn explicit_key_header_overrides_auto() {
        let auth = resolve_auth(&spec(
            ApiFormat::Anthropic,
            ProviderAuth::ApiKey {
                key: "sk-relay".to_string(),
                header: KeyHeader::Bearer,
            },
        ))
        .unwrap();
        assert_eq!(
            header_pairs(&auth),
            vec![("authorization".into(), "Bearer sk-relay".into())]
        );
    }

    #[test]
    fn empty_key_and_no_auth_yield_none() {
        assert!(resolve_auth(&spec(ApiFormat::OpenaiChat, ProviderAuth::api_key("  "))).is_none());
        assert!(resolve_auth(&spec(ApiFormat::OpenaiChat, ProviderAuth::None)).is_none());
    }

    #[test]
    fn static_key_headers() {
        let anthropic = AuthInfo::new("sk-ant".into(), AuthStrategy::Anthropic);
        assert_eq!(
            header_pairs(&anthropic),
            vec![("x-api-key".into(), "sk-ant".into())]
        );

        let google = AuthInfo::new("AIza".into(), AuthStrategy::Google);
        assert_eq!(
            header_pairs(&google),
            vec![("x-goog-api-key".into(), "AIza".into())]
        );
    }

    #[test]
    fn illegal_header_chars_are_rejected_not_panicking() {
        let auth = AuthInfo::new("sk-bad\r\nX-Inject: 1".into(), AuthStrategy::Anthropic);
        assert!(matches!(auth_headers(&auth), Err(ProxyError::AuthError(_))));
    }

    #[test]
    fn managed_subscriptions_need_token_before_headers() {
        for auth in [
            ProviderAuth::GithubCopilot { account_id: None },
            ProviderAuth::CodexOauth { account_id: None },
            ProviderAuth::XaiOauth { account_id: None },
        ] {
            assert!(requires_managed_token(&auth));
            let placeholder = resolve_auth(&spec(ApiFormat::OpenaiResponses, auth)).unwrap();
            assert!(placeholder.api_key.is_empty());
            assert!(matches!(
                auth_headers(&placeholder),
                Err(ProxyError::AuthError(_))
            ));
        }
        assert!(!requires_managed_token(&ProviderAuth::api_key("k")));
    }

    #[test]
    fn codex_oauth_sends_originator_and_version_together() {
        let auth = AuthInfo::new("token".into(), AuthStrategy::CodexOAuth);
        let headers = header_pairs(&auth);
        assert!(headers.contains(&("authorization".into(), "Bearer token".into())));
        assert!(headers.contains(&("originator".into(), CODEX_OAUTH_ORIGINATOR.into())));
        assert!(headers.contains(&("version".into(), CODEX_OAUTH_CLIENT_VERSION.into())));
    }

    #[test]
    fn copilot_headers_include_editor_identity() {
        let auth = AuthInfo::new("token".into(), AuthStrategy::GitHubCopilot);
        let headers = header_pairs(&auth);
        let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
        for expected in [
            "authorization",
            "editor-version",
            "copilot-integration-id",
            "x-request-id",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    // ---- Gemini OAuth（原 gemini.rs / claude.rs 测试） ----

    fn google(credentials: &str) -> AuthInfo {
        resolve_auth(&spec(
            ApiFormat::GeminiNative,
            ProviderAuth::GoogleOauth {
                credentials: credentials.to_string(),
            },
        ))
        .unwrap()
    }

    #[test]
    fn google_oauth_bare_access_token() {
        let auth = google("\nya29.raw-token-value\n");
        assert_eq!(auth.strategy, AuthStrategy::GoogleOAuth);
        assert_eq!(auth.access_token.as_deref(), Some("ya29.raw-token-value"));
    }

    #[test]
    fn google_oauth_json_with_leading_whitespace() {
        let auth = google("\n  {\"access_token\":\"ya29.valid\",\"refresh_token\":\"rt\"}\n");
        assert_eq!(auth.access_token.as_deref(), Some("ya29.valid"));
        let headers = header_pairs(&auth);
        assert!(headers.contains(&("authorization".into(), "Bearer ya29.valid".into())));
        assert!(headers.contains(&("x-goog-api-client".into(), "GeminiCLI/1.0".into())));
    }

    #[test]
    fn google_oauth_refresh_only_or_empty_token_never_exposes_empty_bearer() {
        for raw in [
            r#"{"refresh_token":"rt-abc","client_id":"cid","client_secret":"cs"}"#,
            r#"{"access_token":"","refresh_token":"rt-abc"}"#,
        ] {
            let auth = google(raw);
            assert_eq!(auth.strategy, AuthStrategy::GoogleOAuth);
            assert!(
                auth.access_token.as_deref().is_none_or(|t| !t.is_empty()),
                "empty access_token leaked for {raw}"
            );
        }
    }

    #[test]
    fn parse_google_oauth_credentials_shapes() {
        let direct = parse_google_oauth_credentials("ya29.test").unwrap();
        assert_eq!(direct.access_token, "ya29.test");
        assert!(direct.refresh_token.is_none());

        let json = parse_google_oauth_credentials(
            r#"{"access_token":"ya29.t","refresh_token":"1//r","client_id":"c","client_secret":"s"}"#,
        )
        .unwrap();
        assert_eq!(json.refresh_token.as_deref(), Some("1//r"));
        assert_eq!(json.client_id.as_deref(), Some("c"));

        assert!(parse_google_oauth_credentials("AIza-plain-key").is_none());
        assert!(parse_google_oauth_credentials("{not json").is_none());
        assert!(parse_google_oauth_credentials(r#"{"other":"x"}"#).is_none());
    }
}

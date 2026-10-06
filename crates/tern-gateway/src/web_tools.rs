//! 第三方网关下 WebSearch / WebFetch 可用性的识别与提示。
//!
//! # 为什么网关侧管不了
//!
//! `WebSearch` 和 `WebFetch` 走的**不是** `ANTHROPIC_BASE_URL` 这条消息通道：
//! 它们是 Claude Code 客户端自己发起的独立能力（域名安全校验、搜索端点），
//! 请求根本不经过 tern。所以协议转换做得再完美，这两个工具也可能完全不可用
//! ——这不是网关 bug，也不需要网关修。详见 `docs/guides` 下同名三语文档。
//!
//! # 这里做什么
//!
//! 只做**识别和告知**：判断某个上游能不能同时支撑这两个工具，把结论作为
//! 启动警告 / `check` 输出呈现出来，让用户在配好供应商后第一时间知道
//! 「这个网关下没有联网搜索」，而不是在报错时才去查。
//!
//! 判据只有 `base_url`：Anthropic 官方端点两个工具都正常，其余一律视为
//! 第三方。分辨不出「上游是兼容 Anthropic 协议的官方代理」这种少数情况，
//! 那种会误报——宁可多说一句，也好过用户以为是 tern 坏了。

use crate::ProviderSpec;

/// Anthropic 官方端点。两个联网工具在这里具备完整支持。
const ANTHROPIC_OFFICIAL_HOST: &str = "api.anthropic.com";

/// 对单个上游的联网工具可用性判断
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebToolsSupport {
    /// 官方端点，两个工具都正常
    Official,
    /// 第三方端点：搜索端点大概率不认，域名校验也可能出不去
    ThirdParty,
}

/// 判断上游能不能支撑 Claude Code 的两个联网工具。
///
/// `WebSearch` 的可用性还取决于账号侧是否开通，即使官方也不保证返回结果；
/// 这里只回答「端点层面支不支持」，不承诺一定搜得到东西。
pub fn assess(spec: &ProviderSpec) -> WebToolsSupport {
    if host_of(&spec.effective_base_url())
        .is_some_and(|host| host.eq_ignore_ascii_case(ANTHROPIC_OFFICIAL_HOST))
    {
        WebToolsSupport::Official
    } else {
        WebToolsSupport::ThirdParty
    }
}

/// 扫全部供应商，返回需要提醒的（第三方）那些。
///
/// 官方供应商不返回：两个工具都正常，列出来只是噪音。
pub fn third_party_providers(providers: &[ProviderSpec]) -> Vec<&ProviderSpec> {
    providers
        .iter()
        .filter(|spec| assess(spec) == WebToolsSupport::ThirdParty)
        .collect()
}

/// 给 `serve` / `check` 的警告文案。一条涵盖原因和三种处理方向。
pub fn warning_for(spec: &ProviderSpec) -> String {
    format!(
        "供应商 {} 是第三方网关（{}）：Claude Code 的 WebSearch / WebFetch 可能完全不可用。\
         这两个工具不走消息通道、不经过 tern，网关无法代为转发；\
         需要联网时请改用官方端点，或在 ~/.claude/settings.json 的 permissions.deny 里禁用这两个工具",
        spec.id,
        spec.effective_base_url(),
    )
}

/// 从 URL 里取 host，去掉用户信息和端口。解析失败时返回 None（按第三方处理）。
fn host_of(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest)?;
    let authority = after_scheme.split(['/', '?', '#']).next()?;
    // 去掉 userinfo：http://user:pass@host/
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 字面量带方括号，去掉端口时不能把括号也切掉
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => authority.split(':').next().unwrap_or(authority),
    };
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiFormat, ProviderAuth};

    fn spec(base_url: &str) -> ProviderSpec {
        ProviderSpec::new("p", "P", base_url, ApiFormat::Anthropic, ProviderAuth::None)
    }

    #[test]
    fn official_host_is_not_flagged() {
        for url in [
            "https://api.anthropic.com",
            "https://api.anthropic.com/",
            "http://api.anthropic.com",
            "https://API.ANTHROPIC.COM/v1",
        ] {
            assert_eq!(assess(&spec(url)), WebToolsSupport::Official, "{url}");
        }
    }

    #[test]
    fn third_party_is_flagged() {
        for url in [
            "https://api.deepseek.com/anthropic",
            "https://api.moonshot.cn/anthropic",
            "https://openrouter.ai/api/v1",
            "http://127.0.0.1:9999",
            "https://relay.example.com",
            // 官方域名的子域 / 冒名都不算官方
            "https://api.anthropic.com.evil.com",
            "https://notapi.anthropic.com",
        ] {
            assert_eq!(assess(&spec(url)), WebToolsSupport::ThirdParty, "{url}");
        }
    }

    #[test]
    fn host_parsing_handles_userinfo_port_and_ipv6() {
        assert_eq!(host_of("https://user:pass@api.anthropic.com:8443/x"), Some("api.anthropic.com"));
        assert_eq!(host_of("https://api.anthropic.com:443/"), Some("api.anthropic.com"));
        assert_eq!(host_of("http://[::1]:15800/v1"), Some("::1"));
        assert_eq!(host_of("api.anthropic.com"), None);
        assert_eq!(host_of(""), None);
    }

    #[test]
    fn userinfo_on_official_host_does_not_fool_the_check() {
        // 攻击面：用 @ 把真 host 藏进 userinfo。必须仍判第三方。
        assert_eq!(
            assess(&spec("https://api.anthropic.com@evil.com/")),
            WebToolsSupport::ThirdParty
        );
    }

    #[test]
    fn filter_keeps_only_third_party() {
        let providers = [spec("https://api.anthropic.com"), spec("https://relay.example.com")];
        let flagged = third_party_providers(&providers);
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].id, "p");
    }

    #[test]
    fn warning_names_the_provider_and_the_reason() {
        let text = warning_for(&spec("https://relay.example.com"));
        assert!(text.contains('p'), "{text}");
        assert!(text.contains("relay.example.com"), "{text}");
        assert!(text.contains("WebSearch"), "{text}");
        // 必须说清「不是网关的错」，否则用户会来提 issue
        assert!(text.contains("不经过 tern"), "{text}");
    }
}

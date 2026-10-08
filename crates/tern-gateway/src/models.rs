//! 拉取供应商的模型列表（「获取模型列表」按钮）。
//!
//! # 为什么需要
//!
//! 用户配了一个中转站，不知道该往 `model` 里填什么。让他去翻供应商文档、
//! 或者靠猜，都是把成本推给用户——中转站的模型名千奇百怪
//! （`step-3.5-flash-2603`、`kimi-k2.5`），猜错只得到一句 404。
//!
//! 上游通常有一个 OpenAI 兼容的 `GET /v1/models`，问它就行。
//!
//! # 移植自 cc-switch
//!
//! 候选 URL 的构造逻辑（`build_models_url_candidates`）和错误处理策略
//! 照搬 cc-switch 的 `services/model_fetch.rs`——那是踩过一堆中转站之后
//! 收敛出来的：版本段结尾的（智谱 `/api/coding/paas/v4`）不能再补 `/v1`，
//! Anthropic 兼容子路径（`/anthropic`、`/step_plan`）要剥掉再试。
//! 我自己重写只会把这些坑再踩一遍。
//!
//! # 边界
//!
//! - key 为空时直接拒绝：拿着空 key 去问只会得到 401，白等一个超时
//! - 404 / 405 当"这个候选不对"继续试下一个；其他错误直接返回——
//!   鉴权失败、限额这些试下一个候选也没用
//! - key 不进日志：日志里只出现掩码后的地址

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 获取到的模型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedModel {
    pub id: String,
    pub owned_by: Option<String>,
}

/// OpenAI 兼容的 `/models` 响应。`data` 允许缺失——
/// 有些中转站返回 `{"object":"list"}` 这种空壳，那应当算出 0 个模型
/// 而不是解析失败。
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Option<Vec<ModelEntry>>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
}

const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// 404/405 响应体保留的字符数。中转站的 404 页常常是一整页 HTML，
/// 整个塞进错误信息里会把界面撑爆。
const ERROR_BODY_MAX_CHARS: usize = 512;

/// 已知的「Anthropic 协议兼容子路径」后缀；**按长度降序**，
/// 这样最长前缀优先命中——否则 `/anthropic` 会提前匹配掉 `/api/anthropic`。
const KNOWN_COMPAT_SUFFIXES: &[&str] = &[
    "/api/claudecode",
    "/api/anthropic",
    "/apps/anthropic",
    "/api/coding",
    "/claudecode",
    "/anthropic",
    "/step_plan",
    "/coding",
    "/claude",
];

/// 拉模型列表。
///
/// `models_url_override` 非空时只试它（用户手填了就别自作聪明）；
/// `base_url` 为空时报错。
pub fn fetch_models(
    base_url: &str,
    api_key: &str,
    full_url: bool,
    models_url_override: Option<&str>,
) -> Result<Vec<FetchedModel>, String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("没有 API Key，拉不到模型列表".to_string());
    }
    let candidates = build_models_url_candidates(base_url, full_url, models_url_override)?;

    // 用阻塞客户端：调用方（tauri 命令 / agent）已经在非 async 的上下文里。
    // 每次拉列表新建一个 client 而不是全局复用——这个操作一天点不了几次，
    // 不值得为它引入连接池的生命周期管理；顺带避免复用一个可能被配置污染的 client。
    let client = reqwest::blocking::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut last_error: Option<String> = None;
    for url in &candidates {
        let response = match client
            .get(url)
            .header("authorization", format!("Bearer {api_key}"))
            .send()
        {
            Ok(response) => response,
            Err(error) => {
                // 连接失败（DNS、超时、拒绝）对所有候选都一样，
                // 继续试只是把用户的等待时间乘以候选数
                return Err(format!("连不上 {url}: {error}"));
            }
        };
        let status = response.status();

        if status.is_success() {
            let body: ModelsResponse = response
                .json()
                .map_err(|error| format!("响应解析失败: {error}"))?;
            let mut models: Vec<FetchedModel> = body
                .data
                .unwrap_or_default()
                .into_iter()
                .map(|entry| FetchedModel {
                    id: entry.id,
                    owned_by: entry.owned_by.filter(|s| !s.trim().is_empty()),
                })
                .collect();
            // 排序：上游返回的顺序常常是内部 id 序，用户看不出个头绪
            models.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(models);
        }

        if status == reqwest::StatusCode::NOT_FOUND
            || status == reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            // 这个候选不对，试下一个。把响应体留着——有些中转站会在 404 页里
            // 写清楚正确路径
            last_error = Some(format!(
                "HTTP {} {}",
                status.as_u16(),
                truncate_body(response.text().unwrap_or_default())
            ));
            continue;
        }

        let body = truncate_body(response.text().unwrap_or_default());
        return Err(format!("HTTP {}: {body}", status.as_u16()));
    }

    Err(format!(
        "所有候选地址都不对：{}",
        last_error.unwrap_or_else(|| "没有可试的地址".to_string())
    ))
}

/// 构造「模型列表端点」的候选 URL 列表，去重且保持首次出现顺序。
///
/// 候选顺序：
/// 1. `models_url_override` 非空 → 只返回它
/// 2. baseURL 已以版本段 `/v{N}` 结尾（`/v1`、智谱 `/api/coding/paas/v4`）→
///    `{base}/models`，不能再补 `/v1`；版本不是 v1 时再追加 `/v1/models` 兜底
/// 3. 否则 → `{base}/v1/models`
/// 4. baseURL 命中 [`KNOWN_COMPAT_SUFFIXES`] → 剥离后缀后追加
///    `{root}/v1/models`、`{root}/models`
///
/// `full_url` 为真时 baseURL 本身是完整端点，从里面截出 `/v1/` 之前的根。
pub fn build_models_url_candidates(
    base_url: &str,
    full_url: bool,
    models_url_override: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(raw) = models_url_override {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(vec![trimmed.to_string()]);
        }
    }

    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("供应商地址为空".to_string());
    }

    let mut candidates: Vec<String> = Vec::new();

    if full_url {
        if let Some(idx) = trimmed.find("/v1/") {
            candidates.push(format!("{}/v1/models", &trimmed[..idx]));
        } else if let Some(idx) = trimmed.rfind('/') {
            let root = &trimmed[..idx];
            if root.contains("://") && root.len() > root.find("://").unwrap() + 3 {
                candidates.push(format!("{root}/v1/models"));
            }
        }
        if candidates.is_empty() {
            return Err("从完整端点推不出模型列表地址".to_string());
        }
        return Ok(dedupe(candidates));
    }

    if ends_with_version_segment(trimmed) {
        candidates.push(format!("{trimmed}/models"));
        if !trimmed.ends_with("/v1") {
            candidates.push(format!("{trimmed}/v1/models"));
        }
    } else {
        candidates.push(format!("{trimmed}/v1/models"));
    }

    if let Some(stripped) = strip_compat_suffix(trimmed) {
        let root = stripped.trim_end_matches('/');
        if !root.is_empty() && root.contains("://") {
            candidates.push(format!("{root}/v1/models"));
            candidates.push(format!("{root}/models"));
        }
    }

    Ok(dedupe(candidates))
}

/// 候选最多 4 条，线性去重就够，不值得上 HashSet。
fn dedupe(candidates: Vec<String>) -> Vec<String> {
    let mut unique: Vec<String> = Vec::with_capacity(candidates.len());
    for url in candidates {
        if !unique.contains(&url) {
            unique.push(url);
        }
    }
    unique
}

/// 若 baseURL 以任一已知兼容子路径结尾，返回剥离后的剩余部分。
fn strip_compat_suffix(base_url: &str) -> Option<&str> {
    for suffix in KNOWN_COMPAT_SUFFIXES {
        if base_url.ends_with(*suffix) {
            return Some(&base_url[..base_url.len() - suffix.len()]);
        }
    }
    None
}

/// baseURL 是否以 OpenAI 风格的版本段 `/v{N}` 结尾（`/v1`、`/v10`、
/// 智谱 `/api/coding/paas/v4`）。这类 URL 版本号已在路径里。
fn ends_with_version_segment(url: &str) -> bool {
    let last = url.rsplit('/').next().unwrap_or("");
    last.strip_prefix('v')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn truncate_body(body: String) -> String {
    if body.chars().count() <= ERROR_BODY_MAX_CHARS {
        return body;
    }
    let mut truncated: String = body.chars().take(ERROR_BODY_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_for_plain_root() {
        let c = build_models_url_candidates("https://api.siliconflow.cn", false, None).unwrap();
        assert_eq!(c, vec!["https://api.siliconflow.cn/v1/models"]);
    }

    #[test]
    fn candidates_strip_a_trailing_slash() {
        let c = build_models_url_candidates("https://api.example.com/", false, None).unwrap();
        assert_eq!(c, vec!["https://api.example.com/v1/models"]);
    }

    #[test]
    fn candidates_when_base_already_ends_with_v1() {
        let c = build_models_url_candidates("https://api.example.com/v1", false, None).unwrap();
        assert_eq!(c, vec!["https://api.example.com/v1/models"]);
    }

    /// 版本段非 /v1 时不能再补一个 /v1：智谱就是 `.../coding/paas/v4`，
    /// 补完是 `/v4/v1/models`，404
    #[test]
    fn candidates_for_a_non_v1_version_segment() {
        let c =
            build_models_url_candidates("https://open.bigmodel.cn/api/coding/paas/v4", false, None)
                .unwrap();
        assert_eq!(
            c,
            vec![
                "https://open.bigmodel.cn/api/coding/paas/v4/models",
                "https://open.bigmodel.cn/api/coding/paas/v4/v1/models",
            ]
        );
    }

    #[test]
    fn versions_other_than_v1_are_recognized() {
        assert!(ends_with_version_segment("https://x.com/v1"));
        assert!(ends_with_version_segment("https://x.com/v10"));
        assert!(ends_with_version_segment(
            "https://open.bigmodel.cn/api/coding/paas/v4"
        ));
        assert!(!ends_with_version_segment("https://x.com/api"));
        assert!(!ends_with_version_segment("https://x.com/vX"));
        assert!(!ends_with_version_segment("https://api.siliconflow.cn"));
    }

    /// Anthropic 兼容子路径要剥掉：`api.deepseek.com/anthropic` 的模型列表
    /// 不在 `/anthropic/v1/models` 下
    #[test]
    fn candidates_strip_the_anthropic_compat_suffix() {
        let c =
            build_models_url_candidates("https://api.deepseek.com/anthropic", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.deepseek.com/anthropic/v1/models",
                "https://api.deepseek.com/v1/models",
                "https://api.deepseek.com/models",
            ]
        );
    }

    #[test]
    fn candidates_strip_step_plan() {
        let c =
            build_models_url_candidates("https://api.stepfun.com/step_plan", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.stepfun.com/step_plan/v1/models",
                "https://api.stepfun.com/v1/models",
                "https://api.stepfun.com/models",
            ]
        );
    }

    /// 最长后缀优先：`/api/anthropic` 整个剥掉，
    /// 不能只剥 `/anthropic` 留下一个残缺的 `.../api`
    #[test]
    fn the_longest_matching_suffix_wins() {
        let c = build_models_url_candidates("https://api.z.ai/api/anthropic", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.z.ai/api/anthropic/v1/models",
                "https://api.z.ai/v1/models",
                "https://api.z.ai/models",
            ]
        );
    }

    #[test]
    fn no_suffix_means_no_extra_candidates() {
        let c = build_models_url_candidates("https://openrouter.ai/api", false, None).unwrap();
        assert_eq!(c, vec!["https://openrouter.ai/api/v1/models"]);
    }

    #[test]
    fn a_full_url_is_cut_at_the_version_segment() {
        let c = build_models_url_candidates(
            "https://proxy.example.com/v1/chat/completions",
            true,
            None,
        )
        .unwrap();
        assert_eq!(c, vec!["https://proxy.example.com/v1/models"]);
    }

    #[test]
    fn an_override_wins_over_everything() {
        let c = build_models_url_candidates(
            "https://api.deepseek.com/anthropic",
            false,
            Some("https://api.deepseek.com/models"),
        )
        .unwrap();
        assert_eq!(c, vec!["https://api.deepseek.com/models"]);
    }

    #[test]
    fn a_blank_override_is_ignored() {
        let c =
            build_models_url_candidates("https://api.siliconflow.cn", false, Some("   ")).unwrap();
        assert_eq!(c, vec!["https://api.siliconflow.cn/v1/models"]);
    }

    #[test]
    fn an_empty_base_url_is_an_error() {
        assert!(build_models_url_candidates("", false, None).is_err());
        assert!(build_models_url_candidates("   ", false, None).is_err());
    }

    #[test]
    fn a_bare_host_produces_one_candidate() {
        let c = build_models_url_candidates("https://host.example.com", false, None).unwrap();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // 中文一个 3 字节：按字节切会切出乱码
        assert_eq!(truncate_body("中文测试".into()).chars().count(), 4);
        let long = "x".repeat(600);
        let out = truncate_body(long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), ERROR_BODY_MAX_CHARS + 1);
    }

    /// 用一个真起的 HTTP 服务走完整流程：候选地址、鉴权、解析、排序。
    ///
    /// 前面那些测试只覆盖 URL 构造，而"能不能真从上游拿到列表"是另一件事——
    /// 鉴权头带对没有、`data` 缺失时算不算失败、排序做没做。
    /// 不打真上游：那要消耗用户的 key，而且结果随上游变动。
    #[test]
    fn fetch_models_end_to_end_against_a_local_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        // 故意打乱顺序：fetch_models 承诺按 id 排序
        let body =
            r#"{"object":"list","data":[{"id":"zzz-model","owned_by":"x"},{"id":"aaa-model"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );

        std::thread::spawn(move || {
            // 只服务一次：这个测试只发一个请求
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buffer = [0u8; 2048];
                let read = socket.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                // 鉴权头必须带上：不带的话上游会 401，而这里要验的正是它
                assert!(
                    request
                        .to_lowercase()
                        .contains("authorization: bearer test-key"),
                    "没带鉴权头: {request}"
                );
                let _ = socket.write_all(response.as_bytes());
            }
        });

        let models = fetch_models(&base, "  test-key  ", false, None).unwrap();
        // key 两边的空白要被去掉再用于鉴权
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "aaa-model", "要按 id 排序");
        assert_eq!(models[1].id, "zzz-model");
        // owned_by 缺失时是 None，不是空串——前端要能区分"上游没说"和"说是空的"
        assert_eq!(models[0].owned_by, None);
        assert_eq!(models[1].owned_by.as_deref(), Some("x"));
    }

    /// 空 key 直接拒绝，不发请求。
    ///
    /// 带着空 key 去问只会等到一个 401，白耗一个超时；而"没配 key"是
    /// 用户立刻能看懂的状态，两句话的代价差很多。
    #[test]
    fn an_empty_key_is_refused_before_any_request() {
        for key in ["", "   "] {
            let error = fetch_models("https://api.example.com", key, false, None).unwrap_err();
            assert!(error.contains("API Key"), "{error}");
        }
    }

    /// 404/405 时试下一个候选，全部候选都失败才报错。
    ///
    /// 中转站常常只在某一条路径下提供 /models——第一个候选 404 是正常的，
    /// 这时放弃会让大部分第三方站点都取不到模型。
    #[test]
    fn a_404_falls_through_to_the_next_candidate() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // 两个候选：第一个恒 404，第二个恒 200。
        // 起两个服务，把第一个的地址塞进 base_url 让第一个候选必然命中它
        let not_found = TcpListener::bind("127.0.0.1:0").unwrap();
        let not_found_addr = not_found.local_addr().unwrap();
        let ok = TcpListener::bind("127.0.0.1:0").unwrap();
        let ok_addr = ok.local_addr().unwrap();

        let ok_body = r#"{"data":[{"id":"only-model"}]}"#;
        let ok_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            ok_body.len(),
            ok_body
        );

        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = not_found.accept() {
                let mut buffer = [0u8; 512];
                let _ = socket.read(&mut buffer);
                let _ = socket.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = ok.accept() {
                let mut buffer = [0u8; 512];
                let _ = socket.read(&mut buffer);
                let _ = socket.write_all(ok_response.as_bytes());
            }
        });

        // base_url 用 not_found 那个地址：第一个候选是它 + /v1/models（404），
        // 第二个候选是剥离兼容后缀后的根——这里没后缀可剥，于是只剩它自己。
        // 为了真正走通"失败后换下一个"，用 override 把第二个候选指到 ok 服务。
        let models = fetch_models(
            &format!("http://{not_found_addr}"),
            "test-key",
            false,
            Some(&format!("http://{ok_addr}/models")),
        )
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "only-model");
    }

    /// `{"object":"list"}` 这种空壳要算出 0 个模型，不是解析失败。
    /// 有些中转站就这么返回。
    #[test]
    fn a_missing_data_field_is_an_empty_list_not_an_error() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = r#"{"object":"list"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buffer = [0u8; 512];
                let _ = socket.read(&mut buffer);
                let _ = socket.write_all(response.as_bytes());
            }
        });

        let models = fetch_models(&format!("http://{addr}"), "k", false, None).unwrap();
        assert!(models.is_empty());
    }
}

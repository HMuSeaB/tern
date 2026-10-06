//! TUI 用的数据快照。
//!
//! 一次读全三页要用的东西，渲染时不再碰磁盘：事件循环每 5 秒刷一次，
//! 中间无论怎么切页都是同一份数据，不会出现"这页的数字和那页对不上"。

use std::path::{Path, PathBuf};

use tern_gateway::GatewayConfig;
use tern_store::{DayRange, Store};

/// 三页共用的快照。读库失败不致命：照样显示界面，只是提示为什么没数据。
pub struct Snapshot {
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    /// 配置读不出来时的原因（文件不存在、JSON 坏）
    pub config_error: Option<String>,
    /// 库里还没任何记录（用户还没跑过网关）
    pub empty: bool,
    /// 库读不出来时的原因
    pub db_error: Option<String>,

    pub listen: Option<String>,
    pub default_provider: Option<String>,
    pub providers: Vec<ProviderRow>,

    // 今日 + 昨日，做"较昨日"
    pub today: Option<DayTotals>,
    pub yesterday: Option<DayTotals>,
    /// 最近几天的每日花费，画 sparkline（旧 → 新）
    pub spend_series: Vec<f64>,
    pub unpriced: Vec<(String, u64, u64)>,
    pub failures: Vec<(String, String, u16, u64)>,
    pub recent: Vec<RecentRow>,
}

pub struct ProviderRow {
    pub id: String,
    pub base_url: String,
    pub api_format: String,
    /// 第三方网关：WebSearch / WebFetch 会失效
    pub web_tools_at_risk: bool,
    /// 指向本机回环：等于套两层网关，多半是配置时留下的
    pub loopback: bool,
    pub is_default: bool,
}

pub struct DayTotals {
    pub cost: f64,
    pub requests: u64,
    pub failures: u64,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_savings: f64,
    pub unpriced: u64,
}

pub struct RecentRow {
    pub time: String,
    pub client: String,
    pub model: String,
    /// 客户端模型 ≠ 上游模型时，说明被供应商换过
    pub remapped: bool,
    pub outcome: String,
    pub tokens: u64,
    pub cost: Option<f64>,
}

impl Snapshot {
    pub fn load(config_path: Option<&Path>) -> Self {
        let (config_path, config) = match resolve_config(config_path) {
            Ok(pair) => pair,
            Err(error) => {
                return Self::empty(config_path.map(Path::to_path_buf), Some(error));
            }
        };

        let db_path = db_path_for(&config_path);
        let providers = config
            .providers
            .iter()
            .map(|spec| ProviderRow {
                id: spec.id.clone(),
                base_url: spec.effective_base_url(),
                api_format: spec.effective_api_format().to_string(),
                web_tools_at_risk: matches!(
                    tern_gateway::assess(spec),
                    tern_gateway::WebToolsSupport::ThirdParty
                ),
                loopback: is_loopback(&spec.effective_base_url()),
                is_default: config.default_provider.as_deref() == Some(spec.id.as_str()),
            })
            .collect();

        // 库不在就不读：`Store::open` 会顺手建库并迁移，
        // 那对"只是看一眼面板"来说越权了
        if !db_path.exists() {
            return Self {
                config_path,
                db_path: db_path.clone(),
                config_error: None,
                empty: true,
                db_error: Some(format!("{} 还不存在", db_path.display())),
                listen: Some(config.listen.to_string()),
                default_provider: config.default_provider.clone(),
                providers,
                today: None,
                yesterday: None,
                spend_series: Vec::new(),
                unpriced: Vec::new(),
                failures: Vec::new(),
                recent: Vec::new(),
            };
        }

        let store = match Store::open(&db_path) {
            Ok(store) => store,
            Err(error) => {
                return Self {
                    config_path,
                    db_path,
                    config_error: None,
                    empty: true,
                    db_error: Some(error.to_string()),
                    listen: Some(config.listen.to_string()),
                    default_provider: config.default_provider.clone(),
                    providers,
                    today: None,
                    yesterday: None,
                    spend_series: Vec::new(),
                    unpriced: Vec::new(),
                    failures: Vec::new(),
                    recent: Vec::new(),
                };
            }
        };
        let today = store.summary(&DayRange::today()).ok().map(totals_of);
        let yesterday = store
            .summary(&DayRange::last_days(2))
            .ok()
            .and_then(|_| store.summary(&single_day(1)).ok())
            .map(totals_of);
        let spend_series = daily_spend(&store, 30);
        // 一条记录都没有时不算"有数据"：界面上要给的是引导，不是一片 0
        let empty = today.as_ref().is_none_or(|t| t.requests == 0);

        let unpriced = store
            .unpriced_models(&DayRange::today())
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.model, m.requests, m.tokens))
            .collect();
        let failures = store
            .failures(&DayRange::today())
            .unwrap_or_default()
            .into_iter()
            .map(|f| {
                (
                    f.provider_id.unwrap_or_else(|| "未路由".into()),
                    f.error_kind,
                    f.status,
                    f.count,
                )
            })
            .collect();
        let recent = store
            .recent(12)
            .unwrap_or_default()
            .into_iter()
            .map(|r| RecentRow {
                time: clock(r.started_at_ms),
                client: r.client,
                model: r
                    .response_model
                    .clone()
                    .unwrap_or_else(|| r.client_model.clone()),
                remapped: r
                    .response_model
                    .is_some_and(|m| m != r.client_model),
                outcome: r.outcome,
                tokens: r.fresh_input + r.output + r.cache_read + r.cache_write,
                cost: r.cost.map(|c| c.to_string().parse().unwrap_or(0.0)),
            })
            .collect();

        Self {
            config_path,
            db_path,
            config_error: None,
            empty,
            db_error: None,
            listen: Some(config.listen.to_string()),
            default_provider: config.default_provider.clone(),
            providers,
            today,
            yesterday,
            spend_series,
            unpriced,
            failures,
            recent,
        }
    }

    fn empty(config_path: Option<PathBuf>, config_error: Option<String>) -> Self {
        Self {
            config_path: config_path.unwrap_or_else(|| PathBuf::from("tern.json")),
            db_path: PathBuf::new(),
            config_error,
            empty: true,
            db_error: None,
            listen: None,
            default_provider: None,
            providers: Vec::new(),
            today: None,
            yesterday: None,
            spend_series: Vec::new(),
            unpriced: Vec::new(),
            failures: Vec::new(),
            recent: Vec::new(),
        }
    }

    /// 崩没崩、为什么，一句话。顶栏右侧显示。
    pub fn status_text(&self) -> String {
        if let Some(error) = &self.config_error {
            return format!("配置有问题：{error}");
        }
        if self.empty {
            return match &self.db_error {
                Some(error) => format!("还没有用量数据（{error}）"),
                None => "还没有用量数据".into(),
            };
        }
        let today = self.today.as_ref();
        let cost = today.map(|t| t.cost).unwrap_or(0.0);
        format!("今天 ${cost:.2}")
    }
}

fn resolve_config(explicit: Option<&Path>) -> std::result::Result<(PathBuf, GatewayConfig), String> {
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None => std::env::var_os("TERN_CONFIG")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|d| d.join("tern").join("tern.json")))
            .ok_or("找不到系统配置目录")?,
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读不了 {}: {e}", path.display()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let config: GatewayConfig = serde_json::from_str(text).map_err(|e| format!("{} 解析失败: {e}", path.display()))?;
    Ok((path, config))
}

/// 与 CLI / 面板同一个规则：配置同目录的 usage.db，TERN_DB 优先
fn db_path_for(config_path: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("TERN_DB").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    config_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("usage.db")
}

fn totals_of(summary: tern_store::Summary) -> DayTotals {
    DayTotals {
        // Decimal → f64 只用于显示，不做加减，精度损失无所谓
        cost: summary.cost.to_string().parse().unwrap_or(0.0),
        requests: summary.requests,
        failures: summary.failures,
        fresh_input: summary.fresh_input,
        output: summary.output,
        cache_read: summary.cache_read,
        cache_write: summary.cache_write,
        cache_savings: summary.cache_savings.to_string().parse().unwrap_or(0.0),
        unpriced: summary.unpriced,
    }
}

/// Decimal → f64。只用于显示，不做加减，精度损失无所谓。
fn decimal_to_f64(value: &rust_decimal::Decimal) -> f64 {
    value.to_string().parse().unwrap_or(0.0)
}

fn is_loopback(url: &str) -> bool {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    // 去掉 userinfo：http://user:pass@host/
    let authority = authority.rsplit('@').next().unwrap_or(authority);

    // 剥端口。IPv6 字面量带方括号（[::1]:8080），括号内的冒号不是端口分隔
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => authority.split(':').next().unwrap_or(authority),
    };

    host == "localhost"
        || host == "127.0.0.1"
        || host.starts_with("127.")
        || host == "::1"
        || host == "0.0.0.0"
        || host == "[::1]"
}

/// 最近 n 天每天的总额，旧 → 新。没数据的天补 0，sparkline 才是连续的时间轴。
fn daily_spend(store: &Store, days: i64) -> Vec<f64> {
    let mut out = Vec::with_capacity(days as usize);
    for offset in (0..days).rev() {
        let range = single_day(offset);
        let cost = store
            .summary(&range)
            .map(|s| decimal_to_f64(&s.cost))
            .unwrap_or(0.0);
        out.push(cost);
    }
    out
}

fn single_day(days_ago: i64) -> DayRange {
    let today = chrono::Local::now().date_naive();
    let day = today - chrono::Days::new(u64::try_from(days_ago).unwrap_or(0));
    DayRange {
        from: day.format("%Y-%m-%d").to_string(),
        to: day.format("%Y-%m-%d").to_string(),
    }
}

fn clock(ms: i64) -> String {
    use chrono::{TimeZone, Local};
    match Local.timestamp_millis_opt(ms).single() {
        Some(dt) => dt.format("%H:%M:%S").to_string(),
        None => "--:--:--".into(),
    }
}

/// 供测试：确认快照在没有库时也能构造出来（界面不该因为没数据就起不来）
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_survives_missing_config() {
        let snapshot = Snapshot::load(Some(Path::new("definitely-not-here.json")));
        assert!(snapshot.config_error.is_some());
        assert!(snapshot.empty);
        assert!(snapshot.status_text().contains("配置"), "{}", snapshot.status_text());
    }

    #[test]
    fn loopback_detection_catches_local_addresses() {
        for url in [
            "http://127.0.0.1:8045",
            "http://localhost:4000",
            "http://0.0.0.0:4000",
            "http://[::1]:15800",
        ] {
            assert!(is_loopback(url), "{url}");
        }
        for url in [
            "https://api.deepseek.com/anthropic",
            "https://openrouter.ai/api",
        ] {
            assert!(!is_loopback(url), "{url}");
        }
    }

    #[test]
    fn money_converts_decimal_without_panicking() {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        // 正常值
        assert!((decimal_to_f64(&Decimal::from_str("18.47").unwrap()) - 18.47).abs() < 1e-9);
        // 极小值：纳美元口径的成本经常是 0.000182 这种
        assert!(
            (decimal_to_f64(&Decimal::from_str("0.000182").unwrap()) - 0.000182).abs() < 1e-12
        );
    }
}

//! 给前端的查询命令。
//!
//! SQL 口径与 `tern-store::query` 逐字对齐，这样面板和 `tern usage` 命令行
//! 永远对得上。凡是上百万行的明细都走 `daily` 预聚合表，不把明细拉进前端。

use serde::Serialize;
use tauri::State;

use crate::error::Result;
use crate::AppState;

/// 与 `tern_store::query::Summary` 同构，成本给字符串避免 JS 的 float 误差
/// （纳美元是 i64，转 f64 再过 JSON 会在小数位上丢数）
#[derive(Debug, Serialize)]
pub struct SummaryDto {
    pub requests: u64,
    pub failures: u64,
    pub aborted: u64,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: String,
    pub cache_savings: String,
    pub unpriced: u64,
}

/// 面板首屏需要的全部数字。按 cc-switch 使用统计页的密度做：
/// 一个 hero（Token 总量）+ 一组小卡 + 两条醒目提示。
#[derive(Debug, Serialize)]
pub struct PanelDto {
    /// 数据库文件路径，出错时前端可以直接展示
    pub db_path: String,
    /// 今天没有数据时为 true，前端显示引导而不是 0
    pub first_run: bool,
    pub today: SummaryDto,
    /// 昨天同口径，"较昨日"对比用
    pub yesterday: SummaryDto,
    /// 未定价模型：有 token 却没查到价，成本图因此偏低
    pub unpriced_models: Vec<UnpricedDto>,
    /// 失败按（原因, 供应商, 状态码）聚类，不混进模型统计
    pub failures: Vec<FailureDto>,
    /// 最近若干条，给"请求流"折叠区用
    pub recent: Vec<RecentDto>,
}

#[derive(Debug, Serialize)]
pub struct UnpricedDto {
    pub model: String,
    pub requests: u64,
    pub tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct FailureDto {
    pub error_kind: String,
    pub provider_id: Option<String>,
    pub status: u16,
    pub count: u64,
    pub sample: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecentDto {
    pub started_at_ms: i64,
    pub client: String,
    pub provider_id: Option<String>,
    /// 客户端原始模型名
    pub client_model: String,
    /// 上游回显的模型名，被供应商换过模型时和 client_model 不同
    pub response_model: Option<String>,
    pub role: String,
    pub status: u16,
    pub outcome: String,
    pub error_kind: Option<String>,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: Option<String>,
    pub duration_ms: u64,
}

/// `Summary` 的十个聚合列，与 tern-store 的 SUMMARY_COLUMNS 一致
const SUMMARY_COLUMNS: &str = "COALESCE(SUM(requests), 0), COALESCE(SUM(failures), 0),
    COALESCE(SUM(aborted), 0), COALESCE(SUM(fresh_input), 0), COALESCE(SUM(output), 0),
    COALESCE(SUM(cache_read), 0), COALESCE(SUM(cache_write), 0), COALESCE(SUM(cost_nano), 0),
    COALESCE(SUM(savings_nano), 0), COALESCE(SUM(unpriced), 0)";

/// 纳美元 → 十进制字符串。与 `tern_store::query::nano_to_usd` 保持同样精度。
fn nano_to_usd_string(nano: i64) -> String {
    // 手写而不是用 Decimal：这里只需要显示，且不想让面板依赖 rust_decimal 的版本
    let sign = if nano < 0 { "-" } else { "" };
    let nano = nano.unsigned_abs();
    format!("{}{}.{:09}", sign, nano / 1_000_000_000, nano % 1_000_000_000)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

// hero 的 token 总量、缓存命中率由前端自己算（Dto 只搬原始桶），
// 不在 Rust 侧留一份同逻辑的两处实现。

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryDto> {
    let u = |i: usize| -> rusqlite::Result<u64> {
        Ok(u64::try_from(row.get::<_, i64>(i)?).unwrap_or(0))
    };
    Ok(SummaryDto {
        requests: u(0)?,
        failures: u(1)?,
        aborted: u(2)?,
        fresh_input: u(3)?,
        output: u(4)?,
        cache_read: u(5)?,
        cache_write: u(6)?,
        cost: nano_to_usd_string(row.get(7)?),
        cache_savings: nano_to_usd_string(row.get(8)?),
        unpriced: u(9)?,
    })
}

/// 本地日期 `YYYY-MM-DD`，与写入时算 `day` 列的 `tern_store::local_day` 同一时区口径
fn local_days_ago(days: i64) -> String {
    (chrono::Local::now().date_naive() - chrono::Days::new(u64::try_from(days).unwrap_or(0)))
        .format("%Y-%m-%d")
        .to_string()
}

/// 打开数据库并返回首屏全部数据。库不存在时返回错误，前端据此显示引导。
#[tauri::command]
pub fn open_db(state: State<'_, AppState>) -> Result<String> {
    let path = state.db_path();
    state.ensure_open()?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// 首次运行：从 cc-switch 导入
// ---------------------------------------------------------------------------

/// 首次运行判定：没有配置、或供应商列表为空都算。
#[tauri::command]
pub fn first_run() -> bool {
    crate::config::is_first_run()
}

/// 生成一份空样例配置（带随机 accessToken）。不导入时用户从这里开始。
#[tauri::command]
pub fn write_sample_config() -> Result<String> {
    let path = crate::config::config_path()?;
    crate::config::write_sample(&path).map_err(|e| crate::error::AppError::Config(e.to_string()))
}

/// 探测 cc-switch 数据库，返回"如果导入会发生什么"。
///
/// 纯只读：不写盘、不备份、不改任何状态。和执行用的
/// [`import_from_cc_switch`] 分开，是因为确认必须发生在写入之前——
/// 用户要先看见"将复制 N 个凭据"这句话，再决定要不要继续。
#[derive(Debug, Serialize)]
pub struct CcSwitchPreview {
    /// cc-switch 数据库路径
    pub db_path: String,
    /// 数据库不存在时为 false，前端据此隐藏导入按钮
    pub found: bool,
    /// 可导入的供应商
    pub providers: Vec<CcSwitchProviderPreview>,
    /// 搬不过来但用户该知道的（缺地址 / 缺凭据 / JSON 坏）
    pub skipped: Vec<String>,
    /// 这些供应商里哪些是第三方网关（导入后联网工具会失效）
    pub third_party_count: usize,
    /// 会一起搬过来的自定义文件夹，按 cc-switch 里的顺序。
    /// 非空时前端要说一句"你的分组也一起过来了"——用户排过的文件夹
    ///  silently 消失是最容易让人以为导入失败的一种
    pub folder_names: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CcSwitchProviderPreview {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_format: String,
}

#[tauri::command]
pub fn import_preview() -> Result<CcSwitchPreview> {
    let db = tern_gateway::ccswitch_import::default_cc_switch_db()
        .map_err(|e| crate::error::AppError::Config(e.to_string()))?;
    if !db.exists() {
        return Ok(missing_cc_switch(db));
    }

    let report = tern_gateway::ccswitch_import::import_providers(&db, "claude")
        .map_err(|e| crate::error::AppError::Config(e.to_string()))?;
    Ok(preview_of(&db, report))
}

/// 真正落盘。用户看过 [`import_preview`] 的结果并确认后才该调到这里。
#[tauri::command]
pub fn import_from_cc_switch(state: State<'_, AppState>) -> Result<CcSwitchPreview> {
    let db = tern_gateway::ccswitch_import::default_cc_switch_db()
        .map_err(|e| crate::error::AppError::Config(e.to_string()))?;
    if !db.exists() {
        return Ok(missing_cc_switch(db));
    }

    let report = tern_gateway::ccswitch_import::import_providers(&db, "claude")
        .map_err(|e| crate::error::AppError::Config(e.to_string()))?;

    // 写之前先备份：导入覆盖的是整个 providers 列表，用户手改过的东西不该无声消失
    let config_path = crate::config::config_path()?;
    if config_path.exists() {
        let backup = config_path.with_extension("json.bak");
        if let Err(error) = std::fs::copy(&config_path, &backup) {
            log::warn!("[tern-app] 备份 {config_path:?} 失败: {error}");
        }
    }

    // 保留用户已有的 accessToken / listen，只替换供应商列表
    let mut config = crate::config::load(&config_path)
        .unwrap_or_else(|_| tern_gateway::GatewayConfig::new(Vec::new()));
    config.providers = report.specs.clone();
    if config.access_token.as_deref().unwrap_or("").trim().is_empty() {
        config.access_token = Some(format!("tern-{}", uuid::Uuid::new_v4().simple()));
    }
    std::fs::write(&config_path, serde_json::to_string_pretty(&config)? + "\n")
        .map_err(|e| crate::error::AppError::Config(e.to_string()))?;

    // 分组数据在另一个文件，跟着落一份。顺序：先建注册表再把归属写进去，
    // 否则归属会指向一个还没登记的文件夹（界面上是个管不了的组）
    apply_imported_folders(&report.folder_names, &report.folder_assignments)?;

    // 导入后库路径可能变了，丢掉旧的只读连接，下次查询重新打开
    state.invalidate_db();
    log::info!(
        "[tern-app] 从 cc-switch 导入 {} 个供应商、{} 个文件夹到 {}",
        report.specs.len(),
        report.folder_names.len(),
        config_path.display()
    );

    // 网关还拿着旧配置在跑。不重起的话面板里显示的供应商列表是新的、
    // 实际路由用的是旧的，用户会以为导入失败。
    // 失败不阻断导入本身——文件已经落盘了，那才是要紧的
    if let Err(error) = crate::server::restart_after_config_change() {
        log::warn!("[tern-app] 导入后重起网关失败: {error}");
    }

    Ok(preview_of(&db, report))
}

/// cc-switch 不在时的预览结果。
fn missing_cc_switch(db: std::path::PathBuf) -> CcSwitchPreview {
    CcSwitchPreview {
        db_path: db.display().to_string(),
        found: false,
        providers: Vec::new(),
        skipped: Vec::new(),
        third_party_count: 0,
        folder_names: Vec::new(),
    }
}

/// 把 cc-switch 的文件夹搬进 tern 的 `folders.json`。
///
/// # 为什么是"合并"而不是"覆盖"
///
/// 用户在 tern 这边可能已经建过文件夹了。导入把整个 providers 数组换掉，
/// 但分组文件不该跟着被清零——那会同时丢掉 tern 侧的手工分组。
/// 所以：注册表里没有的名字才追加，归属按 id 覆盖（id 没变的保持原样）。
fn apply_imported_folders(
    folder_names: &[String],
    assignments: &[(String, String)],
) -> crate::error::Result<()> {
    if folder_names.is_empty() && assignments.is_empty() {
        return Ok(());
    }
    let mut file = crate::folders::read();
    // 顺序按 cc-switch 里的来：用户排过的文件夹不该被重排
    crate::folders::ensure_folder_names(&mut file.folders, folder_names);
    for (id, folder) in assignments {
        file.assignments
            .insert(id.trim().to_string(), folder.trim().to_string());
    }
    crate::folders::write(&file)
}

fn preview_of(
    db: &std::path::Path,
    report: tern_gateway::ccswitch_import::ImportReport,
) -> CcSwitchPreview {
    let providers = report
        .specs
        .iter()
        .map(|spec| CcSwitchProviderPreview {
            id: spec.id.clone(),
            name: spec.name.clone(),
            base_url: spec.effective_base_url(),
            api_format: spec.effective_api_format().to_string(),
        })
        .collect::<Vec<_>>();

    let third_party_count = report
        .specs
        .iter()
        .filter(|spec| {
            matches!(
                tern_gateway::assess(spec),
                tern_gateway::WebToolsSupport::ThirdParty
            )
        })
        .count();

    CcSwitchPreview {
        db_path: db.display().to_string(),
        found: true,
        providers,
        skipped: report
            .skipped
            .iter()
            .map(|(id, reason)| format!("{id}: {reason}"))
            .collect(),
        third_party_count,
        folder_names: report.folder_names,
    }
}


#[tauri::command]
pub fn panel_summary(state: State<'_, AppState>) -> Result<PanelDto> {
    state.with_db(|db| {
        db.with_conn(|conn| build_panel(conn, &state.db_path()))
    })
}

/// 面板首屏的全部数据。抽成不依赖 tauri 的普通函数，测试可以直接调用。
pub fn build_panel(conn: &rusqlite::Connection, db_path: &str) -> Result<PanelDto> {
    {
        let today = local_days_ago(0);
        let yesterday = local_days_ago(1);

        let summary = |day: &str| -> rusqlite::Result<SummaryDto> {
            conn.query_row(
                &format!("SELECT {SUMMARY_COLUMNS} FROM daily WHERE day = ?1"),
                [day],
                summary_from_row,
            )
        };
        let today_summary = summary(&today)?;
        let yesterday_summary = summary(&yesterday)?;
        // 有数据才谈"今日"，否则前端显示首次引导
        let first_run = conn.query_row::<i64, _, _>(
            "SELECT COALESCE(SUM(requests), 0) FROM daily",
            [],
            |row| row.get(0),
        )? == 0;

        let unpriced_models = {
            let mut stmt = conn.prepare(
                "SELECT COALESCE(response_model, upstream_model, client_model) AS model,
                        COUNT(*), SUM(fresh_input + output + cache_read + cache_write)
                 FROM requests
                 WHERE day = ?1 AND has_usage = 1 AND cost_nano IS NULL
                 GROUP BY model ORDER BY 3 DESC LIMIT 8",
            )?;
            let rows = stmt.query_map([&today], |row| {
                Ok(UnpricedDto {
                    model: row.get(0)?,
                    requests: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                    tokens: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let failures = {
            let mut stmt = conn.prepare(
                // 与 tern-store 的 failures() 相同：按（原因, 供应商, 状态码）聚类，
                // 取每组最近一条错误摘要做样本
                "SELECT COALESCE(error_kind, 'unknown'), provider_id, status, COUNT(*),
                        (SELECT r2.error_message FROM requests r2
                         WHERE r2.day = ?1 AND r2.outcome = 'failed'
                           AND COALESCE(r2.error_kind, 'unknown') = COALESCE(r.error_kind, 'unknown')
                           AND r2.provider_id IS r.provider_id AND r2.status = r.status
                         ORDER BY r2.started_at DESC LIMIT 1)
                 FROM requests r
                 WHERE day = ?1 AND outcome = 'failed'
                 GROUP BY 1, 2, 3 ORDER BY 4 DESC LIMIT 6",
            )?;
            let rows = stmt.query_map([&today], |row| {
                Ok(FailureDto {
                    error_kind: row.get(0)?,
                    provider_id: row.get(1)?,
                    status: u16::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                    count: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                    sample: row.get(4)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let recent = {
            let mut stmt = conn.prepare(
                "SELECT started_at, client, provider_id, client_model, response_model,
                        role, status, outcome, error_kind,
                        fresh_input, output, cache_read, cache_write, cost_nano, duration_ms
                 FROM requests ORDER BY started_at DESC, id DESC LIMIT 12",
            )?;
            let u = |row: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<u64> {
                Ok(u64::try_from(row.get::<_, i64>(i)?).unwrap_or(0))
            };
            let rows = stmt.query_map([], |row| {
                Ok(RecentDto {
                    started_at_ms: row.get(0)?,
                    client: row.get(1)?,
                    provider_id: row.get(2)?,
                    client_model: row.get(3)?,
                    response_model: row.get(4)?,
                    role: row.get(5)?,
                    status: u16::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
                    outcome: row.get(7)?,
                    error_kind: row.get(8)?,
                    fresh_input: u(row, 9)?,
                    output: u(row, 10)?,
                    cache_read: u(row, 11)?,
                    cache_write: u(row, 12)?,
                    cost: row.get::<_, Option<i64>>(13)?.map(nano_to_usd_string),
                    duration_ms: u(row, 14)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        Ok(PanelDto {
            db_path: db_path.to_string(),
            first_run,
            today: today_summary,
            yesterday: yesterday_summary,
            unpriced_models,
            failures,
            recent,
        })
    }
}

// ---------------------------------------------------------------------------
// 二级视图（ROADMAP T+3）：趋势 / 花在哪 / 会话 / 模型流向
// ---------------------------------------------------------------------------

/// 日期范围。`days = 0` 表示今天一天。
///
/// 日期由 Rust 侧算而不是前端传：本地时区的"今天"得和写入时算 `day` 列的
/// `tern_store::local_day` 同一口径。前端传字符串的话，换时区的人会看到一个
/// 空窗口，而库明明有数据。
#[derive(Debug, Serialize, Clone)]
pub struct RangeDto {
    pub from: String,
    pub to: String,
    pub days: u32,
}

fn range_of(days: u32) -> tern_store::DayRange {
    if days <= 1 {
        tern_store::DayRange::today()
    } else {
        tern_store::DayRange::last_days(days)
    }
}

/// 在 Store 上执行查询。
///
/// 网关在跑（`set_shared_store` 存了一份）就直接用它——同一份 Store，口径不会
/// 分叉；没在跑才另开只读连接。只读打开见 `tern_store::Store::open_readonly`：
/// 它不建库、不迁移，所以库不存在时必须由这里报错，而不是造一个空库。
fn with_store<T>(
    state: &AppState,
    f: impl FnOnce(&tern_store::Store) -> std::result::Result<T, crate::error::AppError>,
) -> Result<T> {
    if let Some(store) = state.shared_store() {
        return f(&store);
    }
    let path = crate::db::resolve_db_path()?;
    if !path.exists() {
        return Err(crate::error::AppError::NoConfigDir);
    }
    // `?` 走 AppError::from(StoreError)：Sqlite 变体保留原错，其余折成 Store。
    // 但路径要补上——库打不开时用户最想知道的是"它在找哪个文件"
    let store = tern_store::Store::open_readonly(&path).map_err(|error| {
        let with_path = match &error {
            tern_store::StoreError::Sqlite(_) => format!(
                "打不开用量数据库 {}：{error}",
                path.display()
            ),
            other => other.to_string(),
        };
        crate::error::AppError::Store(with_path)
    })?;
    f(&store)
}

/// 花费趋势。`dim` 是 `provider` / `model` / `role` / `client` / `day`，
/// 与 tern-store 的 `Breakdown` 同名，解析不了按天走。
#[tauri::command]
pub fn panel_trend(state: State<'_, AppState>, days: u32, dim: String) -> Result<Vec<TrendDto>> {
    with_store(state.inner(), |store| {
        let by = match dim.as_str() {
            "provider" => tern_store::Breakdown::Provider,
            "model" => tern_store::Breakdown::Model,
            "role" => tern_store::Breakdown::Role,
            "client" => tern_store::Breakdown::Client,
            // 不认识的值按天走：报错会让整张图消失，而按天总归是能看的
            _ => tern_store::Breakdown::Day,
        };
        Ok(store
            .trend(&range_of(days), by)?
            .into_iter()
            .map(|point| TrendDto {
                day: point.day,
                key: point.key,
                summary: summary_to_dto(&point.summary),
            })
            .collect())
    })
}

/// 花在哪：按维度拆的占比。和趋势用同一个 [`Breakdown`]，口径才对得上。
#[tauri::command]
pub fn panel_breakdown(
    state: State<'_, AppState>,
    days: u32,
    dim: String,
) -> Result<Vec<BreakdownDto>> {
    with_store(state.inner(), |store| {
        let by = match dim.as_str() {
            "provider" => tern_store::Breakdown::Provider,
            "model" => tern_store::Breakdown::Model,
            "role" => tern_store::Breakdown::Role,
            "client" => tern_store::Breakdown::Client,
            _ => tern_store::Breakdown::Day,
        };
        Ok(store
            .breakdown(&range_of(days), by)?
            .into_iter()
            .map(|row| BreakdownDto {
                key: row.key,
                summary: summary_to_dto(&row.summary),
            })
            .collect())
    })
}

/// 会话视图：按 `session_id` 聚合。"这次重构花了多少"由它回答。
#[tauri::command]
pub fn panel_sessions(state: State<'_, AppState>, days: u32, limit: usize) -> Result<Vec<SessionDto>> {
    with_store(state.inner(), |store| {
        Ok(store
            .sessions(&range_of(days), limit)?
            .into_iter()
            .map(|row| SessionDto {
                session_id: row.session_id,
                client: row.client,
                started_at_ms: row.started_at_ms,
                ended_at_ms: row.ended_at_ms,
                roles: row.roles,
                summary: summary_to_dto(&row.summary),
            })
            .collect())
    })
}

/// 模型流向：客户端模型 → 实际模型。
///
/// 这是"谁在花钱"的直接答案：子代理请求的 sonnet 全被供应商送去 Opus 这件事，
/// 只有把它画出来才看得见。
#[tauri::command]
pub fn panel_model_flow(state: State<'_, AppState>, days: u32) -> Result<Vec<ModelFlowDto>> {
    with_store(state.inner(), |store| {
        Ok(store
            .model_flow(&range_of(days))?
            .into_iter()
            .map(|row| ModelFlowDto {
                client_model: row.client_model,
                response_model: row.response_model,
                summary: summary_to_dto(&row.summary),
            })
            .collect())
    })
}

#[derive(Debug, Serialize)]
pub struct TrendDto {
    pub day: String,
    pub key: String,
    pub summary: SummaryDto,
}

#[derive(Debug, Serialize)]
pub struct BreakdownDto {
    pub key: String,
    pub summary: SummaryDto,
}

#[derive(Debug, Serialize)]
pub struct SessionDto {
    pub session_id: String,
    pub client: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub roles: Vec<String>,
    pub summary: SummaryDto,
}

#[derive(Debug, Serialize)]
pub struct ModelFlowDto {
    pub client_model: String,
    pub response_model: Option<String>,
    pub summary: SummaryDto,
}

/// `tern_store::Summary` → DTO。成本转成十进制字符串，理由同 [`SummaryDto`]。
fn summary_to_dto(summary: &tern_store::Summary) -> SummaryDto {
    SummaryDto {
        requests: summary.requests,
        failures: summary.failures,
        aborted: summary.aborted,
        fresh_input: summary.fresh_input,
        output: summary.output,
        cache_read: summary.cache_read,
        cache_write: summary.cache_write,
        // 与 nano_to_usd 同一套精度：trim 尾零再补分位，避免 "18.470000000"
        cost: decimal_string(&summary.cost),
        cache_savings: decimal_string(&summary.cache_savings),
        unpriced: summary.unpriced,
    }
}

fn decimal_string(value: &rust_decimal::Decimal) -> String {
    let text = value.normalize().to_string();
    if text.contains('.') {
        text
    } else {
        // 前端按分位显示，整数也要有小数部分才好排版
        format!("{text}.0")
    }
}

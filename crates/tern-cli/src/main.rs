//! tern 命令行：`tern init` 生成样例配置，`tern serve` 启动网关并记录用量，
//! `tern usage` / `tern price` 查看用量、维护价格。

mod args;
mod config;
mod report;
mod table;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tern_gateway::{Gateway, GatewayConfig, Outcome, ProviderAuth};
use tern_store::{Inserted, ModelPrice, PriceSource, Store};

use crate::args::{Command, Paths, PriceAction};

const DB_FILE_NAME: &str = "usage.db";

fn main() -> ExitCode {
    // 默认 info；RUST_LOG 可覆盖，如 RUST_LOG=tern_gateway=debug
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_target(false)
        .init();

    let command = match args::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("错误: {error}\n\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Serve { paths, listen } => {
            let config = resolve_config(&paths)?;
            serve(&config, &resolve_db(&paths, &config), listen)
        }
        Command::Init { paths, force } => init(&resolve_config(&paths)?, force),
        Command::Check { paths } => check(&resolve_config(&paths)?),
        Command::Usage {
            paths,
            days,
            by,
            recent,
        } => {
            let store = open_existing_store(&paths)?;
            report::usage(&store, days, by.as_deref(), recent)
        }
        Command::Price { paths, action } => price(&paths, action),
        Command::Help => {
            println!("{}", args::USAGE);
            Ok(())
        }
        Command::Version => {
            println!("tern {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

/// `--config` 优先，其次环境变量 `TERN_CONFIG`，最后是系统配置目录
fn resolve_config(paths: &Paths) -> Result<PathBuf> {
    if let Some(path) = &paths.config {
        return Ok(path.clone());
    }
    match std::env::var_os("TERN_CONFIG").filter(|v| !v.is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => config::default_path(),
    }
}

/// `--db` 优先，其次环境变量 `TERN_DB`，最后是配置文件同目录的 `usage.db`
fn resolve_db(paths: &Paths, config: &Path) -> PathBuf {
    if let Some(path) = &paths.db {
        return path.clone();
    }
    if let Some(path) = std::env::var_os("TERN_DB").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    config
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(DB_FILE_NAME)
}

/// 查询类命令：数据库不存在时提示先运行 serve，而不是建一个空库
fn open_existing_store(paths: &Paths) -> Result<Store> {
    let db = resolve_db(paths, &resolve_config(paths)?);
    if !db.exists() {
        bail!(
            "用量数据库 {} 不存在。先用 `tern serve` 跑一段时间，或用 --db 指定位置",
            db.display()
        );
    }
    Store::open(&db).with_context(|| format!("打开用量数据库 {} 失败", db.display()))
}

fn serve(path: &Path, db: &Path, listen: Option<SocketAddr>) -> Result<()> {
    let mut config = config::load(path)?;
    if let Some(listen) = listen {
        config.listen = listen;
    }
    log::info!("[tern] 配置 {}", path.display());
    for warning in config::warnings(&config) {
        log::warn!("[tern] {warning}");
    }
    // 联网工具可用性：不走网关、网关修不了，只能在启动时讲清楚
    for spec in tern_gateway::third_party_providers(&config.providers) {
        log::warn!("[tern] {}", tern_gateway::warning_for(spec));
    }
    log::info!("[tern] 供应商: {}", provider_ids(&config));

    let store =
        Arc::new(Store::open(db).with_context(|| format!("打开用量数据库 {} 失败", db.display()))?);
    let multipliers = config.providers.iter().filter_map(|spec| {
        spec.cost_multiplier
            .as_deref()
            .map(|value| (spec.id.as_str(), value))
    });
    for error in store.set_multipliers(multipliers) {
        log::warn!("[tern] {error}");
    }
    log::info!(
        "[tern] 用量记录到 {}（{} 条价格）",
        db.display(),
        store.prices().len()
    );
    let (recorder, recorder_handle) = tern_store::spawn_recorder(store, log_usage);

    let gateway = Gateway::new(config)
        .context("配置无效")?
        .with_usage_sink(recorder);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?;
    let result = runtime.block_on(async move {
        gateway
            .serve(async {
                if let Err(e) = tokio::signal::ctrl_c().await {
                    log::error!("[tern] 监听 Ctrl+C 失败: {e}");
                    // 拿不到信号就一直运行，交给用户关窗口 / 杀进程
                    std::future::pending::<()>().await;
                }
                log::info!("[tern] 收到 Ctrl+C，等待进行中的请求结束");
            })
            .await
            .context("网关启动失败（端口被占用时可用 --listen 换一个）")
    });
    // 先停运行时：进行中的流被丢弃时会补记 aborted，然后再让写入线程把队列写完
    drop(runtime);
    recorder_handle.stop();
    result
}

/// 每条请求一行摘要
fn log_usage(event: &tern_gateway::UsageEvent, inserted: &Inserted) {
    let provider = event.provider_id.as_deref().unwrap_or("-");
    let model = event
        .response_model
        .as_deref()
        .or(event.upstream_model.as_deref())
        .unwrap_or(&event.client_model);
    let role = event.role.as_str();
    match (event.outcome, inserted) {
        (_, Inserted::Duplicate) => {}
        (Outcome::Failed, _) => log::warn!(
            "[usage] ✗ {provider} {model} ({role}) HTTP {} {} {}ms",
            event.status,
            event.error_kind.map(|k| k.as_str()).unwrap_or("-"),
            event.duration_ms
        ),
        (outcome, Inserted::Row { cost, .. }) => {
            let tokens = event.tokens.unwrap_or_default();
            let cost = match (cost, event.tokens) {
                (Some(cost), _) => report::money(*cost),
                (None, Some(_)) => "未定价".to_string(),
                (None, None) => "-".to_string(),
            };
            log::info!(
                "[usage] {}{provider} {model} ({role}) in {} out {} cache {} {cost} {}ms",
                if outcome == Outcome::Aborted {
                    "中断 "
                } else {
                    ""
                },
                report::tokens(tokens.fresh_input),
                report::tokens(tokens.output),
                report::tokens(tokens.cache_read),
                event.duration_ms
            );
        }
    }
}

fn price(paths: &Paths, action: PriceAction) -> Result<()> {
    let db = resolve_db(paths, &resolve_config(paths)?);
    let store =
        Store::open(&db).with_context(|| format!("打开用量数据库 {} 失败", db.display()))?;
    match action {
        PriceAction::List { model } => report::prices(&store, model.as_deref()),
        PriceAction::Set { model, mut values } => {
            values.resize(4, "0".to_string());
            let values = [&values[0], &values[1], &values[2], &values[3]].map(String::as_str);
            let price = ModelPrice::parse(&model, "", values, PriceSource::User)?;
            let repriced = store.upsert_prices(std::slice::from_ref(&price))?;
            println!(
                "已设置 {}：输入 {} / 输出 {} / 缓存读 {} / 缓存写 {}（美元 / 百万 token）",
                price.model_id, price.input, price.output, price.cache_read, price.cache_write
            );
            if repriced > 0 {
                println!("给 {repriced} 条之前未定价的记录补上了花费");
            }
        }
        PriceAction::Remove { model } => {
            if store.delete_user_price(&model)? {
                println!("已删除 {model} 的手填价格");
            } else {
                println!("{model} 没有手填价格（内置价格不能删除，可以用 price set 覆盖）");
            }
        }
        PriceAction::Sync { file } => sync_models_dev(&store, file.as_deref())?,
    }
    Ok(())
}

/// 同名模型优先取这些官方供应商在 models.dev 上的价格
const MODELS_DEV_PREFERRED: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "deepseek",
    "moonshotai",
    "moonshotai-cn",
    "zhipuai",
    "zai",
    "alibaba",
    "xai",
    "minimax",
    "minimax-cn",
    "stepfun",
    "xiaomi",
    "mistral",
];

fn sync_models_dev(store: &Store, file: Option<&Path>) -> Result<()> {
    let json = match file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("读取 {} 失败", path.display()))?,
        None => {
            println!("下载 {} …", tern_store::models_dev::API_URL);
            download(tern_store::models_dev::API_URL)
                .context("下载失败。网络不通时可以先用浏览器保存 api.json，再用 --file 导入")?
        }
    };
    let prices = tern_store::models_dev::parse(&json, MODELS_DEV_PREFERRED)
        .context("models.dev 返回的不是预期的 JSON")?;
    if prices.is_empty() {
        bail!("models.dev 数据里没有可用的文本模型价格");
    }
    let repriced = store.upsert_prices(&prices)?;
    store.set_meta("models_dev_synced_at", &chrono::Utc::now().to_rfc3339())?;
    println!("同步了 {} 个模型的价格", prices.len());
    if repriced > 0 {
        println!("给 {repriced} 条之前未定价的记录补上了花费");
    }
    println!("手填的价格优先级更高，不会被覆盖");
    Ok(())
}

/// 跟随系统代理（HTTPS_PROXY 等），与网关的默认行为一致
fn download(url: &str) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let response = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(60))
            .build()?
            .get(url)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.text().await?)
    })
}

fn init(path: &Path, force: bool) -> Result<()> {
    let token = config::write_sample(path, force)?;
    let listen = config::sample(&token)["listen"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    println!("已生成 {}", path.display());
    println!();
    println!("下一步：");
    println!("  1. 把文件里的 {} 换成真实 key", config::PLACEHOLDER_KEY);
    println!("  2. tern serve");
    println!();
    println!("Claude Code（PowerShell）：");
    println!("  $env:ANTHROPIC_BASE_URL = \"http://{listen}\"");
    println!("  $env:ANTHROPIC_AUTH_TOKEN = \"{token}\"");
    println!("  $env:ANTHROPIC_MODEL = \"deepseek/deepseek-v4-pro\"");
    println!();
    println!("Codex（~/.codex/config.toml，并设置环境变量 TERN_ACCESS_TOKEN={token}）：");
    println!("  model = \"kimi/kimi-k3\"");
    println!("  model_provider = \"tern\"");
    println!("  [model_providers.tern]");
    println!("  name = \"tern\"");
    println!("  base_url = \"http://{listen}/v1\"");
    println!("  wire_api = \"responses\"");
    println!("  env_key = \"TERN_ACCESS_TOKEN\"");
    Ok(())
}

fn check(path: &Path) -> Result<()> {
    let config = config::load(path)?;
    // 联网工具判定要在 config 被 Gateway::new 移走之前算完
    let web_tool_warnings: Vec<String> = tern_gateway::third_party_providers(&config.providers)
        .into_iter()
        .map(tern_gateway::warning_for)
        .collect();
    let warnings = config::warnings(&config);
    let listen = config.listen;
    let default_provider = config.default_provider.clone();
    let rows: Vec<[String; 4]> = config
        .providers
        .iter()
        .map(|spec| {
            [
                spec.id.clone(),
                spec.effective_api_format().to_string(),
                auth_kind(&spec.auth).to_string(),
                spec.effective_base_url(),
            ]
        })
        .collect();
    // 走一遍网关自己的校验（id 重复、默认供应商不存在、代理地址无效等）
    Gateway::new(config).context("配置无效")?;

    println!("配置 {}", path.display());
    println!("监听 http://{listen}");
    println!(
        "默认供应商 {}",
        default_provider
            .as_deref()
            .unwrap_or("（未设置，裸模型名会被拒绝）")
    );
    println!();
    // 表头用英文：中文字符显示宽度是 2，按字符数对齐会错位
    print_table(&["id", "format", "auth", "url"], &rows);
    if !warnings.is_empty() {
        println!();
        for warning in &warnings {
            println!("警告: {warning}");
        }
    }
    if !web_tool_warnings.is_empty() {
        println!();
        for warning in &web_tool_warnings {
            println!("联网工具: {warning}");
        }
    }
    Ok(())
}

fn provider_ids(config: &GatewayConfig) -> String {
    if config.providers.is_empty() {
        return "（无）".to_string();
    }
    config
        .providers
        .iter()
        .map(|spec| spec.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn auth_kind(auth: &ProviderAuth) -> &'static str {
    match auth {
        ProviderAuth::None => "none",
        ProviderAuth::ApiKey { .. } => "api_key",
        ProviderAuth::GoogleOauth { .. } => "google_oauth",
        ProviderAuth::GithubCopilot { .. } => "github_copilot",
        ProviderAuth::CodexOauth { .. } => "codex_oauth",
        ProviderAuth::XaiOauth { .. } => "xai_oauth",
    }
}

/// 简单的左对齐表格；列宽按字符数算，单元格基本是 ASCII
fn print_table<const N: usize>(header: &[&str; N], rows: &[[String; N]]) {
    let mut widths = header.map(|h| h.chars().count());
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let padded: Vec<String> = cells
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        println!("{}", padded.join("  ").trim_end());
    };
    line(header.to_vec());
    for row in rows {
        line(row.iter().map(String::as_str).collect());
    }
}

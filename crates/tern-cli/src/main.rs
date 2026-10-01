//! tern 命令行：`tern init` 生成样例配置，`tern serve` 启动网关。

mod args;
mod config;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use tern_gateway::{Gateway, GatewayConfig, ProviderAuth};

use crate::args::Command;

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
        Command::Serve { config, listen } => serve(&resolve_config(config)?, listen),
        Command::Init { config, force } => init(&resolve_config(config)?, force),
        Command::Check { config } => check(&resolve_config(config)?),
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
fn resolve_config(arg: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = arg {
        return Ok(path);
    }
    match std::env::var_os("TERN_CONFIG").filter(|v| !v.is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => config::default_path(),
    }
}

fn serve(path: &Path, listen: Option<SocketAddr>) -> Result<()> {
    let mut config = config::load(path)?;
    if let Some(listen) = listen {
        config.listen = listen;
    }
    log::info!("[tern] 配置 {}", path.display());
    for warning in config::warnings(&config) {
        log::warn!("[tern] {warning}");
    }
    log::info!("[tern] 供应商: {}", provider_ids(&config));

    let gateway = Gateway::new(config).context("配置无效")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?;
    runtime.block_on(async move {
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

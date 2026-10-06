//! 错误类型：序列化后交给前端显示。前端拿到的每条 message 都是人能看懂的一句话。

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("打不开用量数据库 {path}: {source}")]
    Open {
        path: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("查询失败: {source}")]
    Query {
        #[source]
        source: rusqlite::Error,
    },
    #[error("找不到系统配置目录，请设置环境变量 TERN_DB 指向 usage.db")]
    NoConfigDir,
    /// 端口被占用。单独一个变体是因为这是启动失败里最高频的一种，
    /// 要给出"换端口"的下一步，而不是把 bind 错误原样抛出去。
    #[error(
        "端口 {listen} 已被占用。另一个 tern 或 cc-switch 可能正在运行；\n\
         关掉它，或在配置里把 listen 改成别的端口"
    )]
    PortInUse { listen: String },
    #[error("存储失败: {0}")]
    Store(String),
    #[error("配置无效: {0}")]
    Config(String),
}

// Tauri 要求 invoke 的错误实现 Serialize。这里手写而不是 derive：
// derive(Serialize) 会连 #[source] 里的 rusqlite::Error 一起要求实现，
// 而它没有实现。序列化成一句人能看懂的话就够前端展示了。
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

impl From<rusqlite::Error> for AppError {
    fn from(source: rusqlite::Error) -> Self {
        AppError::Query { source }
    }
}

// 配置层用的是 anyhow（带上下文链），收进来时只保留最终那句话：
// 前端只需要知道"去哪改"，不需要看内部调用链。
impl From<anyhow::Error> for AppError {
    fn from(source: anyhow::Error) -> Self {
        AppError::Config(source.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(source: serde_json::Error) -> Self {
        AppError::Config(source.to_string())
    }
}

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

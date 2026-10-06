//! 只读数据库连接。复刻 `Store::open` 里与查询相关的部分，去掉一切写方行为。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;

use crate::AppError;

/// 面板用的只读连接句柄。用 `Mutex` 而不是连接池：单窗口、查询都是毫秒级聚合。
pub(crate) struct ReadonlyDb {
    conn: Mutex<Connection>,
}

impl ReadonlyDb {
    pub(crate) fn open(path: &Path) -> Result<Self, AppError> {
        // 注意这里**没有** create_dir_all / migrate / journal_mode。见模块注释。
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|source| AppError::Open {
            path: path.display().to_string(),
            source,
        })?;
        // WAL 库的 reader 也要能跟上 checkpoint；只读连接同样受 busy 影响
        conn.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(|source| AppError::Query { source })?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T, AppError>) -> Result<T, AppError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        f(&conn)
    }
}

/// 与 `tern serve` 保持一致的默认位置：配置文件同目录的 `usage.db`。
/// 环境变量 `TERN_DB` 优先，其次 `TERN_CONFIG` 推断，最后 `%APPDATA%\tern\usage.db`。
pub(crate) fn resolve_db_path() -> Result<PathBuf, AppError> {
    if let Some(path) = std::env::var_os("TERN_DB").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(config) = std::env::var_os("TERN_CONFIG").filter(|v| !v.is_empty()) {
        if let Some(dir) = Path::new(&config).parent().filter(|d| !d.as_os_str().is_empty()) {
            return Ok(dir.join("usage.db"));
        }
    }
    let dir = dirs::config_dir().ok_or(AppError::NoConfigDir)?;
    Ok(dir.join("tern").join("usage.db"))
}

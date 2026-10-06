//! tern-app 的 Rust 侧：只读打开网关的 `usage.db`，把聚合结果交给前端。
//!
//! 这里刻意**不在进程内跑网关**：网关继续由 `tern serve` 提供，面板只是它的
//! 观察窗口。好处是面板崩了不影响正在进行的请求，也不需要一套启停状态机。
//!
//! 同样刻意**不用 `tern_store::Store`**：那个入口会 `create_dir_all`、执行迁移、
//! 把 journal_mode 设成 WAL——都是写方行为。面板可能和网关同时开着，
//! 用只读连接才不会去抢它的写锁。

pub mod commands;
mod db;
mod error;

pub use error::{AppError, Result};

use std::sync::Mutex;

use db::ReadonlyDb;

/// 面板运行期状态。`None` 表示还没成功打开数据库（库不存在，或用户还没跑过 serve）。
pub struct AppState {
    db: Mutex<Option<ReadonlyDb>>,
    /// 上次尝试打开时的提示，库里没数据时也要告诉用户为什么
    notice: Mutex<Option<String>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            db: Mutex::new(None),
            notice: Mutex::new(None),
        }
    }
}

impl AppState {
    /// 尝试打开数据库。已经打开过就直接复用；打不开时把原因留在这里。
    pub fn ensure_open(&self) -> std::result::Result<(), AppError> {
        {
            let guard = self.db.lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_some() {
                return Ok(());
            }
        }
        let path = db::resolve_db_path()?;
        match ReadonlyDb::open(&path) {
            Ok(handle) => {
                *self.db.lock().unwrap_or_else(|p| p.into_inner()) = Some(handle);
                *self.notice.lock().unwrap_or_else(|p| p.into_inner()) = None;
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                *self.notice.lock().unwrap_or_else(|p| p.into_inner()) = Some(message);
                Err(error)
            }
        }
    }

    pub fn notice(&self) -> Option<String> {
        self.notice.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn db_path(&self) -> String {
        db::resolve_db_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "（无法确定）".to_string())
    }

    /// 在已打开的连接上执行查询。只给 `commands` 用，不对外公开。
    pub(crate) fn with_db<T>(
        &self,
        f: impl FnOnce(&db::ReadonlyDb) -> std::result::Result<T, AppError>,
    ) -> std::result::Result<T, AppError> {
        self.ensure_open()?;
        let guard = self.db.lock().unwrap_or_else(|p| p.into_inner());
        let handle = guard
            .as_ref()
            .ok_or_else(|| error::AppError::NoConfigDir)?;
        f(handle)
    }
}

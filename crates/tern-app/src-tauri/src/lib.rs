//! tern-app 的 Rust 侧：内嵌网关 + 只读面板。
//!
//! # 演化过的定位
//!
//! 最早这里**刻意不在进程内跑网关**，面板只是 `tern serve` 的观察窗口。那个判断对
//! "只做面板"成立，但对"点 exe 就用"不成立：用户还得先开一个终端跑 serve，两步都不
//! 符合预期，而且网关死了面板还在显示旧数据更难排查。
//!
//! 所以现在由应用自己持有网关（`server` 模块）。代价是要管生命周期，收益是用户双击
//! exe 就行。这个取舍对最终用户明显更值。
//!
//! # 数据库的两条路径
//!
//! - `server::server_start` 用 `tern_store::Store`（写方：迁移、WAL、记账）
//! - `commands` 用只读连接查面板
//!
//! 同一个库、两种打开方式，靠 WAL 并存。面板读的时候不阻塞网关写。

pub mod agent;
pub mod commands;
pub mod config;
mod db;
mod error;
pub mod permissions;
pub mod server;
pub mod wire;

pub use error::{AppError, Result};

use std::sync::{Arc, Mutex};

use db::ReadonlyDb;
use tern_store::Store;

/// 应用运行期状态。
pub struct AppState {
    /// 面板查询用的只读连接
    db: Mutex<Option<ReadonlyDb>>,
    /// 上次尝试打开时的提示，库里没数据时也要告诉用户为什么
    notice: Mutex<Option<String>>,
    /// 内嵌网关持有的 Store。面板读它，服务层写它，两端同一份。
    store: Mutex<Option<Arc<Store>>>,
    /// 网关实际使用的库路径。Store 自己不暴露路径，在这里记一份供显示。
    store_path: Mutex<Option<std::path::PathBuf>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            db: Mutex::new(None),
            notice: Mutex::new(None),
            store: Mutex::new(None),
            store_path: Mutex::new(None),
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

    /// 丢掉已打开的只读连接，下次查询重新打开。
    ///
    /// 导入或改过配置后调用：库路径可能变了，旧的连接会读到旧数据。
    pub fn invalidate_db(&self) {
        *self.db.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.store.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.store_path.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    pub fn notice(&self) -> Option<String> {
        self.notice.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn db_path(&self) -> String {
        // 内嵌网关起过的话用它的实际路径，别让 TERN_DB 之类的覆盖造成歧义
        if let Some(path) = self
            .store_path
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return path.display().to_string();
        }
        db::resolve_db_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "（无法确定）".to_string())
    }

    /// 服务层把 Store 存进来，面板查询改走它：两端同一份，口径不会分叉。
    pub fn set_shared_store(&self, store: Arc<Store>, db_path: std::path::PathBuf) {
        *self.store.lock().unwrap_or_else(|p| p.into_inner()) = Some(store);
        *self.store_path.lock().unwrap_or_else(|p| p.into_inner()) = Some(db_path);
    }

    pub fn shared_store(&self) -> Option<Arc<Store>> {
        self.store.lock().unwrap_or_else(|p| p.into_inner()).clone()
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

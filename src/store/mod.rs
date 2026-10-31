//! 存储域（Store）：SQLite 持久化、去重、搜索、容量治理、blob 文件库。
//!
//! 由 store teammate 实现；对外仅暴露：[`StoreHandle`] + [`StoreError`]。
//! 线程模型：`StoreHandle::spawn` 启动一个独占 SQLite 连接的线程，
//! 通过 [`StoreCmd`] 接收命令、通过 `Sender<Event>` 回发事件。

use crate::model::{Event, Settings, StoreCmd};
use eframe::egui;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub struct StoreHandle {
    tx: Sender<StoreCmd>,
}

impl StoreHandle {
    /// 启动存储线程（阻塞到数据库就绪）。
    pub fn spawn(
        _events: Sender<Event>,
        _settings: Arc<Mutex<Settings>>,
        _ctx: egui::Context,
    ) -> Result<Self, StoreError> {
        todo!("store 域由 store teammate 实现")
    }

    pub fn cmd_tx(&self) -> Sender<StoreCmd> {
        self.tx.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("SQLite 错误: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

//! Win32 系统域（Sys）：隐藏消息窗口、剪贴板监听、全局快捷键、Win+V 钩子、
//! 自动粘贴、托盘、开机自启、单实例。
//!
//! 由 win32 teammate 实现；对外仅暴露 [`SysHandle`] + [`SysError`]。
//! 计划中的子模块：`thread.rs`（消息循环/线程入口）、`paste.rs`、`hotkey.rs`、
//! `winv.rs`（Win+V 接管）、`tray.rs`、`autostart.rs`、`util.rs`。

use crate::model::{Event, Settings, SysCmd};
use eframe::egui;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub struct SysHandle {
    tx: Sender<SysCmd>,
}

impl SysHandle {
    /// 启动 Sys 线程（阻塞到隐藏窗口/热键就绪）。
    pub fn spawn(
        _events: Sender<Event>,
        _settings: Arc<Mutex<Settings>>,
        _ctx: egui::Context,
    ) -> Result<Self, SysError> {
        todo!("sys 域由 win32 teammate 实现")
    }

    pub fn cmd_tx(&self) -> Sender<SysCmd> {
        self.tx.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SysError {
    #[error("{0}")]
    Msg(String),
    #[error("Win32 错误: {0}")]
    Win32(String),
}

pub type SysResult<T> = Result<T, SysError>;

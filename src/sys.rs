//! Win32 系统域（Sys）：隐藏消息窗口、剪贴板监听、全局快捷键、Win+V 接管、
//! 自动粘贴、托盘、开机自启。
//!
//! 对外只暴露 [`SysHandle`] + [`SysError`] + 少量诊断工具（`foreground_app_name`/`is_elevated`）。
//! 线程模型：`SysHandle::spawn` 启动唯一的 Win32 消息循环线程（`thread.rs`），
//! 通过 `Sender<SysCmd>` 接收命令、通过 `Sender<Event>` 回发事件给 UI 线程；
//! 线程内失败一律降级为 `Event::Toast`/状态事件，绝不 panic（主窗口默认隐藏，
//! Sys 挂掉用户就无法唤出界面，因此启动阶段容错优先）。
//!
//! P0 结论见 `docs/P0-FINDINGS.md`：L1 `RegisterHotKey(Win+V)` 必失败（1409），
//! Win+V 走 L2 低级键盘钩子 + 开始菜单补偿；备用快捷键注册失败按降级链处理。
#![allow(dead_code)] // Sys 域内部工具：部分函数仅供测试/诊断/后续接入使用


pub mod autostart;
pub mod hotkey;
pub mod paste;
mod state;
pub mod shell;
pub mod thread;
pub mod tray;
pub mod util;

#[allow(unused_imports)]
pub use paste::PasteError;
#[allow(unused_imports)]
pub use thread::{activate_existing_instance, find_existing_instance};
#[allow(unused_imports)]
pub use util::{foreground_app_name, is_elevated};

use crate::logging;
use crate::model::{Event, Settings, SysCmd};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Sys 域运行状态快照（供 `--diagnose` 使用；不参与业务逻辑）。
#[derive(Clone, Debug, Default)]
pub struct SysDiag {
    /// 隐藏消息窗口句柄。
    pub hidden_hwnd: isize,
    /// `AddClipboardFormatListener` 是否成功。
    pub clipboard_listener_ok: bool,
    /// 用户配置的备用快捷键。
    pub hotkey_requested: Option<crate::model::HotkeyChord>,
    /// 实际生效的备用快捷键（可能因冲突降级；用于 `--diagnose`/设置页提示）。
    pub hotkey_effective: Option<crate::model::HotkeyChord>,
    pub hotkey_ok: bool,
    pub hotkey_note: Option<String>,
    /// Win+V 接管状态。
    /// 托盘是否创建成功。
    pub tray_ok: bool,
    /// 当前进程是否提权。
    pub elevated: bool,
}

/// Sys 线程句柄（drop 时发送 `Shutdown` 并等待线程退出，含 3s 超时保护）。
pub struct SysHandle {
    tx: Sender<SysCmd>,
    join: Option<JoinHandle<()>>,
    bridge_stop: Arc<AtomicBool>,
    diag: Arc<Mutex<SysDiag>>,
}

impl SysHandle {
    /// 启动 Sys 线程（阻塞到隐藏窗口/热键/托盘初始化完成）。
    ///
    /// 热键/钩子/托盘初始化失败不会返回 Err，而是通过 `Event::HotkeyStatus` /
    /// `Event::Toast` 通知 UI；只有「Sys 线程/隐藏窗口无法创建」
    /// 这种根本性失败才返回 Err（此时 UI 应展示错误对话框）。
    pub fn spawn(
        events: Sender<Event>,
        settings: Arc<Mutex<Settings>>,
        ctx: egui::Context,
    ) -> Result<Self, SysError> {
        let (tx, cmd_rx) = mpsc::channel::<SysCmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), SysError>>();
        let bridge_stop = Arc::new(AtomicBool::new(false));
        let bridge_stop_thread = bridge_stop.clone();
        let diag = Arc::new(Mutex::new(SysDiag::default()));
        let diag_thread = diag.clone();

        let join = std::thread::Builder::new()
            .name("clipnest-sys".into())
            .spawn(move || {
                // 兜底：Sys 线程内任何未捕获 panic 都只结束本线程并记录，
                // 不让 panic 传播（Drop 时 join 也能正常返回）。
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    thread::run(
                        events,
                        settings,
                        ctx,
                        cmd_rx,
                        ready_tx,
                        bridge_stop_thread,
                        diag_thread,
                    );
                }));
                if result.is_err() {
                    logging::error("Sys 线程因未捕获 panic 退出（已捕获，主程序继续运行）");
                }
            })
            .map_err(|err| SysError::Msg(format!("启动 Sys 线程失败：{err}")))?;

        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {
                logging::debug("SysHandle 已就绪");
                Ok(Self {
                    tx,
                    join: Some(join),
                    bridge_stop,
                    diag,
                })
            }
            Ok(Err(err)) => {
                // 线程内部初始化失败：回收线程，向上返回错误
                bridge_stop.store(true, Ordering::SeqCst);
                let _ = join.join();
                Err(err)
            }
            Err(_) => {
                bridge_stop.store(true, Ordering::SeqCst);
                Err(SysError::Msg(
                    "Sys 线程初始化超时（10s），隐藏窗口可能创建失败".to_string(),
                ))
            }
        }
    }

    /// 命令发送端（可克隆；发送失败表示 Sys 线程已退出）。
    pub fn cmd_tx(&self) -> Sender<SysCmd> {
        self.tx.clone()
    }

    /// 运行状态快照（`--diagnose` 用）：热键实际生效组合、Win+V 状态、托盘/监听状态等。
    pub fn diag(&self) -> SysDiag {
        match self.diag.lock() {
            Ok(d) => d.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Drop for SysHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(SysCmd::Shutdown);
        if let Some(handle) = self.join.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut finished = false;
            while Instant::now() < deadline {
                if handle.is_finished() {
                    finished = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if finished {
                let _ = handle.join();
                logging::debug("Sys 线程已 join");
            } else {
                // 超时：detach（不阻塞 UI 退出；进程结束时 OS 回收资源）
                logging::warn("Sys 线程 3s 内未退出，放弃等待（detach）");
            }
        }
        self.bridge_stop.store(true, Ordering::SeqCst);
    }
}

/// Sys 域错误（仅用于启动阶段的根本性失败）。
#[derive(Debug, thiserror::Error)]
pub enum SysError {
    #[error("{0}")]
    Msg(String),
    #[error("Win32 错误: {0}")]
    Win32(String),
}

#[allow(dead_code)]
pub type SysResult<T> = Result<T, SysError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sys_error_messages_are_chinese() {
        let err = SysError::Win32("创建隐藏窗口失败".to_string());
        assert!(err.to_string().contains("Win32"));
        let err = SysError::Msg("线程启动失败".to_string());
        assert!(err.to_string().contains("失败"));
    }

    #[test]
    fn hidden_window_class_is_stable() {
        // 单实例查找依赖该常量，必须与 CONTRACT/README 一致
        assert_eq!(thread::hidden_window_class(), "ClipNestHidden");
    }

    #[test]
    fn util_reexports_are_callable() {
        let _ = foreground_app_name();
        let _ = is_elevated();
    }
}

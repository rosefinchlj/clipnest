//! 常驻守护进程：托盘 / 全局热键 / Win+V 钩子 / 剪贴板监听 + SQLite 采集落库。
//!
//! **不创建任何 GUI 窗口**（因此不加载 eframe/OpenGL/字体），空闲内存保持在
//! 原生 Rust 程序的量级；面板与管理台按需以子进程启动（`--ui-picker` / `--ui-manager`），
//! 关闭后子进程退出、其 UI 内存全部释放。
//!
//! 与 UI 子进程的协作：
//! - 守护进程监听设置文件变化（子进程保存设置后 1s 内生效：重注册快捷键、暂停状态）；
//! - 子进程通过 `sys::thread::request_daemon_quit()`（WM_COPYDATA）请求退出守护进程；
//! - 子进程直接打开同一个 SQLite（WAL 多进程安全）读写历史。

use crate::config;
use crate::logging;
use crate::model::{Event, Settings, StoreCmd, SysCmd};
use crate::store::StoreHandle;
use crate::sys::SysHandle;
use crate::window;
use eframe::egui;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// 面板/管理台子进程类型。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiKind {
    Picker,
    Manager,
}

/// 守护进程主体。`store`/`sys` 依赖 Drop 顺序负责优雅退出（字段本身不直接读取）。
#[allow(dead_code)]
pub struct Daemon {
    store: StoreHandle,
    sys: SysHandle,
    store_tx: Sender<StoreCmd>,
    sys_tx: Sender<SysCmd>,
    settings: Arc<Mutex<Settings>>,
    events: Receiver<Event>,
    child: Option<Child>,
    child_kind: Option<UiKind>,
    settings_stamp: Option<SystemTime>,
}

/// 启动守护进程并阻塞运行，直到收到退出请求或通道断开。
pub fn run() -> Result<(), String> {
    config::ensure_dirs().map_err(|e| format!("创建数据目录失败：{e}"))?;
    let initial = config::load_settings();
    logging::init(&config::logs_dir(), &initial.log_level);
    logging::log(&format!(
        "ClipNest 守护进程启动（数据目录={}）",
        config::data_dir().display()
    ));

    let settings = Arc::new(Mutex::new(initial));
    // 守护进程没有 UI：给 Store/Sys 一个无窗口的 egui Context 仅用于 request_repaint()（空操作）。
    let ctx = egui::Context::default();
    let (events_tx, events) = mpsc::channel::<Event>();

    let store = StoreHandle::spawn(events_tx.clone(), settings.clone(), ctx.clone())
        .map_err(|e| format!("存储线程启动失败：{e}"))?;
    let store_tx = store.cmd_tx();
    let sys = SysHandle::spawn(events_tx, settings.clone(), ctx)
        .map_err(|e| format!("系统线程启动失败：{e}"))?;
    let sys_tx = sys.cmd_tx();

    let mut daemon = Daemon {
        store,
        sys,
        store_tx,
        sys_tx,
        settings,
        events,
        child: None,
        child_kind: None,
        settings_stamp: settings_stamp(),
    };
    logging::log("守护进程就绪（常驻托盘，无 GUI 窗口）");
    daemon.event_loop();
    daemon.shutdown();
    Ok(())
}

impl Daemon {
    fn event_loop(&mut self) {
        loop {
            match self.events.recv_timeout(Duration::from_millis(250)) {
                Ok(event) => {
                    if !self.handle_event(event) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.reap_child();
            self.poll_settings();
        }
    }

    /// 返回 false 表示请求退出。
    fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::PickerRequested { target_hwnd } => {
                self.spawn_ui(UiKind::Picker, target_hwnd, "history");
            }
            Event::ManagerRequested => {
                self.spawn_ui(UiKind::Manager, 0, "history");
            }
            Event::SettingsRequested => {
                self.spawn_ui(UiKind::Manager, 0, "settings");
            }
            Event::QuitRequested => {
                logging::log("收到退出请求，准备退出守护进程");
                return false;
            }
            Event::Captured(input) => {
                if self.store_tx.send(StoreCmd::Upsert(input)).is_err() {
                    logging::error("Store 通道已断开，无法写入剪贴板记录");
                    return false;
                }
            }
            Event::Toast(msg) => logging::log(&format!("后台提示：{msg}")),
            Event::WinVStatus(status) => logging::log(&format!("Win+V 状态更新：{}", status.label())),
            Event::HotkeyStatus { ok, reason } => logging::log(&format!(
                "备用快捷键状态：ok={ok} {}",
                reason.unwrap_or_default()
            )),
            _ => {}
        }
        true
    }

    fn reap_child(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                logging::log(&format!("UI 子进程已退出（{status}）"));
                self.child = None;
                self.child_kind = None;
            }
            Ok(None) => {}
            Err(err) => {
                logging::warn(&format!("查询 UI 子进程状态失败：{err}"));
                self.child = None;
                self.child_kind = None;
            }
        }
    }

    /// 设置文件变化 → 重载并应用到 Sys（热键/暂停）。
    fn poll_settings(&mut self) {
        let stamp = settings_stamp();
        if stamp == self.settings_stamp {
            return;
        }
        self.settings_stamp = stamp;
        let next = config::load_settings();
        if let Ok(mut guard) = self.settings.lock() {
            *guard = next.clone();
        }
        logging::log("检测到设置文件变化，已重载并应用");
        let _ = self.sys_tx.send(SysCmd::SetHotkeys {
            picker: next.hotkey_picker,
            winv_mode: next.winv_mode,
        });
        let _ = self.sys_tx.send(SysCmd::SetPause(next.pause_capture));
    }

    fn activate_existing_ui(&self) -> bool {
        for title in ["ClipNest · 剪贴匣", "ClipNest · 剪贴匣 · 管理"] {
            if let Some(hwnd) = window::find_window_by_title(title) {
                window::force_foreground(hwnd);
                return true;
            }
        }
        false
    }

    fn spawn_ui(&mut self, kind: UiKind, target_hwnd: isize, page: &str) {
        // 已有 UI 子进程：同类型只抢焦点；不同类型先结束旧的，再开新的。
        let child_alive = match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        };
        if child_alive {
            if self.child_kind == Some(kind) {
                // 同类型：优先聚焦已打开窗口；子进程刚启动/窗口尚未创建时直接忽略重复请求，
                // 避免"杀掉刚启动的子进程再重启"的抖动。
                let _ = self.activate_existing_ui();
                return;
            }
            if let Some(mut child) = self.child.take() {
                logging::log("结束旧的 UI 子进程以切换到新的界面");
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.child = None;
        self.child_kind = None;

        let exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(err) => {
                logging::error(&format!("无法定位当前 exe：{err}"));
                return;
            }
        };
        let mut cmd = Command::new(exe);
        match kind {
            UiKind::Picker => {
                cmd.arg("--ui-picker")
                    .arg("--target")
                    .arg(target_hwnd.to_string());
            }
            UiKind::Manager => {
                cmd.arg("--ui-manager").arg("--page").arg(page);
            }
        }
        // 允许子进程把窗口切到前台（否则 SetForegroundWindow 会被前台锁拒绝）。
        window::allow_set_foreground_any();
        match cmd.spawn() {
            Ok(child) => {
                logging::log(&format!(
                    "启动 UI 子进程：{:?}（target_hwnd={target_hwnd}，page={page}）",
                    kind
                ));
                self.child = Some(child);
                self.child_kind = Some(kind);
            }
            Err(err) => logging::error(&format!("启动 UI 子进程失败：{err}")),
        }
    }

    fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Drop：SysHandle 发送 Shutdown 并 join；StoreHandle 同样。
        logging::log("守护进程退出");
    }
}

/// 设置文件修改时间（用于跨进程变更检测）。
fn settings_stamp() -> Option<SystemTime> {
    std::fs::metadata(config::settings_path())
        .and_then(|meta| meta.modified())
        .ok()
}

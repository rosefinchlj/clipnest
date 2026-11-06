//! Sys 线程：唯一的 Win32 消息循环（隐藏消息窗口）。
//!
//! 职责（CONTRACT §3.2）：
//! - 隐藏窗口类 `ClipNestHidden`（`HWND_MESSAGE`）；
//! - `AddClipboardFormatListener` + `WM_CLIPBOARDUPDATE` → 采集 → `Event::Captured`；
//! - `RegisterHotKey` / Win+V 钩子 → `Event::PickerRequested{target_hwnd}`；
//! - 托盘（tray-icon）创建、菜单事件 → `Event`；
//! - 收 `SysCmd`（mpsc）→ 写剪贴板/自动粘贴/热键重配/暂停/自启/退出；
//! - 退出时清理所有 Win32 资源（钩子/热键/托盘/剪贴板监听/窗口）。
//!
//! 线程间只通过 `mpsc` 与 `Arc<Mutex<Settings>>` 通信；UI 线程通过 `ctx.request_repaint()`
//! 被唤醒。命令从 UI 线程进来后先由 bridge 线程转成窗口消息，保证 Sys 线程用
//! `GetMessageW` 阻塞等待、空闲时零唤醒（低 CPU）。
#![allow(dead_code)] // Sys 域内部工具：部分函数仅供测试/诊断/后续接入使用


use crate::logging;
use crate::model::{Event, Settings, SysCmd, TempPause};
use crate::sys::state::SysState;
use crate::sys::{hotkey, tray, util, SysDiag};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, COPYDATASTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW,
    GetWindowLongPtrW, IsWindow, PostQuitMessage, RegisterClassW, SendMessageW, SetTimer,
    SetWindowLongPtrW, TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, HWND_MESSAGE, MSG, WM_APP,
    WM_CLIPBOARDUPDATE, WM_CLOSE, WM_COPYDATA, WM_DESTROY, WM_HOTKEY, WM_NCCREATE, WM_TIMER,
    WNDCLASSW,
};

/// 备用呼出快捷键的 `WM_HOTKEY` id。
pub(crate) const HOTKEY_ID_PICKER: i32 = 1;
/// 隐藏窗口类名（`main.rs`/单实例查找复用）。
pub const HIDDEN_WINDOW_CLASS: &str = "ClipNestHidden";

/// UI → Sys 的命令消息（由 bridge 线程转投递到 Sys 线程）。
pub(crate) const WM_APP_CMD: u32 = WM_APP + 1;
/// 托盘菜单动作（wparam = `tray::MENU_*`）。
pub(crate) const WM_APP_MENU: u32 = WM_APP + 2;
/// 托盘图标事件（wparam = `tray::TRAY_*`）。
pub(crate) const WM_APP_TRAY: u32 = WM_APP + 3;
/// 前台窗口轮询定时器（记录"最近一个非本进程的前台窗口"，供托盘/热键唤起时作为粘贴目标）。
pub(crate) const FOREGROUND_POLL_TIMER_ID: usize = 0x434E;

/// 单实例激活消息标识（`WM_COPYDATA.dwData`，即 "CLNS"）。
pub const ACTIVATE_MAGIC: usize = 0x434C_4E53;
/// UI 子进程请求守护进程退出（`WM_COPYDATA.dwData`，即 "CLNQ"）。
pub const QUIT_MAGIC: usize = 0x434C_4E51;
/// 设置更新（子进程 → 主进程）：lpData 指向 UTF-8 JSON，cbData=字节数。
pub const SETTINGS_MAGIC: usize = 0x434C_4E54;
/// 临时暂停控制（子进程 → 主进程）：lpData 指向 `TempPause` 的 UTF-8 JSON。
pub const TEMP_PAUSE_MAGIC: usize = 0x434C_4E50;
/// 数据库加密开关请求（管理台 → 主进程）：lpData 为一个 u8（0/1）。
pub const ENCRYPTION_MAGIC: usize = 0x434C_4E45;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// 窗口过程
// ---------------------------------------------------------------------------

unsafe fn state_ptr(hwnd: HWND) -> *mut SysState {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SysState }
}

/// 取窗口关联状态执行 `f`（窗口未就绪/已销毁时安全忽略）。
fn with_state<R>(hwnd: HWND, f: impl FnOnce(&mut SysState) -> R) -> Option<R> {
    let ptr = unsafe { state_ptr(hwnd) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { f(&mut *ptr) })
    }
}

/// 消息分发：返回 `Some(result)` 表示已处理，`None` 交给 DefWindowProc。
///
/// 注意：本函数会调用其它域代码（clipboard/paste 等），任何 panic 都由 [`wndproc`]
/// 用 `catch_unwind` 捕获，绝不让 panic 穿过 FFI 边界导致进程 abort（后台线程不得崩溃）。
fn dispatch_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_NCCREATE => {
            // 把 CreateWindowExW 传入的 Box<SysState> 指针挂到窗口上
            let createstruct = unsafe { &*(lparam as *const CREATESTRUCTW) };
            let ptr = createstruct.lpCreateParams as *mut SysState;
            if !ptr.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
                    (*ptr).hwnd = hwnd;
                }
            }
            Some(1) // TRUE：继续创建窗口
        }
        WM_CLIPBOARDUPDATE => {
            with_state(hwnd, |state| state.on_clipboard_update());
            Some(0)
        }
        WM_HOTKEY => {
            let id = wparam as i32;
            with_state(hwnd, |state| state.on_hotkey(id));
            Some(0)
        }
        WM_APP_CMD => {
            // wparam 不可信（隐藏窗口可被其它进程 PostMessage）；命令走内部 mpsc，
            // 这里只作为唤醒信号触发排空。
            let _ = wparam;
            with_state(hwnd, |state| state.drain_commands());
            Some(0)
        }
        WM_APP_MENU => {
            let code = wparam as u32;
            with_state(hwnd, |state| state.on_tray_menu(code));
            Some(0)
        }
        WM_APP_TRAY => {
            let code = wparam as u32;
            with_state(hwnd, |state| {
                if code == tray::TRAY_RIGHT_CLICK {
                    state.on_tray_right_click();
                } else {
                    state.on_tray_click(code);
                }
            });
            Some(0)
        }
        WM_TIMER if wparam == FOREGROUND_POLL_TIMER_ID => {
            with_state(hwnd, |state| {
                state.update_foreground();
                // 定时检查临时暂停到期，保证没有剪贴板活动时托盘/设置页也能恢复。
                state.refresh_temp_pause();
            });
            Some(0)
        }
        WM_COPYDATA => {
            // 单实例：第二个进程通过 WM_COPYDATA 请求唤起本实例（打开面板）。
            // 防御：lparam 可能为空/伪造；只认 magic 且 cbData==0 的消息（不读 lpData）。
            if lparam == 0 {
                return Some(0);
            }
            let cds = unsafe { &*(lparam as *const COPYDATASTRUCT) };
            if cds.dwData == ACTIVATE_MAGIC
                && (cds.cbData == 0 || cds.cbData == std::mem::size_of::<isize>() as u32)
            {
                logging::log("收到唤起候选窗消息（WM_COPYDATA）");
                let target = if cds.cbData == std::mem::size_of::<isize>() as u32
                    && !cds.lpData.is_null()
                {
                    unsafe { *(cds.lpData as *const isize) }
                } else {
                    0
                };
                with_state(hwnd, |state| {
                    if target != 0 {
                        state.request_picker_with_target(target, "管理台唤起");
                    } else {
                        state.request_picker("第二实例唤起");
                    }
                });
            } else if cds.dwData == QUIT_MAGIC && cds.cbData == 0 {
                logging::log("收到 UI 子进程的退出请求（WM_COPYDATA）");
                with_state(hwnd, |state| state.emit_quit_request());
            } else if cds.dwData == SETTINGS_MAGIC && cds.cbData > 0 && cds.cbData <= 64 * 1024 {
                let bytes = unsafe {
                    std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize)
                };
                match serde_json::from_slice::<Settings>(bytes) {
                    Ok(settings) => {
                        logging::log("收到管理台子进程的设置更新（WM_COPYDATA）");
                        let boxed = Box::new(settings);
                        with_state(hwnd, |state| state.apply_external_settings(boxed));
                    }
                    Err(err) => logging::warn(&format!("子进程设置数据解析失败：{err}")),
                }
            } else if cds.dwData == TEMP_PAUSE_MAGIC && cds.cbData > 0 && cds.cbData <= 4096 {
                let bytes = unsafe {
                    std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize)
                };
                match serde_json::from_slice::<TempPause>(bytes) {
                    Ok(pause) => {
                        logging::log("收到管理台子进程的临时暂停请求（WM_COPYDATA）");
                        with_state(hwnd, |state| state.set_temp_pause(pause));
                    }
                    Err(err) => logging::warn(&format!("临时暂停数据解析失败：{err}")),
                }
            } else if cds.dwData == ENCRYPTION_MAGIC && cds.cbData == 1 {
                let enabled = unsafe { *(cds.lpData as *const u8) } != 0;
                logging::log(&format!(
                    "收到管理台子进程的数据库加密请求（WM_COPYDATA）：enabled={enabled}"
                ));
                with_state(hwnd, |state| state.request_encryption_switch(enabled));
            }
            Some(1) // TRUE：消息已处理
        }
        WM_CLOSE => {
            unsafe {
                PostQuitMessage(0);
            }
            Some(0)
        }
        WM_DESTROY => Some(0),
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // catch_unwind：wndproc 是 extern "system"（FFI 边界），panic 逃逸会直接 abort 进程。
    // 捕获后记录日志并跳过该消息，保证 Sys 线程在任何异常下都不拖垮主程序。
    let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch_message(hwnd, msg, wparam, lparam)
    }));
    match dispatched {
        Ok(Some(result)) => result,
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        Err(_) => {
            crate::logging::error(&format!(
                "Sys 窗口过程捕获到 panic（msg=0x{msg:04X}），已忽略该消息"
            ));
            0
        }
    }
}

// ---------------------------------------------------------------------------
// 线程入口
// ---------------------------------------------------------------------------

/// Sys 线程启动结果（通过 ready 通道回传给 `SysHandle::spawn`）。
pub(crate) type ReadySender = Sender<Result<(), crate::sys::SysError>>;

/// 启动 Sys 线程（由 `SysHandle::spawn` 调用；本函数阻塞在消息循环直到退出）。
pub(crate) fn run(
    events: Sender<Event>,
    settings: Arc<Mutex<Settings>>,
    ctx: egui::Context,
    cmd_rx: Receiver<SysCmd>,
    ready_tx: ReadySender,
    bridge_stop: Arc<AtomicBool>,
    diag: Arc<Mutex<SysDiag>>,
) {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class_name = wide(HIDDEN_WINDOW_CLASS);
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
    }

    // 命令链路：UI mpsc（外部）→ bridge 线程 → 内部 mpsc → WM_APP_CMD 唤醒 → Sys 线程。
    // 数据不经过窗口消息参数，避免伪造指针的安全性/稳定性风险。
    let (cmd_tx_internal, cmd_rx_internal) = mpsc::channel::<SysCmd>();
    let state = Box::new(SysState {
        hwnd: std::ptr::null_mut(),
        events: events.clone(),
        ctx: ctx.clone(),
        settings: settings.clone(),
        picker_hotkey: None,
        tray: None,
        shutdown: false,
        diag: diag.clone(),
        cmd_rx: cmd_rx_internal,
        last_external_fg: 0,
        temp_pause: crate::model::TempPause::Off,
    });
    let state_ptr = Box::into_raw(state);
    let title = wide("ClipNest");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            instance,
            state_ptr as *const std::ffi::c_void,
        )
    };
    if hwnd.is_null() {
        let err = std::io::Error::last_os_error();
        unsafe {
            drop(Box::from_raw(state_ptr));
        }
        let _ = ready_tx.send(Err(crate::sys::SysError::Win32(format!(
            "创建隐藏窗口失败：{err}"
        ))));
        return;
    }
    logging::log(&format!("Sys 线程已启动，隐藏窗口 hwnd={hwnd:?}"));

    // 周期记录"最近一个非本进程的前台窗口"：托盘菜单/热键唤起时，
    // 当前前台可能是本进程的托盘隐藏窗口或自己，不能作为粘贴目标。
    unsafe {
        SetTimer(hwnd, FOREGROUND_POLL_TIMER_ID, 300, None);
    }

    // 1) 剪贴板监听
    let listener_ok = unsafe { AddClipboardFormatListener(hwnd) } != 0;
    if !listener_ok {
        let err = std::io::Error::last_os_error();
        logging::warn(&format!("AddClipboardFormatListener 失败：{err}"));
        let _ = events.send(Event::Toast(format!("剪贴板监听注册失败：{err}")));
        ctx.request_repaint();
    } else {
        logging::log("剪贴板监听已注册（AddClipboardFormatListener）");
    }
    with_state(hwnd, |state| {
        state.update_diag(|d| {
            d.hidden_hwnd = hwnd as isize;
            d.clipboard_listener_ok = listener_ok;
            d.elevated = util::is_elevated();
        })
    });

    // 2) 托盘：先装事件处理器（全局一次），再创建图标
    tray::install_event_handlers(hwnd);
    let tray_state = {
        let snapshot = match settings.lock() {
            Ok(s) => s.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        match tray::create(&snapshot) {
            Ok(tray) => Some(tray),
            Err(err) => {
                logging::warn(&err.to_string());
                let _ = events.send(Event::Toast(err));
                ctx.request_repaint();
                None
            }
        }
    };

    // 3) 备用呼出快捷键（带降级链）：失败只发状态事件，不让 spawn 返回 Err
    let requested = match settings.lock() {
        Ok(s) => s.hotkey_picker,
        Err(poisoned) => poisoned.into_inner().hotkey_picker,
    };
    let registration = hotkey::register_with_fallback(hwnd, HOTKEY_ID_PICKER, requested);
    let (hotkey_ok, hotkey_reason) = registration.status();
    logging::log(&format!(
        "备用快捷键：请求={} 生效={} ok={hotkey_ok}",
        requested.label(),
        registration.effective.label()
    ));
    let _ = events.send(Event::HotkeyStatus {
        ok: hotkey_ok,
        reason: hotkey_reason,
    });

    // 把初始化结果写回状态 + 诊断快照
    let tray_ok = tray_state.is_some();
    with_state(hwnd, |state| {
        state.picker_hotkey = Some(registration);
        state.tray = tray_state;
        // 托盘创建后同步一次暂停状态（勾选/临时暂停文案/tooltip）。
        state.sync_tray_pause_state();
        state.update_diag(|d| {
            d.tray_ok = tray_ok;
            d.hotkey_requested = Some(requested);
            d.hotkey_effective = state
                .picker_hotkey
                .as_ref()
                .map(|registration| registration.effective);
            d.hotkey_ok = hotkey_ok;
            d.hotkey_note = state
                .picker_hotkey
                .as_ref()
                .and_then(|registration| registration.note.clone());
        });
    });

    // 5) 命令 bridge 线程：UI mpsc → 窗口消息（保证 Sys 线程阻塞式等待消息）
    let bridge = spawn_cmd_bridge(hwnd as isize, cmd_rx, cmd_tx_internal, bridge_stop.clone());

    // 就绪：通知 spawn 结束阻塞（此后即使热键/托盘失败，Sys 仍可用）
    let _ = ready_tx.send(Ok(()));

    // 6) 消息循环
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if ret <= 0 {
            break; // 0 = WM_QUIT，-1 = 错误
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // 7) 清理：Win32 资源 → 状态 Box → bridge 线程
    logging::log("Sys 线程消息循环结束，开始清理");
    with_state(hwnd, |state| state.cleanup());
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        DestroyWindow(hwnd);
        drop(Box::from_raw(state_ptr));
    }
    bridge_stop.store(true, Ordering::SeqCst);
    let _ = bridge.join();
    logging::log("Sys 线程已退出");
}

/// 命令 bridge：把外部 mpsc 收到的 `SysCmd` 转入 Sys 线程自有队列，再 PostMessage 唤醒。
///
/// HWND 是裸指针（非 Send），跨线程传递时用 `isize` 保存，线程内再转回 HWND。
/// 数据不放进窗口消息参数：隐藏窗口可被其它进程 `FindWindow` + `PostMessage` 伪造，
/// 只有 mpsc 数据是安全的。
fn spawn_cmd_bridge(
    hwnd: isize,
    cmd_rx: Receiver<SysCmd>,
    cmd_tx_internal: Sender<SysCmd>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("clipnest-sys-bridge".into())
        .spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(cmd) => {
                        // Sys 线程已退出时 send 失败：直接丢弃即可
                        if cmd_tx_internal.send(cmd).is_ok() {
                            unsafe {
                                windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                                    hwnd as HWND,
                                    WM_APP_CMD,
                                    0,
                                    0,
                                );
                            }
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .unwrap_or_else(|err| {
            logging::error(&format!("创建 Sys 命令 bridge 线程失败：{err}"));
            // 返回一个立即结束的线程句柄不可行，直接构造空线程
            std::thread::spawn(|| {})
        })
}

/// 隐藏窗口类名（`--diagnose`/单实例使用）。
pub fn hidden_window_class() -> &'static str {
    HIDDEN_WINDOW_CLASS
}

/// 查找已运行实例的隐藏窗口句柄（0 = 未找到）。
///
/// 请求主进程唤出候选窗，并指定粘贴目标（管理台子进程用）。
pub fn activate_existing_instance_with_target(target: isize) -> bool {
    let hwnd = find_existing_instance();
    if hwnd == 0 || unsafe { IsWindow(hwnd as HWND) } == 0 {
        return false;
    }
    let cds = COPYDATASTRUCT {
        dwData: ACTIVATE_MAGIC,
        cbData: std::mem::size_of::<isize>() as u32,
        lpData: &target as *const isize as *mut _,
    };
    let ok = unsafe {
        SendMessageW(hwnd as HWND, WM_COPYDATA, 0, &cds as *const COPYDATASTRUCT as isize)
    };
    ok != 0
}

/// 把设置以 WM_COPYDATA(JSON) 推送给主进程（管理台子进程用）。
pub fn send_settings_update(settings: &Settings) -> bool {
    let hwnd = find_existing_instance();
    if hwnd == 0 || unsafe { IsWindow(hwnd as HWND) } == 0 {
        return false;
    }
    let Ok(bytes) = serde_json::to_vec(settings) else {
        return false;
    };
    let cds = COPYDATASTRUCT {
        dwData: SETTINGS_MAGIC,
        cbData: bytes.len() as u32,
        lpData: bytes.as_ptr() as *mut _,
    };
    let ok = unsafe {
        SendMessageW(hwnd as HWND, WM_COPYDATA, 0, &cds as *const COPYDATASTRUCT as isize)
    };
    ok != 0
}

/// 把临时暂停请求推送给主进程（管理台子进程用）。
pub fn send_temp_pause_update(pause: TempPause) -> bool {
    let hwnd = find_existing_instance();
    if hwnd == 0 || unsafe { IsWindow(hwnd as HWND) } == 0 {
        return false;
    }
    let Ok(bytes) = serde_json::to_vec(&pause) else {
        return false;
    };
    let cds = COPYDATASTRUCT {
        dwData: TEMP_PAUSE_MAGIC,
        cbData: bytes.len() as u32,
        lpData: bytes.as_ptr() as *mut _,
    };
    let ok = unsafe {
        SendMessageW(hwnd as HWND, WM_COPYDATA, 0, &cds as *const COPYDATASTRUCT as isize)
    };
    ok != 0
}

/// 把数据库加密请求推送给主进程（管理台子进程用）。
///
/// 主进程收到后会先关闭自己的 Store、完成文件转换、重建 Store，再重新打开设置页。
pub fn send_encryption_request(enabled: bool) -> bool {
    let hwnd = find_existing_instance();
    if hwnd == 0 || unsafe { IsWindow(hwnd as HWND) } == 0 {
        return false;
    }
    let byte = [u8::from(enabled)];
    let cds = COPYDATASTRUCT {
        dwData: ENCRYPTION_MAGIC,
        cbData: byte.len() as u32,
        lpData: byte.as_ptr() as *mut _,
    };
    let ok = unsafe {
        SendMessageW(hwnd as HWND, WM_COPYDATA, 0, &cds as *const COPYDATASTRUCT as isize)
    };
    ok != 0
}

/// 供 `main.rs` 的单实例逻辑使用：命名互斥体/类名查找 + [`activate_existing_instance`]。
pub fn find_existing_instance() -> isize {
    let class = wide(HIDDEN_WINDOW_CLASS);
    unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) as isize }
}

/// 通知已运行的实例打开面板（单实例第二个进程调用）。
///
/// 返回 false 表示未找到实例或发送失败（调用方应允许继续启动，不阻塞）。
#[allow(dead_code)]
pub fn activate_existing_instance() -> bool {
    let hwnd = find_existing_instance();
    if hwnd == 0 || unsafe { IsWindow(hwnd as HWND) } == 0 {
        return false;
    }
    // 防伪装：目标窗口所属进程必须是同名 exe（同一程序的另一个实例）
    let target_exe = util::process_name(util::window_pid(hwnd));
    let own_exe = std::env::current_exe()
        .ok()
        .and_then(|path| util::exe_name_from_path(&path.to_string_lossy()));
    if target_exe.is_none() || target_exe != own_exe {
        return false;
    }
    let mut data = COPYDATASTRUCT {
        dwData: ACTIVATE_MAGIC,
        cbData: 0,
        lpData: std::ptr::null_mut(),
    };
    let result = unsafe {
        SendMessageW(
            hwnd as HWND,
            WM_COPYDATA,
            0,
            &mut data as *mut _ as isize,
        )
    };
    result != 0
}

/// UI 子进程请求守护进程退出（"退出 ClipNest" 菜单/管理器动作使用）。
pub fn request_daemon_quit() -> bool {
    let hwnd = find_existing_instance();
    if hwnd == 0 || unsafe { IsWindow(hwnd as HWND) } == 0 {
        return false;
    }
    let mut data = COPYDATASTRUCT {
        dwData: QUIT_MAGIC,
        cbData: 0,
        lpData: std::ptr::null_mut(),
    };
    let result = unsafe {
        SendMessageW(
            hwnd as HWND,
            WM_COPYDATA,
            0,
            &mut data as *mut _ as isize,
        )
    };
    result != 0
}

/// 避免未使用警告：托盘菜单编码在 tray.rs 中使用。
#[allow(dead_code)]
fn _assert_consts() {
    let _ = mpsc::channel::<()>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_message_ids_are_distinct() {
        // 三个自定义消息必须互不相同，且都落在 WM_APP 区间
        let ids = [WM_APP_CMD, WM_APP_MENU, WM_APP_TRAY];
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
        for id in ids {
            assert!(id >= WM_APP, "自定义消息必须在 WM_APP 之上：{id:#x}");
        }
    }

    #[test]
    fn single_instance_lookup_does_not_panic() {
        // 测试进程通常没有 ClipNest 隐藏窗口；若本机正在运行主程序也可能非 0，两种情况都允许。
        let hwnd = find_existing_instance();
        // 未找到实例时 activate 必须安全返回 false
        if hwnd == 0 {
            assert!(!activate_existing_instance());
        }
    }
}

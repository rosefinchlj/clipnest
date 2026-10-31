//! Win+V 接管（spec §13）：L1 直接注册 → 失败降级 L2 低级键盘钩子（含开始菜单补偿）。
//!
//! P0 实测（`docs/P0-FINDINGS.md`）：
//! - 本机 `RegisterHotKey(MOD_WIN,'V')` 必然失败（1409，被系统剪贴板历史占用）；
//! - `WH_KEYBOARD_LL` 能完整捕获并吞掉 Win+V；
//! - 只吞 V 时系统会认为发生了一次「干净的 Win 单击」（有弹开始菜单风险）；
//! - 补偿：吞 V down/up + 吞 Win up，并 30ms 后注入一次 Win up 复位系统键状态（已验证可行）。
//!
//! 本模块只负责策略/钩子；`WM_HOTKEY`/补偿定时器消息由 `thread.rs` 分发。
#![allow(dead_code)] // Sys 域内部工具：部分函数仅供测试/诊断/后续接入使用


use crate::logging;
use crate::model::{Event, WinVMode, WinVStatus};
use eframe::egui;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, MOD_WIN, VK_LWIN, VK_RWIN, VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, KBDLLHOOKSTRUCT, PostMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, LLKHF_INJECTED, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// L1（`RegisterHotKey`）使用的热键 id；与 `thread.rs` 的 `WM_HOTKEY` 分发对应。
pub const WINV_HOTKEY_ID: i32 = 2;

/// 补偿消息：钩子吞掉 Win up 后通知 Sys 线程启动 30ms 定时器注入 Win up。
pub const WM_APP_WINV_RESET: u32 = 0x8000 + 0x20;
/// 补偿定时器 id（`thread.rs` 的 `WM_TIMER` 分发使用）。
pub const WINV_RESET_TIMER_ID: usize = 0x5E;
/// 补偿注入延迟（毫秒）。
pub const WINV_RESET_DELAY_MS: u32 = 30;

/// Win+V 接管句柄（由 Sys 线程持有，仅在该线程使用）。
pub struct WinVHandle {
    /// 当前状态（同时通过 `Event::WinVStatus` 发给 UI）。
    pub status: WinVStatus,
    /// L1 是否注册成功。
    l1_registered: bool,
    /// L2 钩子句柄（空表示未安装）。
    hook: HHOOK,
}

impl Default for WinVHandle {
    fn default() -> Self {
        Self {
            status: WinVStatus::Unknown,
            l1_registered: false,
            hook: std::ptr::null_mut(),
        }
    }
}

impl WinVHandle {
    /// 按模式应用 Win+V 接管策略，返回句柄；失败通过 `status` 表达，不返回 Err。
    pub fn apply(
        hwnd: HWND,
        mode: WinVMode,
        events: Sender<Event>,
        ctx: egui::Context,
    ) -> WinVHandle {
        let mut handle = WinVHandle::default();
        handle.install(hwnd, mode, events, ctx);
        handle
    }

    /// 注销旧策略后重新应用（`SysCmd::SetHotkeys` 使用）。
    pub fn reapply(
        &mut self,
        hwnd: HWND,
        mode: WinVMode,
        events: Sender<Event>,
        ctx: egui::Context,
    ) {
        self.uninstall(hwnd);
        self.install(hwnd, mode, events, ctx);
    }

    fn install(&mut self, hwnd: HWND, mode: WinVMode, events: Sender<Event>, ctx: egui::Context) {
        self.l1_registered = false;
        self.hook = std::ptr::null_mut();
        match mode {
            WinVMode::Off => {
                self.status = WinVStatus::Unavailable {
                    reason: "已在设置中关闭 Win+V 接管（备用快捷键仍可用）".to_string(),
                };
                logging::log("Win+V：模式=关闭，不接管");
            }
            WinVMode::RegisterOnly => {
                if self.try_register_l1(hwnd) {
                    self.status = WinVStatus::Registered;
                } else {
                    self.status = WinVStatus::Unavailable {
                        reason: "Win+V 已被系统剪贴板历史占用，且设置为仅系统热键注册".to_string(),
                    };
                }
            }
            WinVMode::Auto => {
                if self.try_register_l1(hwnd) {
                    self.status = WinVStatus::Registered;
                } else {
                    self.install_hook(hwnd, events, ctx);
                }
            }
            WinVMode::HookOnly => {
                self.install_hook(hwnd, events, ctx);
            }
        }
        logging::log(&format!("Win+V 状态：{}", self.status.label()));
    }

    /// L1：尝试 `RegisterHotKey(hwnd, WINV_HOTKEY_ID, MOD_WIN, 'V')`。
    fn try_register_l1(&mut self, hwnd: HWND) -> bool {
        let ok = unsafe { RegisterHotKey(hwnd, WINV_HOTKEY_ID, MOD_WIN, VK_V as u32) };
        if ok != 0 {
            self.l1_registered = true;
            logging::log("Win+V：L1 RegisterHotKey 注册成功（无需键盘钩子）");
            true
        } else {
            let err = std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(0);
            logging::log(&format!(
                "Win+V：L1 RegisterHotKey 失败（错误 {err}），降级 L2 键盘钩子"
            ));
            false
        }
    }

    /// L2：安装 `WH_KEYBOARD_LL` 钩子。
    fn install_hook(&mut self, hwnd: HWND, events: Sender<Event>, ctx: egui::Context) {
        let winv_ctx = winv_ctx(hwnd, events, ctx);
        // 复用同一个静态上下文（重新安装时只更新 hwnd）
        winv_ctx.hwnd.store(hwnd as isize, Ordering::SeqCst);
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(ll_keyboard_proc),
                std::ptr::null_mut(),
                0,
            )
        };
        if hook.is_null() {
            let err = std::io::Error::last_os_error();
            self.status = WinVStatus::Unavailable {
                reason: format!("键盘钩子安装失败（{err}）"),
            };
            logging::error(&format!("Win+V：L2 键盘钩子安装失败：{err}"));
            return;
        }
        self.hook = hook;
        self.status = WinVStatus::HookActive { compensated: true };
        logging::log("Win+V：L2 键盘钩子已安装（含开始菜单补偿）");
    }

    /// 卸载当前策略（注销 L1 / 卸钩子）。
    pub fn uninstall(&mut self, hwnd: HWND) {
        if self.l1_registered {
            unsafe {
                UnregisterHotKey(hwnd, WINV_HOTKEY_ID);
            }
            self.l1_registered = false;
            logging::debug("Win+V：已注销 L1 热键");
        }
        if !self.hook.is_null() {
            unsafe {
                UnhookWindowsHookEx(self.hook);
            }
            self.hook = std::ptr::null_mut();
            logging::debug("Win+V：已卸载 L2 键盘钩子");
        }
        self.status = WinVStatus::Unknown;
    }

    /// L1 是否生效（诊断用）。
    pub fn is_l1_registered(&self) -> bool {
        self.l1_registered
    }

    /// L2 钩子是否生效。
    pub fn is_hook_active(&self) -> bool {
        !self.hook.is_null()
    }
}

// ---------------------------------------------------------------------------
// 低级键盘钩子
// ---------------------------------------------------------------------------

struct WinVCtx {
    /// 事件发送端（钩子命中 → PickerRequested）。
    events: Sender<Event>,
    /// 用于 request_repaint。
    ctx: egui::Context,
    /// Sys 隐藏窗口句柄（接收补偿消息）。
    hwnd: AtomicIsize,
    /// Win 键是否按住（物理状态跟踪）。
    win_down: AtomicBool,
    /// 本轮 Win 按住期间是否吞过 V（决定是否要补偿吞 Win up）。
    swallowed_v_cycle: AtomicBool,
    /// 累计 Win+V 命中次数（钩子回调内只做原子自增，不写日志/文件）。
    hits: AtomicU32,
    /// 最近一次命中的前台窗口（供 Sys 线程日志/诊断读取）。
    last_target: AtomicIsize,
}

static WINV_CTX: OnceLock<WinVCtx> = OnceLock::new();

fn winv_ctx(hwnd: HWND, events: Sender<Event>, ctx: egui::Context) -> &'static WinVCtx {
    WINV_CTX.get_or_init(|| WinVCtx {
        events,
        ctx,
        hwnd: AtomicIsize::new(hwnd as isize),
        win_down: AtomicBool::new(false),
        swallowed_v_cycle: AtomicBool::new(false),
        hits: AtomicU32::new(0),
        last_target: AtomicIsize::new(0),
    })
}

/// Win+V 钩子累计命中次数 + 最近一次目标窗口（供 Sys 线程日志/诊断；钩子回调内不写日志）。
pub fn hook_stats() -> (u32, isize) {
    match WINV_CTX.get() {
        Some(ctx) => (
            ctx.hits.load(Ordering::Relaxed),
            ctx.last_target.load(Ordering::Relaxed),
        ),
        None => (0, 0),
    }
}

unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // 低级键盘钩子回调也是 FFI 边界（extern "system"）：panic 逃逸会 abort 进程，
    // 这里兜底捕获，异常时按「放行」处理，绝不吞掉用户按键。
    let swallow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        ll_keyboard_impl(code, wparam, lparam)
    }))
    .unwrap_or(false);
    if swallow {
        return 1;
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// 钩子回调实现；返回 true 表示吞掉该按键事件。
unsafe fn ll_keyboard_impl(code: i32, wparam: WPARAM, lparam: LPARAM) -> bool {
    if code >= 0 {
        if let Some(ctx) = WINV_CTX.get() {
            let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let injected = (kb.flags & LLKHF_INJECTED) != 0;
            // 忽略注入事件：避免把本程序/SendInput 回环当成用户按键（spec §13 L2）
            if !injected {
                let is_down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let is_up = matches!(wparam as u32, WM_KEYUP | WM_SYSKEYUP);
                if (is_down || is_up) && handle_key(ctx, kb.vkCode, is_down) {
                    return true; // 吞掉该按键
                }
            }
        }
    }
    false
}

/// Win+V 关键逻辑；返回 true 表示吞掉该按键事件。
fn handle_key(ctx: &WinVCtx, vk: u32, is_down: bool) -> bool {
    let vk_lwin = VK_LWIN as u32;
    let vk_rwin = VK_RWIN as u32;

    // Win 键状态跟踪
    if vk == vk_lwin || vk == vk_rwin {
        if is_down {
            ctx.win_down.store(true, Ordering::SeqCst);
            return false;
        }
        ctx.win_down.store(false, Ordering::SeqCst);
        if ctx.swallowed_v_cycle.swap(false, Ordering::SeqCst) {
            // 开始菜单补偿：吞掉 Win up，让系统看不到「干净的一次 Win 单击」
            let hwnd = ctx.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                unsafe {
                    PostMessageW(hwnd as HWND, WM_APP_WINV_RESET, 0, 0);
                }
            }
            // 注意：钩子回调内严禁写日志/阻塞 IO；命中统计由 Sys 线程稍后读取。
            return true;
        }
        return false;
    }

    // 只在「Win 按住 + V」时接管；其它 Win+字母组合一律放行
    if vk == VK_V as u32 && ctx.win_down.load(Ordering::SeqCst) {
        if is_down {
            ctx.swallowed_v_cycle.store(true, Ordering::SeqCst);
            let target = unsafe { GetForegroundWindow() } as isize;
            // 钩子回调内只做原子操作 + 非阻塞 mpsc 发送 + request_repaint，安全且低延迟
            ctx.hits.fetch_add(1, Ordering::Relaxed);
            ctx.last_target.store(target, Ordering::Relaxed);
            let _ = ctx.events.send(Event::PickerRequested {
                target_hwnd: target,
            });
            ctx.ctx.request_repaint();
        }
        return true; // 吞 V down/up
    }
    false
}

/// 注入一次 Win up：清除被吞掉的 Win up 造成的系统键状态粘滞。
/// 由 Sys 线程在 `WINV_RESET_DELAY_MS` 后调用（不在钩子回调里执行）。
pub fn inject_win_up() {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_LWIN,
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        logging::warn(&format!(
            "Win+V 补偿注入 Win up 失败：{}",
            std::io::Error::last_os_error()
        ));
    } else {
        logging::debug("Win+V：补偿注入 Win up 完成");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winv_hotkey_id_does_not_clash_with_picker() {
        // 约定：id 1 = 备用呼出键，id 2 = Win+V L1
        assert_eq!(WINV_HOTKEY_ID, 2);
    }

    #[test]
    fn default_status_is_unknown() {
        let handle = WinVHandle::default();
        assert!(matches!(handle.status, WinVStatus::Unknown));
        assert!(!handle.is_l1_registered());
        assert!(!handle.is_hook_active());
    }

    #[test]
    fn off_mode_is_unavailable() {
        let handle = WinVHandle::apply(
            std::ptr::null_mut(),
            WinVMode::Off,
            std::sync::mpsc::channel().0,
            egui::Context::default(),
        );
        match handle.status {
            WinVStatus::Unavailable { ref reason } => assert!(reason.contains("关闭")),
            ref other => panic!("Off 模式状态应为 Unavailable，实际 {other:?}"),
        }
    }

    #[test]
    fn register_only_mode_with_null_hwnd_is_unavailable() {
        // 空 hwnd 注册必然失败；不得 panic
        let handle = WinVHandle::apply(
            std::ptr::null_mut(),
            WinVMode::RegisterOnly,
            std::sync::mpsc::channel().0,
            egui::Context::default(),
        );
        assert!(matches!(handle.status, WinVStatus::Unavailable { .. }));
    }

    #[test]
    fn uninstall_with_null_handle_is_safe() {
        let mut handle = WinVHandle::default();
        handle.uninstall(std::ptr::null_mut());
        assert!(!handle.is_l1_registered());
    }
}

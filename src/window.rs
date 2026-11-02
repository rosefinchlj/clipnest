//! 原生窗口辅助（Lead 写域）：HWND 获取、Win11 背景材质、圆角、屏幕定位。
//!
//! 所有函数只在 UI 线程调用；平台失败一律返回 Err/None，由调用方降级，
//! 绝不 panic（窗口效果不能阻塞主流程）。

use eframe::egui;
use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use std::num::NonZeroIsize;

/// 从 eframe Frame 取原生 HWND（isize）。
pub fn hwnd_of_frame(frame: &eframe::Frame) -> Option<isize> {
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// 让 window-vibrancy 之类需要 `HasWindowHandle` 的库接受裸 HWND。
struct HwndHandle(isize);

impl HasWindowHandle for HwndHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let hwnd = NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(hwnd));
        // Safety: 调用方保证 HWND 有效且属于本进程窗口，且在 UI 线程调用。
        unsafe { Ok(WindowHandle::borrow_raw(raw)) }
    }
}

/// 应用 Win11 背景材质：优先 Mica Alt（tabbed，无拖动卡顿），回退 Acrylic。
/// 返回实际生效的材质名，便于日志/诊断。
pub fn apply_backdrop(hwnd: isize, light: bool) -> Result<&'static str, String> {
    // 首选 Acrylic（SetWindowCompositionAttribute）：对 OpenGL/GL 交换链窗口也能真正透出模糊背景；
    // Win11 的 Mica（DWM system backdrop）要求应用自己绘制透明表面，egui/glow 下通常看不到效果，
    // 所以把 Mica 仅作为 Acrylic 失败时的兜底。
    set_dark_mode(hwnd, !light);
    let tint = if light {
        (246, 249, 255, 170)
    } else {
        (16, 20, 27, 150)
    };
    match window_vibrancy::apply_acrylic(HwndHandle(hwnd), Some(tint)) {
        Ok(()) => {
            set_round_corners(hwnd);
            return Ok("acrylic");
        }
        Err(err) => {
            crate::logging::warn(&format!("Acrylic 不可用：{err}，尝试 Mica"));
        }
    }
    // Win11 22H2+：DWM system backdrop（部分机器/渲染器下可能看不到模糊，但聊胜于无）。
    if window_vibrancy::apply_mica(HwndHandle(hwnd), Some(!light)).is_ok() {
        set_round_corners(hwnd);
        return Ok("mica");
    }
    if window_vibrancy::apply_tabbed(HwndHandle(hwnd), Some(!light)).is_ok() {
        set_round_corners(hwnd);
        return Ok("mica-tabbed");
    }
    Err("背景材质不可用（Acrylic/Mica 都失败）".to_string())
}

/// 深色/浅色标题栏属性（DWMWA_USE_IMMERSIVE_DARK_MODE = 20）。失败静默。
fn set_dark_mode(hwnd: isize, dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let value: i32 = if dark { 1 } else { 0 };
    unsafe {
        DwmSetWindowAttribute(
            hwnd as _,
            20,
            &value as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

/// Win11 圆角（DWMWCP_ROUND）。失败静默（系统不支持就当方角）。
pub fn set_round_corners(hwnd: isize) {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    let value: i32 = DWMWCP_ROUND;
    unsafe {
        DwmSetWindowAttribute(
            hwnd as _,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &value as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

/// 让窗口出现在鼠标所在显示器的工作区居中（物理→逻辑坐标已按 DPI 换算）。
/// 返回 egui 逻辑坐标，供 `ViewportCommand::OuterPosition` 使用。
pub fn picker_position(size: egui::Vec2, pixels_per_point: f32) -> Option<egui::Pos2> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let ppp = pixels_per_point.max(0.5);
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut cursor) == 0 {
            return None;
        }
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return None;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let work = info.rcWork;
        let work_w = (work.right - work.left).max(0);
        let work_h = (work.bottom - work.top).max(0);
        let win_w = (size.x * ppp).round() as i32;
        let win_h = (size.y * ppp).round() as i32;
        let x = work.left + ((work_w - win_w).max(0) / 2);
        let y = work.top + ((work_h - win_h).max(0) / 2);
        Some(egui::pos2(x as f32 / ppp, y as f32 / ppp))
    }
}

/// 请求关闭本进程某个窗口（发 WM_CLOSE；比 egui `ViewportCommand::Close` 更可靠，
/// 尤其是窗口已被隐藏时——隐藏窗口的 viewport close 命令可能不会被事件循环处理）。
pub fn request_close_window(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    if hwnd == 0 {
        return;
    }
    unsafe {
        PostMessageW(hwnd as _, WM_CLOSE, 0, 0);
    }
}

/// 强制把窗口切到前台并聚焦（托盘菜单/热键从隐藏状态唤起时，
/// 仅靠 egui `ViewportCommand::Focus` 在部分场景会被前台锁拒绝）。
/// 用 AttachThreadInput 绕过前台锁定；失败静默（只是得不到焦点，不影响功能）。
pub fn force_foreground(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
        ShowWindow, SW_SHOW,
    };

    unsafe {
        let foreground = GetForegroundWindow();
        if foreground as isize == hwnd {
            return;
        }
        let current_thread = GetCurrentThreadId();
        let mut fg_pid: u32 = 0;
        let fg_thread = if foreground.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, &mut fg_pid)
        };

        let attached = fg_thread != 0 && fg_thread != current_thread;
        if attached {
            AttachThreadInput(current_thread, fg_thread, 1);
        }
        ShowWindow(hwnd as _, SW_SHOW);
        BringWindowToTop(hwnd as _);
        SetForegroundWindow(hwnd as _);
        SetActiveWindow(hwnd as _);
        SetFocus(hwnd as _);
        if attached {
            AttachThreadInput(current_thread, fg_thread, 0);
        }
    }
}

/// 诊断/能力判断用：真实 OS 版本 (major, build)。
/// `GetVersionExW` 会受应用兼容性清单影响（无清单时返回 6.1/9200），
/// 因此优先用 ntdll 的 `RtlGetVersion`，失败再回退 `GetVersionExW`。
pub fn os_version() -> (u32, u32) {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOW};

    type RtlGetVersionFn = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;

    unsafe {
        let dll_name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
        let ntdll = GetModuleHandleW(dll_name.as_ptr());
        if !ntdll.is_null() {
            if let Some(proc) = GetProcAddress(ntdll, b"RtlGetVersion\0".as_ptr()) {
                let rtl_get_version: RtlGetVersionFn = std::mem::transmute(proc);
                let mut info: OSVERSIONINFOW = std::mem::zeroed();
                info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
                if rtl_get_version(&mut info) == 0 {
                    return (info.dwMajorVersion, info.dwBuildNumber);
                }
            }
        }
        let mut info: OSVERSIONINFOW = std::mem::zeroed();
        info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
        if GetVersionExW(&mut info) != 0 {
            (info.dwMajorVersion, info.dwBuildNumber)
        } else {
            (0, 0)
        }
    }
}

/// 诊断用：当前系统是否支持 Win11 的背景材质（build >= 22621）。
pub fn supports_mica() -> bool {
    os_version().1 >= 22621
}

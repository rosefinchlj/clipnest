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
pub fn apply_backdrop(hwnd: isize) -> Result<&'static str, String> {
    // Win11 22H2+：系统背景材质（DWM system backdrop），性能最好。
    if window_vibrancy::apply_tabbed(HwndHandle(hwnd), Some(true)).is_ok() {
        set_round_corners(hwnd);
        return Ok("mica-tabbed");
    }
    // Win10 1809+：非官方 Acrylic（拖动可能略卡，但观感接近磨砂玻璃）。
    match window_vibrancy::apply_acrylic(HwndHandle(hwnd), Some((14, 17, 22, 165))) {
        Ok(()) => {
            set_round_corners(hwnd);
            Ok("acrylic")
        }
        Err(err) => Err(format!("背景材质不可用: {err}")),
    }
}

/// Win11 圆角（DWMWCP_ROUND）。失败静默（系统不支持就当方角）。
pub fn set_round_corners(hwnd: isize) {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    let value: i32 = DWMWCP_ROUND as i32;
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
        Some(egui::pos2(
            x as f32 / ppp,
            y as f32 / ppp,
        ))
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

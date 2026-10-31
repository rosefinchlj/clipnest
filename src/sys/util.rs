//! Win32 辅助：前台窗口/进程名/窗口标题/提权判断。
//!
//! 说明：窗口创建、亚克力、屏幕定位等 UI 相关工具由 Lead 在 `src/window.rs` 实现，
//! 本模块只提供 Sys 域（采集来源记录、忽略名单、诊断）需要的最小工具集。
//! 所有函数失败时返回 `None`/`false`，绝不 panic（Sys 线程不允许 panic 传播）。

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};

/// 从 Windows 路径中取小写文件名（含扩展名），例如
/// `C:\Program Files\Foo\Foo.EXE` → `foo.exe`。
pub fn exe_name_from_path(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }
    let name = trimmed
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(trimmed)
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_ascii_lowercase())
    }
}

/// 当前前台窗口句柄（无前台窗口返回 0）。
pub fn foreground_window_handle() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

/// 当前前台窗口所属进程名（小写、含 `.exe`），用于忽略名单与来源记录。
pub fn foreground_app_name() -> Option<String> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    process_name(pid)
}

/// 指定进程 id 的可执行文件名（小写、含 `.exe`）。
pub fn process_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut len: u32 = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) };
    unsafe { CloseHandle(handle) };
    if ok == 0 || len == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    exe_name_from_path(&path)
}

/// 当前前台窗口标题（用于来源记录；过长截断到 200 字符）。
pub fn foreground_window_title() -> Option<String> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 0 {
        return None;
    }
    let title = String::from_utf16_lossy(&buf[..len as usize]);
    let title = title.trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some(crate::model::truncate_chars(&title, 200))
}

/// 指定进程是否已提权（UIPI 诊断用；打不开进程时返回 false）。
pub fn is_process_elevated(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let elevated = token_is_elevated(handle);
    unsafe { CloseHandle(handle) };
    elevated
}

/// 当前进程是否提权。
pub fn is_elevated() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 || token.is_null() {
        return false;
    }
    let elevated = token_is_elevated(token);
    unsafe { CloseHandle(token) };
    elevated
}

/// 查询进程令牌是否提权；调用方负责关闭 handle。
fn token_is_elevated(process_or_token: HANDLE) -> bool {
    let mut info = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len: u32 = 0;
    let ok = unsafe {
        GetTokenInformation(
            process_or_token,
            TokenElevation,
            &mut info as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    ok != 0 && info.TokenIsElevated != 0
}

/// 指定窗口所属进程 id（无效窗口返回 0）。
pub fn window_pid(hwnd: isize) -> u32 {
    if hwnd == 0 {
        return 0;
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd as _, &mut pid) };
    pid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_from_path_variants() {
        assert_eq!(
            exe_name_from_path(r"C:\Program Files\Foo\Foo.EXE").as_deref(),
            Some("foo.exe")
        );
        assert_eq!(
            exe_name_from_path(r#""C:\Tools\notepad.exe""#).as_deref(),
            Some("notepad.exe")
        );
        assert_eq!(
            exe_name_from_path("C:/Users/a/b/WeChat.exe").as_deref(),
            Some("wechat.exe")
        );
        assert_eq!(exe_name_from_path("  "), None);
        assert_eq!(exe_name_from_path("\\"), None);
    }

    #[test]
    fn window_helpers_do_not_panic() {
        // 不依赖真实桌面环境：无效句柄必须安全返回
        assert_eq!(window_pid(0), 0);
        let _ = foreground_app_name();
        let _ = foreground_window_title();
        let _ = foreground_window_handle();
        let _ = is_elevated();
        assert!(!is_process_elevated(0));
    }
}

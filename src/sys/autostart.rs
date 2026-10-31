//! 开机自启：读写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 的 `ClipNest` 值。
//!
//! 约定（spec §16）：只写 HKCU，绝不写 HKLM，不申请管理员权限。
//! 失败时返回中文错误信息，由 Sys 线程转成 `Event::Toast`，不 panic。

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

/// Run 键路径。
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 注册表值名（与 `model::APP_NAME` 一致）。
const VALUE_NAME: &str = "ClipNest";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 当前 exe 的启动命令（带引号，避免路径含空格时被截断）。
pub fn exe_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取 exe 路径失败：{e}"))?;
    let path = exe.to_string_lossy().to_string();
    Ok(quote_command(&path))
}

/// 给可执行路径加引号；已有引号则原样返回。
pub fn quote_command(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed.to_string()
    } else {
        format!("\"{trimmed}\"")
    }
}

fn open_run_key(write: bool) -> Result<HKEY, String> {
    let sub = wide(RUN_KEY);
    let access = if write { KEY_SET_VALUE } else { KEY_QUERY_VALUE };
    let mut key: HKEY = std::ptr::null_mut();
    let rc: WIN32_ERROR = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, access, &mut key) };
    if rc != 0 || key.is_null() {
        return Err(format!("打开注册表 Run 键失败（错误 {rc}）"));
    }
    Ok(key)
}

/// 设置/取消开机自启。`enabled=false` 时删除值（不存在视为成功）。
pub fn set(enabled: bool) -> Result<(), String> {
    if enabled {
        let command = exe_command()?;
        let key = open_run_key(true)?;
        // REG_SZ 数据：UTF-16LE + 结尾 NUL
        let data: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        let name = wide(VALUE_NAME);
        let bytes = data.len() * std::mem::size_of::<u16>();
        let rc: WIN32_ERROR = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr() as *const u8,
                bytes as u32,
            )
        };
        unsafe { RegCloseKey(key) };
        if rc != 0 {
            return Err(format!("写入开机自启失败（错误 {rc}）"));
        }
    } else {
        let key = open_run_key(true)?;
        let name = wide(VALUE_NAME);
        let rc: WIN32_ERROR = unsafe { RegDeleteValueW(key, name.as_ptr()) };
        unsafe { RegCloseKey(key) };
        if rc != 0 && rc != ERROR_FILE_NOT_FOUND {
            return Err(format!("取消开机自启失败（错误 {rc}）"));
        }
    }
    crate::logging::log(&format!("开机自启已{}", if enabled { "开启" } else { "关闭" }));
    Ok(())
}

/// 读取 Run 值（存在返回其内容；不存在/读取失败返回 None）。
pub fn get_command() -> Option<String> {
    let key = open_run_key(false).ok()?;
    let name = wide(VALUE_NAME);
    let mut value_type: u32 = 0;
    let mut size: u32 = 0;
    let rc: WIN32_ERROR = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if rc != 0 || size == 0 {
        unsafe { RegCloseKey(key) };
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            buf.as_mut_ptr(),
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };
    if rc != 0 {
        return None;
    }
    // 按 UTF-16LE 解码，去掉结尾 NUL
    let u16s: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let text = String::from_utf16_lossy(&u16s);
    let text = text.trim_end_matches('\0').trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// 是否已启用开机自启（值存在即视为启用，不校验路径是否指向当前 exe）。
pub fn get() -> bool {
    get_command().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_command_variants() {
        assert_eq!(quote_command(r"C:\app.exe"), r#""C:\app.exe""#);
        assert_eq!(
            quote_command(r#""C:\Program Files\app.exe""#),
            r#""C:\Program Files\app.exe""#
        );
        assert_eq!(quote_command("  C:\\a b\\x.exe  "), r#""C:\a b\x.exe""#);
    }

    #[test]
    fn exe_command_is_quoted() {
        let cmd = exe_command().expect("测试进程应有 exe 路径");
        assert!(cmd.starts_with('"') && cmd.ends_with('"'));
    }

    #[test]
    fn get_does_not_panic() {
        // 只读，不修改用户注册表
        let _ = get();
        let _ = get_command();
    }
}

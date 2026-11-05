//! 系统外壳集成：用默认程序打开路径、在资源管理器中定位文件、复制文本到剪贴板。
//!
//! 全部在调用线程同步执行；失败返回中文错误信息，由调用方 toast 提示。
//! 这里不引入第三方 crate：打开/定位走 ShellExecuteW 与 explorer.exe，
//! 复制文本复用 [`crate::clipboard::write_clipboard`]（携带 ClipNest 自写标记）。

use std::path::Path;

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::model::{ClipKind, PastePayload};

/// 用系统默认程序打开文件/文件夹。
pub fn open_path(path: &Path) -> Result<(), String> {
    shell_execute("open", path, None)
}

/// 在资源管理器中定位文件；文件夹直接打开。
pub fn reveal_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("路径已不存在，可能在磁盘上被移动或删除".to_string());
    }
    if path.is_dir() {
        return open_path(path);
    }
    let arg = explorer_select_arg(path);
    std::process::Command::new("explorer.exe")
        .arg(&arg)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("打开资源管理器失败：{err}"))
}

/// 把文本写入系统剪贴板（用于「复制路径」）。
pub fn copy_text(text: &str) -> Result<(), String> {
    let payload = PastePayload {
        clip_id: 0,
        kind: ClipKind::Text,
        text: Some(text.to_string()),
        html: None,
        rtf: None,
        png_path: None,
        files: Vec::new(),
    };
    crate::clipboard::write_clipboard(&payload, Path::new(""))
        .map_err(|err| format!("写入剪贴板失败：{err}"))
}

/// 资源管理器 `/select,` 参数（纯函数，便于单测）。
pub(crate) fn explorer_select_arg(path: &Path) -> String {
    format!("/select,{}", path.display())
}

/// 调 ShellExecuteW；返回值 ≤32 视为失败（Win32 约定）。
fn shell_execute(operation: &str, file: &Path, params: Option<&str>) -> Result<(), String> {
    let op = to_wide(operation);
    let file_wide = to_wide(&file.to_string_lossy());
    let params_wide = params.map(to_wide);
    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file_wide.as_ptr(),
            params_wide
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = ret as isize;
    if code <= 32 {
        Err(format!(
            "系统调用失败（ShellExecute 返回 {code}）：{}",
            file.display()
        ))
    } else {
        Ok(())
    }
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_select_arg_formats_path() {
        assert_eq!(
            explorer_select_arg(Path::new(r"C:\Users\me\Documents\a.txt")),
            r"/select,C:\Users\me\Documents\a.txt"
        );
        // 含空格/中文时保持原样，由 Command 负责加引号。
        assert_eq!(
            explorer_select_arg(Path::new(r"C:\我的 文档\截图 1.png")),
            r"/select,C:\我的 文档\截图 1.png"
        );
    }

    #[test]
    fn shell_execute_rejects_missing_path() {
        let err = open_path(Path::new(r"Z:\clipnest-not-exist\a.txt")).unwrap_err();
        assert!(err.contains("ShellExecute"), "错误信息应包含系统调用名：{err}");
    }

    #[test]
    fn reveal_missing_path_reports_error() {
        let err = reveal_path(Path::new(r"Z:\clipnest-not-exist\a.txt")).unwrap_err();
        assert!(err.contains("不存在"), "错误信息应提示路径失效：{err}");
    }
}

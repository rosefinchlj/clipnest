//! 系统「另存为」对话框。
//!
//! 导出按钮不再静默写入数据目录，而是让用户选择保存位置；取消时返回 `None`。

use std::path::PathBuf;

use windows_sys::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OPENFILENAMEW, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// 打开 Windows 原生保存对话框，返回用户选择的 `.json` 路径。
pub(crate) fn choose_export_path(default_name: &str) -> Option<PathBuf> {
    let mut file_buf = vec![0u16; 1024];
    for (slot, unit) in file_buf.iter_mut().zip(default_name.encode_utf16()) {
        *slot = unit;
    }

    let filter: Vec<u16> = "JSON 文件 (*.json)\0*.json\0所有文件 (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "导出 ClipNest 数据".encode_utf16().chain(Some(0)).collect();
    let def_ext: Vec<u16> = "json".encode_utf16().chain(Some(0)).collect();

    let mut ofn = OPENFILENAMEW::default();
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = unsafe { GetForegroundWindow() };
    ofn.lpstrFilter = filter.as_ptr();
    ofn.nFilterIndex = 1;
    ofn.lpstrFile = file_buf.as_mut_ptr();
    ofn.nMaxFile = file_buf.len() as u32;
    ofn.lpstrTitle = title.as_ptr();
    ofn.lpstrDefExt = def_ext.as_ptr();
    ofn.Flags = OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;

    let ok = unsafe { GetSaveFileNameW(&mut ofn) };
    if ok == 0 {
        return None;
    }

    let end = file_buf
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(file_buf.len());
    if end == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&file_buf[..end])))
}

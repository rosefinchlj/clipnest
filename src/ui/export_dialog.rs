//! 系统「另存为 / 打开」对话框（Windows 原生）。
//!
//! 导出 JSON、完整备份、从备份恢复都先让用户选择位置；取消时返回 `None`。

use std::path::PathBuf;

use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OPENFILENAMEW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::model::BACKUP_EXTENSION;

const JSON_FILTER: &str = "JSON 文件 (*.json)\0*.json\0所有文件 (*.*)\0*.*\0\0";
const BACKUP_FILTER: &str = "ClipNest 备份 (*.cnbak)\0*.cnbak\0所有文件 (*.*)\0*.*\0\0";

/// 打开 Windows 原生保存对话框，返回用户选择的 `.json` 路径。
pub(crate) fn choose_export_path(default_name: &str) -> Option<PathBuf> {
    run_dialog(
        true,
        Some(default_name),
        JSON_FILTER,
        "导出 ClipNest 数据",
        "json",
    )
}

/// 打开 Windows 原生保存对话框，返回完整备份的保存路径（`.cnbak`）。
pub(crate) fn choose_backup_save_path(default_name: &str) -> Option<PathBuf> {
    run_dialog(
        true,
        Some(default_name),
        BACKUP_FILTER,
        "创建 ClipNest 完整备份",
        BACKUP_EXTENSION,
    )
}

/// 打开 Windows 原生打开对话框，返回要恢复的备份文件路径。
pub(crate) fn choose_backup_open_path() -> Option<PathBuf> {
    run_dialog(
        false,
        None,
        BACKUP_FILTER,
        "选择 ClipNest 备份文件",
        BACKUP_EXTENSION,
    )
}

fn run_dialog(
    save: bool,
    default_name: Option<&str>,
    filter: &str,
    title: &str,
    default_ext: &str,
) -> Option<PathBuf> {
    let mut file_buf = vec![0u16; 2048];
    if let Some(name) = default_name {
        for (slot, unit) in file_buf.iter_mut().zip(name.encode_utf16()) {
            *slot = unit;
        }
    }

    let filter: Vec<u16> = filter.encode_utf16().collect();
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let def_ext: Vec<u16> = default_ext.encode_utf16().chain(Some(0)).collect();

    let mut ofn = OPENFILENAMEW::default();
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = unsafe { GetForegroundWindow() };
    ofn.lpstrFilter = filter.as_ptr();
    ofn.nFilterIndex = 1;
    ofn.lpstrFile = file_buf.as_mut_ptr();
    ofn.nMaxFile = file_buf.len() as u32;
    ofn.lpstrTitle = title.as_ptr();
    ofn.lpstrDefExt = def_ext.as_ptr();
    let mut flags = OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_HIDEREADONLY;
    flags |= if save {
        OFN_OVERWRITEPROMPT
    } else {
        OFN_FILEMUSTEXIST
    };
    ofn.Flags = flags;

    let ok = unsafe {
        if save {
            GetSaveFileNameW(&mut ofn)
        } else {
            GetOpenFileNameW(&mut ofn)
        }
    };
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

//! 系统「另存为 / 打开」对话框（Windows 原生）。
//!
//! 导出 JSON、完整备份、从备份恢复、图片/文本另存为共用原生对话框；
//! 用户取消时返回 `None`（调用方不产生任何副作用）。

use std::path::PathBuf;

use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OPENFILENAMEW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::model::{truncate_chars, ClipItem, ClipKind, BACKUP_EXTENSION};

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

/// 通用「另存为」框；按条目类型自动拼过滤器与默认扩展名。
pub(crate) fn choose_save_path(
    default_name: &str,
    filter_name: &str,
    filter_spec: &str,
    ext: &str,
    title: &str,
) -> Option<PathBuf> {
    let filter = format!("{filter_name}\0{filter_spec}\0所有文件 (*.*)\0*.*\0\0");
    run_dialog(true, Some(default_name), &filter, title, ext)
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

/// 图片「另存为…」的默认文件名。
pub(crate) fn image_default_name(item: &ClipItem) -> String {
    let (w, h) = (item.width.unwrap_or(0), item.height.unwrap_or(0));
    if w > 0 && h > 0 {
        format!("ClipNest-图片-{w}x{h}.png")
    } else {
        format!("ClipNest-图片-{}.png", item.id)
    }
}

/// 文本「另存为…」的默认文件名：取首行前若干字符。
pub(crate) fn text_default_name(item: &ClipItem) -> String {
    let first_line = item
        .text
        .as_deref()
        .or(Some(item.preview.as_str()))
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
        .trim();
    if first_line.is_empty() {
        format!("ClipNest-文本-{}.txt", item.id)
    } else {
        format!("{}.txt", truncate_chars(first_line, 24).trim_end_matches('…'))
    }
}

/// 打开「另存为」框并返回目标路径；按条目类型选择默认文件名/过滤器。
pub(crate) fn choose_item_save_path(item: &ClipItem) -> Option<PathBuf> {
    match item.kind {
        ClipKind::Image => choose_save_path(
            &image_default_name(item),
            "PNG 图片",
            "*.png",
            "png",
            "图片另存为",
        ),
        ClipKind::Text => choose_save_path(
            &text_default_name(item),
            "文本文件",
            "*.txt",
            "txt",
            "文本另存为",
        ),
        // 文件类条目没有「另存为」——应使用「打开文件 / 所在文件夹 / 复制路径」。
        ClipKind::Files => None,
    }
}

/// 清洗默认文件名：非法字符替换为 `_`，去掉首尾空白/点，限 80 字符。
fn sanitize_file_name(raw: &str) -> String {
    const MAX: usize = 80;
    let mut out = String::with_capacity(raw.len().min(MAX * 3));
    let mut last_underscore = false;
    for ch in raw.chars().take(MAX) {
        let valid = !matches!(
            ch,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\0' | '\n' | '\r' | '\t'
        );
        if valid {
            out.push(ch);
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    let trimmed = out.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "ClipNest".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ClipItem;

    #[test]
    fn sanitize_replaces_illegal_chars() {
        assert_eq!(sanitize_file_name(r#"a<b>c:d"e/f\g|h?i*j"#), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize_file_name("  我的 文档  "), "我的 文档");
        assert_eq!(sanitize_file_name("..."), "ClipNest");
        assert_eq!(sanitize_file_name(""), "ClipNest");
        assert_eq!(sanitize_file_name("多个\\\\\\分隔"), "多个_分隔");
    }

    #[test]
    fn image_default_names() {
        let mut item = ClipItem {
            id: 7,
            kind: ClipKind::Image,
            ..Default::default()
        };
        assert_eq!(image_default_name(&item), "ClipNest-图片-7.png");
        item.width = Some(1920);
        item.height = Some(1080);
        assert_eq!(image_default_name(&item), "ClipNest-图片-1920x1080.png");
    }

    #[test]
    fn text_default_names() {
        let item = ClipItem {
            id: 3,
            kind: ClipKind::Text,
            text: Some("第一行标题\n第二行".into()),
            preview: "第一行标题".into(),
            ..Default::default()
        };
        assert_eq!(text_default_name(&item), "第一行标题.txt");

        let long = ClipItem {
            id: 4,
            kind: ClipKind::Text,
            text: Some("这是一个非常非常长的标题".repeat(5)),
            ..Default::default()
        };
        let name = text_default_name(&long);
        assert!(name.ends_with(".txt"));
        assert!(name.chars().count() <= 28, "默认文件名应截断：{name}");
        assert!(!name.contains('…'));

        let empty = ClipItem {
            id: 5,
            kind: ClipKind::Text,
            ..Default::default()
        };
        assert_eq!(text_default_name(&empty), "ClipNest-文本-5.txt");
    }
}

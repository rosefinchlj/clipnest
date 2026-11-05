//! 从系统剪贴板读取原始内容（只在 Sys 线程调用）。
//!
//! 读取顺序不重要，读取阶段只把各格式原样取出；优先级判断在 [`super::normalize`]。
//! 关键安全约定：
//! - `OpenClipboard` 失败按 spec §14 重试 5 次 × 20ms；
//! - 无论成功/失败/提前返回，[`ClipboardGuard`] 的 `Drop` 一定调用 `CloseClipboard`；
//! - `GetClipboardData` 返回的句柄归剪贴板所有，**只 Lock/Unlock 不 Free**；
//! - `GlobalLock` 成功后必定 `GlobalUnlock`，且所有切片长度都经 `GlobalSize` 校验。

use super::html;
use super::{
    ClipError, ClipResult, RawClipboard, CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, PNG_FORMAT,
    SELF_MARKER_FORMAT, VIEWER_IGNORE_FORMAT,
};
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::HGLOBAL;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW,
};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};

/// OpenClipboard 重试次数（spec §14：5 次 × 20ms）。
const OPEN_RETRY_TIMES: u32 = 5;
/// 重试间隔。
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(20);

/// 读取文本的硬上限：设置里单条文本最大 16MB，再多也无意义（normalize 还会按设置截断）。
const MAX_TEXT_READ_BYTES: usize = 16 * 1024 * 1024;
const MAX_TEXT_READ_UNITS: usize = MAX_TEXT_READ_BYTES / 2;

/// PNG 注册格式的读取上限（约 40MP 的 PNG 也能容纳，防异常巨块）。
pub(super) const MAX_PNG_BYTES: usize = 128 * 1024 * 1024;
/// CF_HTML 读取上限：normalize 阶段还会 sanitize 并按 2MB 截断，这里防异常巨块。
const MAX_HTML_READ_BYTES: usize = 4 * 1024 * 1024;
/// RTF 读取上限（同上）。
const MAX_RTF_READ_BYTES: usize = 4 * 1024 * 1024;
/// CF_DIB/CF_DIBV5 的读取上限：40MP × 32bpp ≈ 160MB，留出头/行对齐余量。
pub(super) const MAX_DIB_BYTES: usize = 200 * 1024 * 1024;
/// 单次最多读取的文件数（normalize 再按设置截断到默认 100）。
const MAX_FILES_READ: u32 = 1000;
/// 单个文件路径的最大 UTF-16 单元数（Windows 长路径上限 32767）。
const MAX_PATH_UNITS: u32 = 32768;

/// 打开的剪贴板守卫：`Drop` 时必定 `CloseClipboard`，保证所有错误路径都不泄漏。
pub(crate) struct ClipboardGuard;

impl ClipboardGuard {
    /// 打开剪贴板（失败重试 5 × 20ms）。返回的守卫存活期间剪贴板保持打开。
    pub(crate) fn open() -> ClipResult<Self> {
        for attempt in 1..=OPEN_RETRY_TIMES {
            let ok = unsafe { OpenClipboard(ptr::null_mut()) };
            if ok != 0 {
                return Ok(Self);
            }
            if attempt < OPEN_RETRY_TIMES {
                std::thread::sleep(OPEN_RETRY_DELAY);
            }
        }
        Err(ClipError::Busy)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

/// 读取系统剪贴板的原始内容。
///
/// 带 `Clipboard Viewer Ignore` 或本程序自写标记时直接返回 `ignore = true` 的空内容
/// （不碰任何数据，避免敏感内容进入内存）。
pub fn read_clipboard() -> ClipResult<RawClipboard> {
    // 守卫必须在读取期间一直存活：所有提前 return 都会触发 Drop → CloseClipboard
    let _guard = ClipboardGuard::open()?;

    let mut raw = RawClipboard::default();

    // 1) 忽略标记检测：密码管理器 / 本程序自身回写
    let viewer_ignore = register_format(VIEWER_IGNORE_FORMAT);
    let self_marker = register_format(SELF_MARKER_FORMAT);
    if is_available(viewer_ignore) || is_available(self_marker) {
        raw.ignore = true;
        crate::logging::debug("剪贴板含忽略/自写标记，跳过采集");
        return Ok(raw);
    }

    // 2) 文件列表（HDROP）
    raw.files = read_hdrop();

    // 3) 文本（UNICODETEXT）
    raw.text = read_text();

    // 4) 富文本（CF_HTML / RTF）：文件优先，存在文件列表时不再读富文本。
    //    读取/解析失败都静默降级为纯文本，不影响文本/图片/文件采集。
    if raw.files.is_empty() {
        raw.html = read_cf_html();
        raw.rtf = read_rtf();
    }

    // 5) 图片：仅在文件/非空文本都不存在时才值得读（优先级低于二者）
    let text_takes_priority = raw
        .text
        .as_deref()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false);
    if raw.files.is_empty() && !text_takes_priority {
        let png_format = register_format(PNG_FORMAT);
        if is_available(png_format) {
            raw.png = read_clipboard_bytes(png_format, MAX_PNG_BYTES);
        }
        if raw.png.is_none() {
            // CF_DIBV5 优先于 CF_DIB（V5 头信息更完整）
            if is_available(CF_DIBV5) {
                raw.dib = read_clipboard_bytes(CF_DIBV5, MAX_DIB_BYTES);
            }
            if raw.dib.is_none() && is_available(CF_DIB) {
                raw.dib = read_clipboard_bytes(CF_DIB, MAX_DIB_BYTES);
            }
        }
    }

    Ok(raw)
}

/// 注册（或复用已注册的）剪贴板格式，返回格式 ID；失败返回 0。
pub(crate) fn register_format(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
}

pub(super) fn is_available(format: u32) -> bool {
    format != 0 && unsafe { IsClipboardFormatAvailable(format) } != 0
}

/// 读取 CF_UNICODETEXT：GlobalLock / GlobalUnlock 严格配对，按第一个 `\0` 截断。
pub(super) fn read_text() -> Option<String> {
    if !is_available(CF_UNICODETEXT) {
        return None;
    }
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
    if handle.is_null() {
        return None;
    }
    let hglobal: HGLOBAL = handle;
    let size = unsafe { GlobalSize(hglobal) };
    if size < 2 {
        return None;
    }
    let lock = unsafe { GlobalLock(hglobal) };
    if lock.is_null() {
        crate::logging::debug("GlobalLock(文本) 失败，跳过文本格式");
        return None;
    }
    // GlobalSize 给出的是内存块大小（可能大于实际文本），用它作上界安全扫描
    let units = (size / 2).min(MAX_TEXT_READ_UNITS);
    let text = unsafe {
        let slice = std::slice::from_raw_parts(lock as *const u16, units);
        let end = slice.iter().position(|&u| u == 0).unwrap_or(units);
        String::from_utf16_lossy(&slice[..end])
    };
    // 严格配对解锁（无论内容异常都必须执行到这里）
    let _ = unsafe { GlobalUnlock(hglobal) };
    // 读取阶段再去一次尾部 \0（理论上已按 NUL 截断，这里是让 RawClipboard 契约自洽）
    let text = text.trim_end_matches('\0').to_string();
    Some(text)
}

/// 读取注册格式 "HTML Format"（CF_HTML）并解析出 fragment。
///
/// 解析失败（header 损坏 / 非 UTF-8 / 片段为空）时返回 `None`，调用方照常使用纯文本。
pub(super) fn read_cf_html() -> Option<String> {
    let format = register_format(html::HTML_FORMAT);
    if !is_available(format) {
        return None;
    }
    let bytes = read_clipboard_bytes(format, MAX_HTML_READ_BYTES)?;
    match html::parse_cf_html(&bytes) {
        Some(fragment) => Some(fragment),
        None => {
            crate::logging::debug("CF_HTML 解析失败，已降级为纯文本");
            None
        }
    }
}

/// 读取注册格式 "Rich Text Format" 的 RTF 原文（ASCII/ANSI 为常见情况，按 lossy 处理）。
pub(super) fn read_rtf() -> Option<String> {
    let format = register_format(html::RTF_FORMAT);
    if !is_available(format) {
        return None;
    }
    let bytes = read_clipboard_bytes(format, MAX_RTF_READ_BYTES)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_end_matches('\0');
    if text.trim().is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// 读取 CF_HDROP 文件列表（宽字符路径，支持多个文件）。
pub(super) fn read_hdrop() -> Vec<PathBuf> {
    if !is_available(CF_HDROP) {
        return Vec::new();
    }
    let handle = unsafe { GetClipboardData(CF_HDROP) };
    if handle.is_null() {
        return Vec::new();
    }
    let hdrop: HDROP = handle;
    // ifile = 0xFFFFFFFF → 返回文件数量
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }
    if count > MAX_FILES_READ {
        crate::logging::debug(&format!(
            "HDROP 文件数 {count} 超过读取上限 {MAX_FILES_READ}，只读前 {MAX_FILES_READ} 个"
        ));
    }
    let count = count.min(MAX_FILES_READ);
    let mut files = Vec::with_capacity(count as usize);
    for index in 0..count {
        // cch = 0 → 返回所需字符数（不含结尾 NUL）
        let len = unsafe { DragQueryFileW(hdrop, index, ptr::null_mut(), 0) };
        if len == 0 {
            continue;
        }
        let len = len.min(MAX_PATH_UNITS.saturating_sub(1));
        let mut buf = vec![0u16; len as usize + 1];
        let copied = unsafe { DragQueryFileW(hdrop, index, buf.as_mut_ptr(), buf.len() as u32) };
        if copied == 0 {
            continue;
        }
        let copied = copied.min(len);
        let path = String::from_utf16_lossy(&buf[..copied as usize]);
        if !path.is_empty() {
            files.push(PathBuf::from(path));
        }
    }
    files
}

/// 读取任意"内存块型"剪贴板格式的原始字节（PNG / DIB）。
pub(super) fn read_clipboard_bytes(format: u32, max_bytes: usize) -> Option<Vec<u8>> {
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return None;
    }
    let hglobal: HGLOBAL = handle;
    let size = unsafe { GlobalSize(hglobal) };
    if size == 0 || size > max_bytes {
        if size > max_bytes {
            crate::logging::warn(&format!(
                "剪贴板格式 {format} 数据过大（{size} 字节 > {max_bytes}），已跳过"
            ));
        }
        return None;
    }
    let lock = unsafe { GlobalLock(hglobal) };
    if lock.is_null() {
        crate::logging::debug(&format!("GlobalLock(格式 {format}) 失败，跳过"));
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(lock as *const u8, size) }.to_vec();
    let _ = unsafe { GlobalUnlock(hglobal) };
    Some(bytes)
}

// read.rs 本身不做单测：真实剪贴板读写冒烟在 `smoke.rs`（`#[ignore]`，手动运行）；
// 纯函数部分（格式优先级、字节解析）都在 clipboard.rs / dib.rs / write.rs 覆盖。

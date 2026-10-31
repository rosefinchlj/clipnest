//! 把 Store 的粘贴载荷写回系统剪贴板（只在 Sys 线程调用）。
//!
//! 写入格式（spec §12.1 / §12.2）：
//! - 文本：CF_UNICODETEXT（UTF-16LE，尾部 `\0`）；
//! - 图片：CF_DIB（32bpp BI_RGB、top-down、BGRA，A 固定 255）+ 注册格式 "PNG"（原始 PNG 字节）；
//! - 文件：CF_HDROP（DROPFILES + 宽字符路径列表，双 `\0` 结尾）；
//! - 最后（实际放在格式列表首位，保证即使后续格式失败也不会被自身采集）写
//!   [`SELF_MARKER_FORMAT`] 自写标记。
//!
//! 所有 HGLOBAL 都遵循：`GlobalAlloc(GMEM_MOVEABLE)` → `GlobalLock` → 拷贝 → `GlobalUnlock`
//! → `SetClipboardData`（成功后所有权交给系统，**不 Free**；失败则 `GlobalFree` 释放）。

use super::read::{register_format, ClipboardGuard};
use super::{
    dib, ClipError, ClipResult, CF_DIB, CF_HDROP, CF_UNICODETEXT, PNG_FORMAT, SELF_MARKER_FORMAT,
};
use crate::model::{ClipKind, PastePayload};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

/// 自写标记的数据前缀（后接 clip_id，便于诊断时识别来源条目）。
const SELF_MARKER_MAGIC: &[u8; 4] = b"CNSW";
/// DIB 头的固定大小（BITMAPINFOHEADER）。
const BITMAPINFOHEADER_SIZE: u32 = 40;
/// 回写图片解码的硬上限（图片来自自己的库，正常不会超；防止损坏文件导致巨量分配）。
const MAX_WRITE_PIXELS: u32 = 100_000_000;

// ---------------------------------------------------------------------------
// 对外 API
// ---------------------------------------------------------------------------

/// 把粘贴载荷写回系统剪贴板，并写自写标记避免自己的 WM_CLIPBOARDUPDATE 被再次采集。
///
/// `data_dir` 仅用于把相对的 `png_path` 解析成绝对路径（正常 Store 给的是绝对路径）。
pub fn write_clipboard(payload: &PastePayload, data_dir: &Path) -> ClipResult<()> {
    // 1) 先在剪贴板外准备好全部格式数据：解码/编码可能较慢，尽量缩短剪贴板被占用时间
    let formats = build_formats(payload, data_dir)?;

    // 2) 打开 → 清空 → 逐个写入（守卫 Drop 时 CloseClipboard）
    let _guard = ClipboardGuard::open()?;
    let ok = unsafe { EmptyClipboard() };
    if ok == 0 {
        return Err(last_win32("EmptyClipboard"));
    }
    for (format, bytes) in &formats {
        set_clipboard_bytes(*format, bytes)?;
    }
    crate::logging::debug(&format!(
        "剪贴板回写完成：kind={:?}，clip_id={}，格式数={}",
        payload.kind,
        payload.clip_id,
        formats.len()
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// 格式数据构建
// ---------------------------------------------------------------------------

/// 构建 `(格式 ID, 字节数据)` 列表；第一项固定为自写标记。
fn build_formats(payload: &PastePayload, data_dir: &Path) -> ClipResult<Vec<(u32, Vec<u8>)>> {
    let marker_format = register_format(SELF_MARKER_FORMAT);
    if marker_format == 0 {
        return Err(last_win32("RegisterClipboardFormatW(SELF_MARKER)"));
    }
    let mut formats: Vec<(u32, Vec<u8>)> =
        vec![(marker_format, self_marker_bytes(payload.clip_id))];

    match payload.kind {
        ClipKind::Text => {
            let text = payload
                .text
                .as_deref()
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ClipError::Win32("粘贴载荷缺少文本内容".into()))?;
            let bytes = encode_utf16_bytes(text);
            if bytes.len() <= 2 {
                return Err(ClipError::Win32("粘贴文本为空".into()));
            }
            formats.push((CF_UNICODETEXT, bytes));
        }
        ClipKind::Image => {
            let path = resolve_png_path(payload, data_dir)?;
            let png = std::fs::read(&path)?;
            if png.is_empty() {
                return Err(ClipError::Image(format!(
                    "图片文件为空: {}",
                    path.display()
                )));
            }
            let (width, height, rgba) = dib::decode_png_to_rgba(&png, MAX_WRITE_PIXELS)?;
            let dib_bytes = build_dib(&rgba, width, height)?;
            // 注册格式 "PNG" 放原始 PNG 字节；CF_DIB 作为只认 DIB 的老程序兜底
            let png_format = register_format(PNG_FORMAT);
            if png_format == 0 {
                return Err(last_win32("RegisterClipboardFormatW(PNG)"));
            }
            formats.push((png_format, png));
            formats.push((CF_DIB, dib_bytes));
        }
        ClipKind::Files => {
            let paths: Vec<PathBuf> = payload
                .files
                .iter()
                .map(|f| PathBuf::from(f.path.trim()))
                .filter(|p| !p.as_os_str().is_empty())
                .collect();
            if paths.is_empty() {
                return Err(ClipError::Win32("粘贴载荷缺少文件路径".into()));
            }
            formats.push((CF_HDROP, build_hdrop(&paths)?));
        }
    }
    Ok(formats)
}

/// 解析图片路径：优先绝对路径；相对路径按 `data_dir` 拼接；并检查文件存在。
fn resolve_png_path(payload: &PastePayload, data_dir: &Path) -> ClipResult<PathBuf> {
    let raw = payload
        .png_path
        .clone()
        .ok_or_else(|| ClipError::Win32("粘贴载荷缺少图片路径".into()))?;
    let path = if raw.is_absolute() {
        raw
    } else {
        data_dir.join(raw)
    };
    if !path.is_file() {
        return Err(ClipError::Win32(format!(
            "图片文件不存在: {}",
            path.display()
        )));
    }
    Ok(path)
}

/// UTF-16LE 编码并追加结尾 `\0`；顺带丢弃文本中的 NUL（剪贴板文本以 NUL 结尾）。
fn encode_utf16_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity((text.len() + 1) * 2);
    for unit in text.encode_utf16().filter(|&u| u != 0) {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// 自写标记内容：`"CNSW"` + clip_id (i64 LE)，共 12 字节。
fn self_marker_bytes(clip_id: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(SELF_MARKER_MAGIC.len() + 8);
    out.extend_from_slice(SELF_MARKER_MAGIC);
    out.extend_from_slice(&clip_id.to_le_bytes());
    out
}

// ---------------------------------------------------------------------------
// 纯函数：DIB / HDROP 字节构建（单元测试覆盖）
// ---------------------------------------------------------------------------

/// BITMAPINFOHEADER + 32bpp BI_RGB top-down 像素数据。
///
/// 像素按自上而下逐行、每像素 BGRA 排列；alpha 固定 255（DIB 无 alpha 语义，
/// 透明度由同时写入的注册格式 "PNG" 承载）。
pub(crate) fn build_dib(rgba: &[u8], width: u32, height: u32) -> ClipResult<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(ClipError::Image("图片尺寸为 0".into()));
    }
    let expected = width as u64 * height as u64 * 4;
    if expected != rgba.len() as u64 {
        return Err(ClipError::Image(format!(
            "RGBA 缓冲区长度 {} 与尺寸 {}×{} 不匹配",
            rgba.len(),
            width,
            height
        )));
    }
    let stride = width as usize * 4;
    let mut out = Vec::with_capacity(BITMAPINFOHEADER_SIZE as usize + stride * height as usize);
    // ---- BITMAPINFOHEADER（40 字节）----
    out.extend_from_slice(&BITMAPINFOHEADER_SIZE.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(-(height as i32)).to_le_bytes()); // 负高度 = top-down
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&((stride * height as usize) as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
                                                // ---- 像素：top-down，BGRA，A=255 ----
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    Ok(out)
}

/// DROPFILES 结构（20 字节，`#[repr(C)]`；字段全部显式按小端序列化，避免依赖内存布局）。
#[repr(C)]
#[derive(Clone, Copy)]
struct DropFiles {
    /// 文件列表相对结构体起始的字节偏移（= 结构体大小 20）。
    p_files: u32,
    pt_x: i32,
    pt_y: i32,
    /// 是否非客户区坐标（0）。
    f_nc: i32,
    /// 路径是否为宽字符（1 = UTF-16）。
    f_wide: i32,
}

/// CF_HDROP 数据：DROPFILES（20 字节）+ UTF-16LE 路径列表（每个路径以 `\0` 结尾，
/// 整个列表再以一个额外的 `\0` 结束，即"双 `\0` 结尾"）。
pub(crate) fn build_hdrop(paths: &[PathBuf]) -> ClipResult<Vec<u8>> {
    if paths.is_empty() {
        return Err(ClipError::Win32("文件列表为空".into()));
    }
    let header = DropFiles {
        p_files: std::mem::size_of::<DropFiles>() as u32,
        pt_x: 0,
        pt_y: 0,
        f_nc: 0,
        f_wide: 1,
    };
    let mut out = Vec::with_capacity(header.p_files as usize + paths.len() * 64);
    out.extend_from_slice(&header.p_files.to_le_bytes());
    out.extend_from_slice(&header.pt_x.to_le_bytes());
    out.extend_from_slice(&header.pt_y.to_le_bytes());
    out.extend_from_slice(&header.f_nc.to_le_bytes());
    out.extend_from_slice(&header.f_wide.to_le_bytes());

    for path in paths {
        let text = path.to_string_lossy();
        if text.is_empty() {
            return Err(ClipError::Win32("文件路径为空".into()));
        }
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    // 列表结尾的额外 \0（与最后一个路径的 \0 一起构成双 \0）
    out.extend_from_slice(&0u16.to_le_bytes());
    Ok(out)
}

// ---------------------------------------------------------------------------
// Win32 全局内存 / 剪贴板写入
// ---------------------------------------------------------------------------

/// 分配可移动全局内存并把 `bytes` 拷进去，然后交给剪贴板。
fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> ClipResult<()> {
    if format == 0 {
        return Err(ClipError::Win32("剪贴板格式 ID 为 0".into()));
    }
    if bytes.is_empty() {
        return Err(ClipError::Win32("剪贴板格式数据为空".into()));
    }
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if handle.is_null() {
            return Err(last_win32("GlobalAlloc"));
        }
        let lock = GlobalLock(handle);
        if lock.is_null() {
            // 不许泄漏全局内存
            GlobalFree(handle);
            return Err(last_win32("GlobalLock"));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), lock as *mut u8, bytes.len());
        // 与 GlobalLock 严格配对；返回值只表示是否需要 SetLastError，这里无需处理
        let _ = GlobalUnlock(handle);
        let result = SetClipboardData(format, handle);
        if result.is_null() {
            // 所有权未转移给系统 → 必须释放
            GlobalFree(handle);
            return Err(last_win32("SetClipboardData"));
        }
    }
    Ok(())
}

/// 构造带 Win32 错误码的 [`ClipError::Win32`]。
fn last_win32(what: &str) -> ClipError {
    ClipError::Win32(format!("{what} 失败: {}", std::io::Error::last_os_error()))
}

// ---------------------------------------------------------------------------
// 测试（纯函数，不依赖真实剪贴板）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FileEntry;

    #[test]
    fn dropfiles_struct_is_20_bytes() {
        assert_eq!(std::mem::size_of::<DropFiles>(), 20);
    }

    #[test]
    fn build_hdrop_layout_and_double_nul() {
        let paths = vec![PathBuf::from("C:\\a.txt"), PathBuf::from("D:\\目录\\b.bin")];
        let data = build_hdrop(&paths).expect("构造 HDROP");

        // DROPFILES 头部：pFiles=20，pt=(0,0)，fNC=0，fWide=1
        assert_eq!(u32::from_le_bytes(data[0..4].try_into().unwrap()), 20);
        assert_eq!(i32::from_le_bytes(data[4..8].try_into().unwrap()), 0);
        assert_eq!(i32::from_le_bytes(data[8..12].try_into().unwrap()), 0);
        assert_eq!(i32::from_le_bytes(data[12..16].try_into().unwrap()), 0);
        assert_eq!(i32::from_le_bytes(data[16..20].try_into().unwrap()), 1);

        // 解码 UTF-16 路径列表，验证以双 \0 结尾
        let units: Vec<u16> = data[20..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(&units[units.len() - 2..], &[0, 0], "必须以双 \\0 结尾");
        let expected: Vec<u16> = paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .flat_map(|s| {
                s.encode_utf16()
                    .chain(std::iter::once(0))
                    .collect::<Vec<u16>>()
            })
            .chain(std::iter::once(0))
            .collect();
        assert_eq!(units, expected);

        // 第三个文件路径段不复存在：整个列表里只有 2 个路径段
        let nul_positions: Vec<usize> = units
            .iter()
            .enumerate()
            .filter(|(_, &u)| u == 0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(nul_positions.len(), 3); // 两个路径各一个 + 列表结尾一个
    }

    #[test]
    fn build_hdrop_single_file_and_empty() {
        let data = build_hdrop(&[PathBuf::from("C:\\x")]).expect("单文件");
        // 20 字节头 + "C:\x"(4 字符)*2 + 2 字节 NUL + 2 字节列表 NUL = 20 + 8 + 2 + 2
        assert_eq!(data.len(), 32);
        assert_eq!(&data[data.len() - 4..], &[0, 0, 0, 0]);
        assert!(build_hdrop(&[]).is_err());
        assert!(build_hdrop(&[PathBuf::from("")]).is_err());
    }

    #[test]
    fn build_dib_header_and_pixels() {
        // 2×2 RGBA：左上红、右上绿、左下蓝、右下白
        let rgba: Vec<u8> = vec![
            255, 0, 0, 255, // (0,0) 红
            0, 255, 0, 255, // (1,0) 绿
            0, 0, 255, 255, // (0,1) 蓝
            255, 255, 255, 255, // (1,1) 白
        ];
        let dib = build_dib(&rgba, 2, 2).expect("构造 DIB");
        assert_eq!(dib.len(), 40 + 16);

        // 头部字段
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), -2); // top-down
        assert_eq!(u16::from_le_bytes(dib[12..14].try_into().unwrap()), 1); // planes
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32); // bpp
        assert_eq!(u32::from_le_bytes(dib[16..20].try_into().unwrap()), 0); // BI_RGB
        assert_eq!(u32::from_le_bytes(dib[20..24].try_into().unwrap()), 16); // sizeImage

        // 像素：BGRA，A=255，top-down 顺序与输入一致
        assert_eq!(&dib[40..44], &[0, 0, 255, 255]); // 红 → B=0,G=0,R=255
        assert_eq!(&dib[44..48], &[0, 255, 0, 255]); // 绿
        assert_eq!(&dib[48..52], &[255, 0, 0, 255]); // 蓝
        assert_eq!(&dib[52..56], &[255, 255, 255, 255]); // 白
    }

    #[test]
    fn build_dib_roundtrip_through_decoder() {
        let width = 5u32;
        let height = 3u32;
        let rgba: Vec<u8> = (0..(width * height * 4))
            .map(|i| (i * 13 % 251) as u8)
            .collect();
        let dib = build_dib(&rgba, width, height).expect("构造 DIB");
        let image = dib::decode_dib_to_rgba(&dib, 100).expect("回读 DIB");
        assert_eq!((image.width, image.height), (width, height));
        // 回读后 RGB 一致，alpha 被规范化为 255（DIB 不带 alpha）
        for (src, dst) in rgba.chunks_exact(4).zip(image.rgba.chunks_exact(4)) {
            assert_eq!(src[0..3], dst[0..3]);
            assert_eq!(dst[3], 255);
        }
    }

    #[test]
    fn build_dib_rejects_bad_input() {
        assert!(build_dib(&[], 0, 1).is_err());
        assert!(build_dib(&[], 1, 0).is_err());
        assert!(build_dib(&[0u8; 8], 2, 2).is_err()); // 长度不匹配
    }

    #[test]
    fn encode_utf16_adds_terminator_and_drops_nul() {
        let bytes = encode_utf16_bytes("a中");
        // 'a'(1) + '中'(1) + 结尾 NUL = 3 个单元 = 6 字节
        assert_eq!(bytes.len(), 6);
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 'a' as u16);
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), '中' as u16);
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 0);
        // 文本内部的 NUL 被丢弃，不会提前截断剪贴板文本
        let bytes = encode_utf16_bytes("a\0b");
        assert_eq!(bytes.len(), 6);
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 'b' as u16);
    }

    #[test]
    fn self_marker_payload_layout() {
        let marker = self_marker_bytes(42);
        assert_eq!(marker.len(), 12);
        assert_eq!(&marker[0..4], b"CNSW");
        assert_eq!(i64::from_le_bytes(marker[4..12].try_into().unwrap()), 42);
    }

    #[test]
    fn build_formats_orders_marker_first_and_rejects_missing_payload() {
        // 文本：标记 + CF_UNICODETEXT，标记必须在首位（保证部分失败时也不被自身采集）
        let payload = PastePayload {
            clip_id: 7,
            kind: ClipKind::Text,
            text: Some("你好".into()),
            png_path: None,
            files: Vec::new(),
        };
        let formats = build_formats(&payload, Path::new(".")).expect("构建文本格式");
        assert_eq!(formats.len(), 2);
        assert_eq!(formats[0].0, register_format(SELF_MARKER_FORMAT));
        assert_eq!(formats[1].0, CF_UNICODETEXT);
        // UTF-16LE："你好\0"
        assert_eq!(formats[1].1.len(), 6);

        // 缺文本 → Err
        let bad = PastePayload {
            text: None,
            ..payload.clone()
        };
        assert!(build_formats(&bad, Path::new(".")).is_err());

        // 文件：标记 + CF_HDROP
        let files_payload = PastePayload {
            clip_id: 8,
            kind: ClipKind::Files,
            text: None,
            png_path: None,
            files: vec![FileEntry {
                name: "a.txt".into(),
                path: "C:\\a.txt".into(),
                ..Default::default()
            }],
        };
        let formats = build_formats(&files_payload, Path::new(".")).expect("构建文件格式");
        assert_eq!(formats.len(), 2);
        assert_eq!(formats[1].0, CF_HDROP);
        // DROPFILES 头之后是路径 "C:\a.txt\0" 的 UTF-16LE + 列表结尾 \0
        let mut expect = Vec::new();
        for unit in "C:\\a.txt".encode_utf16() {
            expect.extend_from_slice(&unit.to_le_bytes());
        }
        expect.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(&formats[1].1[20..], expect.as_slice());

        // 缺路径 → Err
        let bad_files = PastePayload {
            files: Vec::new(),
            ..files_payload.clone()
        };
        assert!(build_formats(&bad_files, Path::new(".")).is_err());
    }
}

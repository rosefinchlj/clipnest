//! 真实剪贴板冒烟测试（默认忽略，手动运行）：
//!
//! ```text
//! cargo test clipboard::smoke -- --ignored --nocapture
//! ```
//!
//! 会短暂改写系统剪贴板；测试结束按"最佳努力"恢复原内容（文件 > 文本 > PNG > DIB，
//! 原剪贴板为空则清空）。原剪贴板含 `Clipboard Viewer Ignore`（敏感内容）时跳过；
//! 仅含本程序自写标记时继续（该内容可由本程序历史面板重新复制）。
//!
//! 常态单元测试不依赖真实剪贴板，本文件只在手动冒烟时运行。

use super::dib;
use super::*;
use crate::model::{ClipKind, FileEntry, PastePayload};
use std::path::{Path, PathBuf};

/// 冒烟测试用的固定 clip_id（与真实数据库无关，仅用于自写标记内容）。
const SMOKE_CLIP_ID_TEXT: i64 = 900_001;
const SMOKE_CLIP_ID_IMAGE: i64 = 900_002;
const SMOKE_CLIP_ID_FILES: i64 = 900_003;

/// 真实桌面上可能有 Ditto 等剪贴板管理器在收到更新后异步改写/追加剪贴板格式，
/// 回读因此采用"短暂重试"，避免把环境竞争误判为本域缺陷。
fn retry_option<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    const RETRY_TIMES: usize = 20;
    for attempt in 0..RETRY_TIMES {
        if let Some(value) = f() {
            return Some(value);
        }
        if attempt + 1 < RETRY_TIMES {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    None
}

/// 回写后确认自写标记生效（read_clipboard 应返回 ignore=true）。
fn assert_self_marker_ignored(context: &str) {
    let ignored =
        retry_option(|| read_clipboard().ok().filter(|raw| raw.ignore).map(|_| ())).is_some();
    assert!(
        ignored,
        "{context}：回写后 read_clipboard 应返回 ignore=true"
    );
}

/// 打开剪贴板后重试读取（真实桌面存在剪贴板管理器竞争，允许最多 20×10ms 重试）。
fn read_back<T>(mut read: impl FnMut() -> Option<T>) -> Option<T> {
    retry_option(|| {
        let _guard = read::ClipboardGuard::open().ok()?;
        read()
    })
}

/// 用真实系统剪贴板做 文本 / 图片（PNG+DIB）/ 文件（HDROP）三类往返冒烟。
#[test]
#[ignore = "会真实改写系统剪贴板，手动运行：cargo test clipboard::smoke -- --ignored --nocapture"]
fn real_clipboard_roundtrip() {
    // 0) 先备份原内容；剪贴板被占用或带忽略标记时安全跳过
    let original = match read_clipboard() {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!("[smoke] 剪贴板暂时不可用（{err}），跳过本次冒烟");
            return;
        }
    };
    if original.ignore {
        // 仅当存在 "Clipboard Viewer Ignore"（可能是密码管理器内容）时才跳过；
        // 若只是本程序自写标记（ClipNest 自己回写的内容），继续冒烟并清空恢复。
        let viewer_format = read::register_format(VIEWER_IGNORE_FORMAT);
        if read::is_available(viewer_format) {
            eprintln!("[smoke] 原剪贴板含 Clipboard Viewer Ignore 标记（敏感内容），跳过本次冒烟");
            return;
        }
        eprintln!("[smoke] 原剪贴板含自写标记（内容由本程序写入），继续冒烟并在结束时清空恢复");
    }
    let temp_dir = std::env::temp_dir().join(format!("clipnest-clip-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).expect("创建冒烟测试临时目录");
    // 守卫在 Drop 时恢复原剪贴板并清理临时文件（即使断言 panic 也会恢复）
    let _restorer = ClipboardRestorer {
        original,
        temp_dir: temp_dir.clone(),
    };

    smoke_text_roundtrip(&temp_dir);
    smoke_image_roundtrip(&temp_dir);
    smoke_files_roundtrip(&temp_dir);
}

fn smoke_text_roundtrip(temp_dir: &Path) {
    let text = format!(
        "ClipNest 冒烟测试 {} · 第二行 with ASCII",
        crate::model::now_ms()
    );
    let payload = PastePayload {
        clip_id: SMOKE_CLIP_ID_TEXT,
        kind: ClipKind::Text,
        text: Some(text.clone()),
        png_path: None,
        files: Vec::new(),
    };
    write_clipboard(&payload, temp_dir).expect("写文本到剪贴板");
    // 自写标记 → 读取阶段应直接忽略；内容校验走内部读取（绕过忽略逻辑）
    assert_self_marker_ignored("文本");
    let read_text = read_back(read::read_text).expect("读取 CF_UNICODETEXT");
    assert_eq!(
        read_text,
        text,
        "CF_UNICODETEXT 往返内容不一致（原长度 {}，读回 {}）",
        text.len(),
        read_text.len()
    );
    eprintln!("[smoke] 文本：写入 / 自写标记 / 读回 全部 OK");
}

fn smoke_image_roundtrip(temp_dir: &Path) {
    // 7×5 RGBA 渐变色，写入临时 PNG 文件作为粘贴载荷来源
    let width = 7u32;
    let height = 5u32;
    let rgba: Vec<u8> = (0..(width as usize * height as usize * 4))
        .map(|i| (i * 17 % 256) as u8)
        .collect();
    let png = dib::encode_png(&rgba, width, height).expect("生成冒烟测试 PNG");
    let png_path = temp_dir.join("smoke.png");
    std::fs::write(&png_path, &png).expect("写冒烟测试 PNG 文件");

    let payload = PastePayload {
        clip_id: SMOKE_CLIP_ID_IMAGE,
        kind: ClipKind::Image,
        text: None,
        png_path: Some(png_path),
        files: Vec::new(),
    };
    write_clipboard(&payload, temp_dir).expect("写图片到剪贴板");
    assert_self_marker_ignored("图片");

    let png_format = read::register_format(PNG_FORMAT);
    let read_png = read_back(|| read::read_clipboard_bytes(png_format, read::MAX_PNG_BYTES))
        .expect("读取注册格式 PNG");
    assert_eq!(read_png, png, "注册格式 PNG 字节往返不一致");

    let dib_bytes =
        read_back(|| read::read_clipboard_bytes(CF_DIB, read::MAX_DIB_BYTES)).expect("读取 CF_DIB");
    let image = dib::decode_dib_to_rgba(&dib_bytes, 100_000_000).expect("解析回写的 CF_DIB");
    assert_eq!(
        (image.width, image.height),
        (width, height),
        "DIB 尺寸不一致"
    );
    for (src, dst) in rgba.chunks_exact(4).zip(image.rgba.chunks_exact(4)) {
        assert_eq!(&src[0..3], &dst[0..3], "DIB 像素 RGB 往返不一致");
    }
    eprintln!("[smoke] 图片：PNG 注册格式 + CF_DIB 写入 / 读回 全部 OK");
}

fn smoke_files_roundtrip(temp_dir: &Path) {
    let file_a = temp_dir.join("冒烟 a.txt");
    let file_b = temp_dir.join("b.bin");
    std::fs::write(&file_a, b"clipnest smoke").expect("写冒烟文件 a");
    std::fs::write(&file_b, [1u8, 2, 3, 4]).expect("写冒烟文件 b");
    let files: Vec<FileEntry> = [&file_a, &file_b]
        .iter()
        .map(|path| FileEntry::from_path(path))
        .collect();

    let payload = PastePayload {
        clip_id: SMOKE_CLIP_ID_FILES,
        kind: ClipKind::Files,
        text: None,
        png_path: None,
        files,
    };
    write_clipboard(&payload, temp_dir).expect("写文件列表到剪贴板");
    assert_self_marker_ignored("文件");

    let paths = read_back(|| {
        let paths = read::read_hdrop();
        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    })
    .expect("读取 CF_HDROP");
    assert_eq!(
        paths,
        vec![file_a, file_b],
        "CF_HDROP 文件路径列表往返不一致"
    );
    eprintln!("[smoke] 文件：CF_HDROP 写入 / 读回 全部 OK");
}

// ---------------------------------------------------------------------------
// 剪贴板恢复（Drop 时执行，最佳努力）
// ---------------------------------------------------------------------------

/// 在 Drop 中恢复原剪贴板并清理临时目录；恢复失败只打印警告，不 panic（避免双 panic）。
struct ClipboardRestorer {
    original: RawClipboard,
    temp_dir: PathBuf,
}

impl Drop for ClipboardRestorer {
    fn drop(&mut self) {
        match restore_clipboard(&self.original, &self.temp_dir) {
            Ok(()) => eprintln!("[smoke] 已按最佳努力恢复原剪贴板内容"),
            Err(err) => eprintln!("[smoke] 警告：恢复原剪贴板失败：{err}"),
        }
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

/// 恢复优先级：文件 > 文本 > PNG > DIB；原剪贴板为空则清空。
fn restore_clipboard(original: &RawClipboard, temp_dir: &Path) -> ClipResult<()> {
    if !original.files.is_empty() {
        let files: Vec<FileEntry> = original
            .files
            .iter()
            .map(|path| FileEntry::from_path(path))
            .collect();
        return write_clipboard(
            &PastePayload {
                clip_id: 0,
                kind: ClipKind::Files,
                text: None,
                png_path: None,
                files,
            },
            temp_dir,
        );
    }
    if let Some(text) = original.text.as_deref().filter(|t| !t.is_empty()) {
        return write_clipboard(
            &PastePayload {
                clip_id: 0,
                kind: ClipKind::Text,
                text: Some(text.to_string()),
                png_path: None,
                files: Vec::new(),
            },
            temp_dir,
        );
    }
    if let Some(png) = original.png.as_deref() {
        return restore_image(png, temp_dir);
    }
    if let Some(dib) = original.dib.as_deref() {
        let image = dib::decode_dib_to_rgba(dib, 100_000_000)?;
        let png = dib::encode_png(&image.rgba, image.width, image.height)?;
        return restore_image(&png, temp_dir);
    }
    // 原剪贴板为空 → 清空恢复
    let _guard = read::ClipboardGuard::open()?;
    let ok = unsafe { windows_sys::Win32::System::DataExchange::EmptyClipboard() };
    if ok == 0 {
        return Err(ClipError::Win32("清空剪贴板失败".into()));
    }
    Ok(())
}

fn restore_image(png: &[u8], temp_dir: &Path) -> ClipResult<()> {
    std::fs::create_dir_all(temp_dir)?;
    let path = temp_dir.join("restore.png");
    std::fs::write(&path, png)?;
    write_clipboard(
        &PastePayload {
            clip_id: 0,
            kind: ClipKind::Image,
            text: None,
            png_path: Some(path),
            files: Vec::new(),
        },
        temp_dir,
    )
}

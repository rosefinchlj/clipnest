//! 剪贴板域：系统剪贴板读取、归一化、回写，以及 DIB ↔ PNG 编解码。
//!
//! 设计要点（spec §10.3 / §12 / §14，CONTRACT §3.2）：
//! - 读取（[`read_clipboard`]）只负责把原始格式取出：HDROP / CF_UNICODETEXT / 注册格式 "PNG" /
//!   CF_DIBV5 / CF_DIB；不做优先级判断，所有错误路径都关闭剪贴板、不泄漏全局内存。
//! - 归一化（[`normalize`]）按 **HDROP > 非空文本 > PNG > DIB** 的优先级挑一种内容，
//!   做上限检查/截断，计算 SHA-256（kind 前缀 + 内容）并组装 [`ClipInput`]。
//! - 回写（[`write_clipboard`]）只被 Sys 线程调用，负责把 Store 给的粘贴载荷写回剪贴板，
//!   并写入 [`SELF_MARKER_FORMAT`] 自写标记，避免自己的 WM_CLIPBOARDUPDATE 被再次采集。
//! - 本域不持有任何跨线程状态，日志只记类型/大小/哈希前缀，绝不记录剪贴板正文。

#![allow(dead_code)]

mod dib;
mod html;
mod read;
mod write;

/// 真实剪贴板冒烟测试（`#[ignore]`，手动运行；常态单测不依赖桌面）。
#[cfg(test)]
mod smoke;

use crate::model::{now_ms, ClipInput, ClipKind, FileEntry, Settings};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub use read::read_clipboard;
pub use write::write_clipboard;

/// 本程序回写剪贴板时附加的私有标记格式名；读取阶段发现它存在说明是自身写入，应忽略。
pub const SELF_MARKER_FORMAT: &str = "ClipNest.SelfWrite";

/// 密码管理器等敏感程序用来要求"不要记录"的格式名（spec §10.3 / §16）。
pub(crate) const VIEWER_IGNORE_FORMAT: &str = "Clipboard Viewer Ignore";

/// 图片的注册剪贴板格式名：内容已是 PNG 压缩数据。
pub(crate) const PNG_FORMAT: &str = "PNG";

// Windows 剪贴板标准格式常量。
// windows-sys 0.61 把这些常量放在需要 `Win32_System_Ole` feature 的模块里，而本项目
// 未启用该 feature（不擅自改 Cargo.toml），故按 Win32 SDK 固定数值本地定义。
pub(crate) const CF_UNICODETEXT: u32 = 13;
pub(crate) const CF_DIB: u32 = 8;
pub(crate) const CF_DIBV5: u32 = 17;
pub(crate) const CF_HDROP: u32 = 15;

/// 文本 preview 的最大字节数（spec §10.2：首 8KB 归一化文本）。
pub(crate) const PREVIEW_BYTES: usize = 8 * 1024;

/// 读取阶段，剪贴板上的原始内容（未归一化）。
///
/// 由 [`read_clipboard`] 填充；`png` 与 `dib` 只会保留其中一个（PNG 优先）。
#[derive(Clone, Debug, Default)]
pub struct RawClipboard {
    /// CF_UNICODETEXT 文本（已去掉尾部 `\0`，保留内部换行）。
    pub text: Option<String>,
    /// CF_HTML 解析出的 HTML fragment（未 sanitize，normalize 时统一清洗）。
    pub html: Option<String>,
    /// 注册格式 "Rich Text Format" 的 RTF 原文（未清洗）。
    pub rtf: Option<String>,
    /// 已是 PNG 编码的图片（注册格式 "PNG"），或由 DIB 转码前的占位（读取阶段只存原始字节）。
    pub png: Option<Vec<u8>>,
    /// 原始 CF_DIB/CF_DIBV5 字节（仅在无 PNG 时保留，由 normalize 转码）。
    pub dib: Option<Vec<u8>>,
    /// CF_HDROP 文件路径列表。
    pub files: Vec<PathBuf>,
    /// 含 `Clipboard Viewer Ignore` 或本程序自写标记 → 不应入库。
    pub ignore: bool,
}

/// 采集上限（来自用户设置；0 值视为"未配置"并用默认值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureLimits {
    /// 单条文本最大字节数（超出按字符边界截断）。
    pub max_text_bytes: u64,
    /// 单张图片最大像素数（宽 × 高；超出跳过）。
    pub max_image_pixels: u32,
    /// 单条文件列表最大文件数。
    pub max_files: usize,
}

impl CaptureLimits {
    /// 从用户设置构造；字段为 0（异常/旧配置缺失）时回退默认值。
    pub fn from_settings(settings: &Settings) -> Self {
        let d = Self::defaults();
        Self {
            max_text_bytes: if settings.max_text_bytes == 0 {
                d.max_text_bytes
            } else {
                settings.max_text_bytes
            },
            max_image_pixels: if settings.max_image_pixels == 0 {
                d.max_image_pixels
            } else {
                settings.max_image_pixels
            },
            max_files: if settings.max_files_per_clip == 0 {
                d.max_files
            } else {
                settings.max_files_per_clip
            },
        }
    }

    /// 默认上限（与 spec §20 附录 A / `Settings::default()` 一致）。
    pub fn defaults() -> Self {
        let s = Settings::default();
        Self {
            max_text_bytes: s.max_text_bytes,
            max_image_pixels: s.max_image_pixels,
            max_files: s.max_files_per_clip,
        }
    }
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self::defaults()
    }
}

/// 剪贴板域错误：不 panic，调用方（Sys 线程）记录日志后跳过本次采集/粘贴。
#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    #[error("剪贴板被占用")]
    Busy,
    #[error("Win32 错误: {0}")]
    Win32(String),
    #[error("图片处理失败: {0}")]
    Image(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

pub type ClipResult<T> = Result<T, ClipError>;

// ---------------------------------------------------------------------------
// 归一化
// ---------------------------------------------------------------------------

/// 把原始剪贴板内容归一化为 [`ClipInput`]。
///
/// 优先级：文件列表 > 非空文本 > PNG > DIB；以下情况返回 `None`（不写库）：
/// - 带忽略标记（密码管理器 / 本程序自写）；
/// - 纯空白文本且无图片；
/// - 图片像素超过 `limits.max_image_pixels`、或 PNG/DIB 解码失败；
/// - 所有格式都为空。
///
/// 文本超过 `limits.max_text_bytes` 时按 UTF-8 字符边界安全截断；
/// 文件数超过 `limits.max_files` 时取前 N 个（日志记录截断，不记录正文）。
pub fn normalize(raw: RawClipboard, limits: &CaptureLimits) -> Option<ClipInput> {
    if raw.ignore {
        crate::logging::debug("剪贴板带忽略/自写标记，跳过归一化");
        return None;
    }

    // 1) 文件列表优先（spec §10.3）
    if !raw.files.is_empty() {
        if let Some(input) = normalize_files(&raw.files, limits) {
            return Some(input);
        }
    }

    // 2) 非空文本（纯空白文本视作"无文本"，继续尝试图片）；
    //    富文本（CF_HTML / RTF）与文本同属文本类：纯文本为空时从 HTML 提取回退。
    if raw.text.is_some() || raw.html.is_some() || raw.rtf.is_some() {
        if let Some(input) = normalize_text_rich(
            raw.text.as_deref(),
            raw.html.as_deref(),
            raw.rtf.as_deref(),
            limits,
        ) {
            return Some(input);
        }
    }

    // 3) PNG（注册格式，已是压缩数据），失败再尝试 DIB 兜底
    if let Some(png) = raw.png.as_deref() {
        if let Some(input) = normalize_png(png, limits) {
            return Some(input);
        }
    }

    // 4) DIB → RGBA → PNG
    if let Some(dib) = raw.dib.as_deref() {
        if let Some(input) = normalize_dib(dib, limits) {
            return Some(input);
        }
    }

    None
}

/// 文件列表归一化：路径、名称、大小、存在性 → `FileEntry`，并计算 hash/preview。
fn normalize_files(raw_files: &[PathBuf], limits: &CaptureLimits) -> Option<ClipInput> {
    let max_files = limits.max_files.max(1);
    let files: Vec<FileEntry> = raw_files
        .iter()
        .take(max_files)
        .map(|path| FileEntry::from_path(path))
        .collect();
    if files.is_empty() {
        return None;
    }
    if raw_files.len() > files.len() {
        crate::logging::debug(&format!(
            "文件列表 {} 个超过上限 {}，已截断采集",
            raw_files.len(),
            max_files
        ));
    }

    let preview_src = files
        .iter()
        .map(|f| f.path.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let byte_len = files.iter().fold(0u64, |acc, f| acc.saturating_add(f.size));

    Some(ClipInput {
        kind: ClipKind::Files,
        hash: hash_files(files.iter().map(|f| f.path.as_str())),
        preview: preview_text(&preview_src),
        text: None,
        html: None,
        rtf: None,
        image_png: None,
        files,
        byte_len,
        width: None,
        height: None,
        source_app: None,
        source_title: None,
        captured_at: now_ms(),
    })
}

/// 文本（含富文本）归一化：去尾部 `\0`、忽略纯空白、按上限截断、计算 hash/preview。
///
/// 富文本规则（B1）：
/// - HTML 先 sanitize（脚本/事件/外链过滤）再存储；RTF 只做基本控制字过滤；
/// - 纯文本为空（例如从网页复制了带格式图片说明）时，从 HTML 提取纯文本作为
///   `text`/preview 回退；两者都为空时返回 `None`，让图片分支继续；
/// - 没有任何 html/rtf 时行为与原先完全一致。
fn normalize_text_rich(
    text: Option<&str>,
    html: Option<&str>,
    rtf: Option<&str>,
    limits: &CaptureLimits,
) -> Option<ClipInput> {
    // 读取阶段已去尾部 \0；这里再兜底一次（测试/其它调用方可能直接构造 RawClipboard）
    let text = text.unwrap_or_default().trim_end_matches('\0');
    let sanitized_html = html.and_then(html::sanitize_html);
    let sanitized_rtf = rtf.and_then(html::sanitize_rtf);
    // 纯文本为空时从 HTML 提取回退文本（标签结构在 text 里没有意义）
    let body = if !text.trim().is_empty() {
        text.to_string()
    } else {
        sanitized_html
            .as_deref()
            .map(html::plain_text_from_html)
            .unwrap_or_default()
    };
    if body.trim().is_empty() {
        return None;
    }
    let original_len = body.len();
    let (body, truncated) = truncate_text(&body, limits.max_text_bytes);
    if body.trim().is_empty() {
        // 上限过小，截断后没有有效内容
        return None;
    }
    if truncated {
        // 只记录长度，不记录正文（隐私要求）
        crate::logging::warn(&format!(
            "文本超过上限 {} 字节（原长 {}），已截断",
            limits.max_text_bytes, original_len
        ));
    }
    let byte_len = body.len() as u64;
    Some(ClipInput {
        kind: ClipKind::Text,
        hash: hash_text(&body),
        preview: preview_text(&body),
        text: Some(body),
        html: sanitized_html,
        rtf: sanitized_rtf,
        image_png: None,
        files: Vec::new(),
        byte_len,
        width: None,
        height: None,
        source_app: None,
        source_title: None,
        captured_at: now_ms(),
    })
}

/// 注册格式 "PNG" 的归一化：校验可解码 + 像素上限，原样保留 PNG 字节。
fn normalize_png(png: &[u8], limits: &CaptureLimits) -> Option<ClipInput> {
    if png.is_empty() {
        return None;
    }
    match dib::validate_png(png, limits.max_image_pixels) {
        Ok((width, height)) => Some(image_input(png.to_vec(), width, height)),
        Err(err) => {
            crate::logging::debug(&format!("剪贴板 PNG 无效，已跳过：{err}"));
            None
        }
    }
}

/// CF_DIB/CF_DIBV5 的归一化：解析 → RGBA → PNG 编码。
fn normalize_dib(dib: &[u8], limits: &CaptureLimits) -> Option<ClipInput> {
    if dib.is_empty() {
        return None;
    }
    let image = match dib::decode_dib_to_rgba(dib, limits.max_image_pixels) {
        Ok(image) => image,
        Err(err) => {
            crate::logging::warn(&format!("剪贴板 DIB 解析失败，已跳过：{err}"));
            return None;
        }
    };
    let png = match dib::encode_png(&image.rgba, image.width, image.height) {
        Ok(png) => png,
        Err(err) => {
            crate::logging::warn(&format!("剪贴板图片 PNG 编码失败，已跳过：{err}"));
            return None;
        }
    };
    Some(image_input(png, image.width, image.height))
}

/// 用已编码好的 PNG 字节构造图片 `ClipInput`。
fn image_input(png: Vec<u8>, width: u32, height: u32) -> ClipInput {
    let byte_len = png.len() as u64;
    ClipInput {
        kind: ClipKind::Image,
        hash: hash_image(&png),
        preview: format!(
            "图片 {}×{} · {}",
            width,
            height,
            crate::model::fmt_size(byte_len)
        ),
        text: None,
        html: None,
        rtf: None,
        image_png: Some(png),
        files: Vec::new(),
        byte_len,
        width: Some(width),
        height: Some(height),
        source_app: None,
        source_title: None,
        captured_at: now_ms(),
    }
}

// ---------------------------------------------------------------------------
// 哈希（SHA-256，"kind 前缀 + 归一化内容"）
// ---------------------------------------------------------------------------

/// 按 [`ClipInput`] 的 kind 与内容重算哈希（与 normalize 内部算法一致，测试/诊断用）。
pub fn hash_input(input: &ClipInput) -> String {
    match input.kind {
        ClipKind::Text => hash_text(input.text.as_deref().unwrap_or("")),
        ClipKind::Image => hash_image(input.image_png.as_deref().unwrap_or(&[])),
        ClipKind::Files => hash_files(input.files.iter().map(|f| f.path.as_str())),
    }
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"text:");
    hasher.update(text.as_bytes());
    to_hex(&hasher.finalize())
}

fn hash_image(png: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"image:");
    hasher.update(png);
    to_hex(&hasher.finalize())
}

/// 文件列表哈希：逐路径做 `长度前缀 + 归一化路径`，避免路径拼接歧义。
/// 归一化 = 分隔符统一为 `\`、ASCII 小写（Windows 路径不区分大小写，同文件不同写法可去重）。
fn hash_files<'a>(paths: impl Iterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"files:");
    for path in paths {
        let normalized = normalize_path_for_hash(path);
        hasher.update((normalized.len() as u32).to_le_bytes());
        hasher.update(normalized.as_bytes());
    }
    to_hex(&hasher.finalize())
}

fn normalize_path_for_hash(path: &str) -> String {
    let mut out = path.replace('/', "\\");
    out.make_ascii_lowercase();
    out
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

// ---------------------------------------------------------------------------
// 文本截断 / preview
// ---------------------------------------------------------------------------

/// 按 UTF-8 字符边界把文本截断到 `max_bytes`，返回 (截断结果, 是否发生截断)。
fn truncate_text(text: &str, max_bytes: u64) -> (String, bool) {
    if text.len() as u64 <= max_bytes {
        return (text.to_string(), false);
    }
    let mut cut = (max_bytes.min(usize::MAX as u64)) as usize;
    cut = cut.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_string(), true)
}

/// preview：取前 [`PREVIEW_BYTES`] 字节（字符边界安全），供 DB 搜索/列表使用。
fn preview_text(text: &str) -> String {
    if text.len() <= PREVIEW_BYTES {
        return text.to_string();
    }
    let mut cut = PREVIEW_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

// ---------------------------------------------------------------------------
// 测试（不依赖真实剪贴板）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Settings;

    /// 生成一张合法 PNG（用于归一化测试）。
    fn sample_png(width: u32, height: u32) -> Vec<u8> {
        let rgba: Vec<u8> = (0..(width as usize * height as usize * 4))
            .map(|i| (i % 251) as u8)
            .collect();
        dib::encode_png(&rgba, width, height).expect("测试用 PNG 编码应成功")
    }

    fn raw_text(text: &str) -> RawClipboard {
        RawClipboard {
            text: Some(text.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn limits_from_settings_and_defaults() {
        let s = Settings::default();
        let limits = CaptureLimits::from_settings(&s);
        assert_eq!(limits.max_text_bytes, 2 * 1024 * 1024);
        assert_eq!(limits.max_image_pixels, 40_000_000);
        assert_eq!(limits.max_files, 100);
        assert_eq!(limits, CaptureLimits::defaults());

        // 0 值视为未配置 → 回退默认
        let mut zero = Settings::default();
        zero.max_text_bytes = 0;
        zero.max_image_pixels = 0;
        zero.max_files_per_clip = 0;
        assert_eq!(
            CaptureLimits::from_settings(&zero),
            CaptureLimits::defaults()
        );
    }

    #[test]
    fn normalize_priority_files_over_text_and_image() {
        let exe = std::env::current_exe().expect("当前测试可执行文件路径");
        let raw = RawClipboard {
            files: vec![exe],
            text: Some("普通文本".into()),
            png: Some(sample_png(2, 2)),
            ..Default::default()
        };
        let input = normalize(raw, &CaptureLimits::defaults()).expect("应有结果");
        assert_eq!(input.kind, ClipKind::Files);
        assert_eq!(input.files.len(), 1);
        assert!(input.files[0].size > 0);
        assert!(input.text.is_none());
        assert!(input.image_png.is_none());
        assert_eq!(input.hash, hash_input(&input));
    }

    #[test]
    fn normalize_text_over_image() {
        let raw = RawClipboard {
            text: Some("第一行\n第二行".into()),
            png: Some(sample_png(2, 2)),
            ..Default::default()
        };
        let input = normalize(raw, &CaptureLimits::defaults()).expect("应有结果");
        assert_eq!(input.kind, ClipKind::Text);
        // 内部换行保留
        assert_eq!(input.text.as_deref(), Some("第一行\n第二行"));
        assert_eq!(input.byte_len, "第一行\n第二行".len() as u64);
        assert_eq!(input.preview, "第一行\n第二行");
    }

    #[test]
    fn normalize_whitespace_text_falls_back_to_image() {
        let png = sample_png(3, 2);
        let raw = RawClipboard {
            text: Some(" \t\r\n\u{0}\u{0}".into()),
            png: Some(png.clone()),
            ..Default::default()
        };
        let input = normalize(raw, &CaptureLimits::defaults()).expect("应回退到图片");
        assert_eq!(input.kind, ClipKind::Image);
        assert_eq!(input.width, Some(3));
        assert_eq!(input.height, Some(2));
        assert!(input.image_png.is_some());
        assert_eq!(input.byte_len, png.len() as u64);
        assert!(input.preview.starts_with("图片 3×2"));
    }

    #[test]
    fn normalize_empty_and_ignore() {
        assert!(normalize(RawClipboard::default(), &CaptureLimits::defaults()).is_none());
        let mut raw = raw_text("secret");
        raw.ignore = true;
        assert!(normalize(raw, &CaptureLimits::defaults()).is_none());
    }

    #[test]
    fn normalize_text_truncates_on_char_boundary() {
        // "中" 占 3 字节：上限 4 字节只能放下 1 个汉字
        let text = "中文字符串";
        let limits = CaptureLimits {
            max_text_bytes: 4,
            ..CaptureLimits::defaults()
        };
        let input = normalize(raw_text(text), &limits).expect("应有结果");
        assert_eq!(input.text.as_deref(), Some("中"));
        assert_eq!(input.byte_len, 3);
    }

    #[test]
    fn normalize_text_under_limit_not_truncated() {
        let text = "短文本";
        let limits = CaptureLimits {
            max_text_bytes: 1024,
            ..CaptureLimits::defaults()
        };
        let input = normalize(raw_text(text), &limits).expect("应有结果");
        assert_eq!(input.text.as_deref(), Some(text));
    }

    #[test]
    fn normalize_files_truncated_to_limit() {
        let raw = RawClipboard {
            files: vec![
                PathBuf::from("C:\\a.txt"),
                PathBuf::from("C:\\b.txt"),
                PathBuf::from("C:\\c.txt"),
            ],
            ..Default::default()
        };
        let limits = CaptureLimits {
            max_files: 2,
            ..CaptureLimits::defaults()
        };
        let input = normalize(raw, &limits).expect("应有结果");
        assert_eq!(input.kind, ClipKind::Files);
        assert_eq!(input.files.len(), 2);
        assert_eq!(input.files[1].name, "b.txt");
    }

    #[test]
    fn normalize_png_rejects_oversized_image() {
        let png = sample_png(100, 100); // 10000 像素
        let limits = CaptureLimits {
            max_image_pixels: 1000,
            ..CaptureLimits::defaults()
        };
        assert!(normalize(
            RawClipboard {
                png: Some(png),
                ..Default::default()
            },
            &limits
        )
        .is_none());
    }

    #[test]
    fn normalize_png_rejects_corrupt_data_then_uses_dib() {
        let png = sample_png(2, 2);
        let rgba = vec![
            10u8, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
        ];
        let dib = super::write::build_dib(&rgba, 2, 2).expect("构造 DIB");

        // 坏 PNG + 合法 DIB → 回退 DIB
        let raw = RawClipboard {
            png: Some(b"not a png".to_vec()),
            dib: Some(dib),
            ..Default::default()
        };
        let input = normalize(raw, &CaptureLimits::defaults()).expect("应回退到 DIB");
        assert_eq!(input.kind, ClipKind::Image);
        assert_eq!((input.width, input.height), (Some(2), Some(2)));

        // 只有坏 PNG → None
        assert!(normalize(
            RawClipboard {
                png: Some(b"broken".to_vec()),
                ..Default::default()
            },
            &CaptureLimits::defaults()
        )
        .is_none());

        // 原样保留合法 PNG 字节
        let raw = RawClipboard {
            png: Some(png.clone()),
            ..Default::default()
        };
        let input = normalize(raw, &CaptureLimits::defaults()).expect("合法 PNG");
        assert_eq!(input.image_png.as_deref(), Some(png.as_slice()));
    }

    #[test]
    fn normalize_dib_rejects_oversized() {
        let rgba = vec![0u8; 100 * 100 * 4];
        let dib = super::write::build_dib(&rgba, 100, 100).expect("构造 DIB");
        let limits = CaptureLimits {
            max_image_pixels: 100,
            ..CaptureLimits::defaults()
        };
        assert!(normalize(
            RawClipboard {
                dib: Some(dib),
                ..Default::default()
            },
            &limits
        )
        .is_none());
    }

    #[test]
    fn hash_is_stable_and_kind_scoped() {
        let text = "稳定性测试 abc";
        let a = normalize(raw_text(text), &CaptureLimits::defaults()).unwrap();
        let b = normalize(raw_text(text), &CaptureLimits::defaults()).unwrap();
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.hash.len(), 64); // SHA-256 hex
        assert_eq!(a.hash, hash_input(&a));

        // 文本与文件列表的哈希前缀不同：同字面内容也不会冲突
        let file_input = ClipInput {
            kind: ClipKind::Files,
            text: None,
            files: vec![FileEntry {
                name: "x".into(),
                path: text.into(),
                size: 0,
                exists: false,
                stored: false,
            }],
            ..text_input_stub()
        };
        assert_ne!(a.hash, hash_input(&file_input));
        // 大小写不同但语义相同的 Windows 路径哈希一致
        let f1 = ClipInput {
            kind: ClipKind::Files,
            files: vec![FileEntry {
                name: "a".into(),
                path: "C:\\Temp\\A.TXT".into(),
                size: 0,
                exists: false,
                stored: false,
            }],
            ..text_input_stub()
        };
        let f2 = ClipInput {
            kind: ClipKind::Files,
            files: vec![FileEntry {
                path: "c:/temp/a.txt".into(),
                ..f1.files[0].clone()
            }],
            ..text_input_stub()
        };
        assert_eq!(hash_input(&f1), hash_input(&f2));
    }

    /// 构造一个只用于填充字段的 ClipInput 骨架。
    fn text_input_stub() -> ClipInput {
        ClipInput {
            kind: ClipKind::Text,
            hash: String::new(),
            preview: String::new(),
            text: Some(String::new()),
            html: None,
            rtf: None,
            image_png: None,
            files: Vec::new(),
            byte_len: 0,
            width: None,
            height: None,
            source_app: None,
            source_title: None,
            captured_at: 0,
        }
    }

    #[test]
    fn preview_is_capped_at_8kb() {
        let text = "a".repeat(PREVIEW_BYTES * 2);
        let input = normalize(raw_text(&text), &CaptureLimits::defaults()).unwrap();
        assert_eq!(input.preview.len(), PREVIEW_BYTES);
    }

    #[test]
    fn normalize_rich_text_sanitizes_html_and_keeps_rtf() {
        let raw = RawClipboard {
            text: Some("纯文本".into()),
            html: Some(r#"<p onclick="evil()">纯文本<script>evil()</script></p>"#.into()),
            rtf: Some(r"{\rtf1 纯文本{\object\objdata 0102}}".into()),
            ..Default::default()
        };
        let input = normalize(raw, &CaptureLimits::defaults()).expect("应有结果");
        assert_eq!(input.kind, ClipKind::Text);
        assert_eq!(input.text.as_deref(), Some("纯文本"));
        let html = input.html.expect("应保留 HTML");
        assert!(html.contains("<p>纯文本</p>"), "html={html}");
        assert!(!html.contains("script") && !html.contains("onclick"));
        let rtf = input.rtf.expect("应保留 RTF");
        assert!(!rtf.contains("objdata"), "rtf={rtf}");
        assert!(rtf.contains("纯文本"));
    }

    #[test]
    fn normalize_html_only_falls_back_to_plain_text() {
        let raw = RawClipboard {
            text: Some(" \r\n".into()),
            html: Some("<p>来自网页</p><div>第二段</div>".into()),
            ..Default::default()
        };
        let input = normalize(raw, &CaptureLimits::defaults()).expect("应从 HTML 提取文本");
        assert_eq!(input.kind, ClipKind::Text);
        let text = input.text.expect("text 回退");
        assert!(text.contains("来自网页") && text.contains("第二段"), "text={text}");
        assert_eq!(input.preview, preview_text(&text));
        assert!(input.html.is_some());
    }

    #[test]
    fn normalize_without_rich_text_is_unchanged() {
        let input = normalize(raw_text("旧行为"), &CaptureLimits::defaults()).unwrap();
        assert!(input.html.is_none());
        assert!(input.rtf.is_none());
        assert_eq!(input.byte_len, "旧行为".len() as u64);
    }

    #[test]
    fn normalize_script_only_html_falls_through_to_image() {
        let png = sample_png(2, 2);
        let raw = RawClipboard {
            html: Some("<script>evil()</script>".into()),
            png: Some(png),
            ..Default::default()
        };
        let input = normalize(raw, &CaptureLimits::defaults()).expect("应回退到图片");
        assert_eq!(input.kind, ClipKind::Image);
        assert!(input.html.is_none());
    }

    #[test]
    fn text_preview_respects_char_boundary() {
        // 每个汉字 3 字节，8KB 截断点必须落在字符边界
        let text = "中".repeat(PREVIEW_BYTES);
        let input = normalize(raw_text(&text), &CaptureLimits::defaults()).unwrap();
        assert!(input.preview.len() <= PREVIEW_BYTES);
        assert!(input.preview.chars().all(|c| c == '中'));
    }
}

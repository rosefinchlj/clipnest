//! 共享数据模型、设置与线程间消息类型。
//!
//! 这是四个域（store / clipboard / sys / ui）唯一的公共语言：
//! - 任何线程都不得直接传递 UI 类型或平台句柄（HWND 以 `isize` 传递）；
//! - 事件（Event）由后台线程发往 UI；命令（StoreCmd/SysCmd）由 UI 发往后台；
//! - `Settings` 通过 `Arc<Mutex<Settings>>` 共享（唯一允许的跨线程可变状态）。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 应用名（注册表值名、单实例名、窗口类名前缀）。
pub const APP_NAME: &str = "ClipNest";
/// 窗口标题。
pub const APP_TITLE: &str = "ClipNest · 剪贴匣";
/// 当前数据库 schema 版本。
pub const SCHEMA_VERSION: i64 = 1;

pub type ClipId = i64;

/// 当前 unix 毫秒时间戳。
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------------
// 剪贴板条目
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[repr(i32)]
pub enum ClipKind {
    #[default]
    Text = 0,
    Image = 1,
    Files = 2,
}

impl ClipKind {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(ClipKind::Text),
            1 => Some(ClipKind::Image),
            2 => Some(ClipKind::Files),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ClipKind::Text => "文本",
            ClipKind::Image => "图片",
            ClipKind::Files => "文件",
        }
    }

}

/// 文件列表中的一项。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub exists: bool,
    /// 是否已在数据目录中保存实体副本（仅收藏项会置 true）。
    pub stored: bool,
}

impl FileEntry {
    pub fn from_path(path: &std::path::Path) -> Self {
        let meta = std::fs::metadata(path).ok();
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        Self {
            name,
            path: path.to_string_lossy().to_string(),
            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            exists: meta.is_some(),
            stored: false,
        }
    }

    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            std::path::Path::new(&self.path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| self.path.clone())
        } else {
            self.name.clone()
        }
    }
}

/// 一条剪贴板历史（UI/详情展示用）。
#[derive(Clone, Debug, Default)]
pub struct ClipItem {
    pub id: ClipId,
    pub kind: ClipKind,
    pub preview: String,
    pub text: Option<String>,
    pub files: Vec<FileEntry>,
    pub blob_id: Option<String>,
    pub byte_len: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub created_at: i64,
    pub last_used_at: i64,
    pub use_count: u32,
    pub pinned: bool,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
}

impl ClipItem {
    /// 列表标题：单行、紧凑。
    pub fn title(&self) -> String {
        match self.kind {
            ClipKind::Text => {
                let first = self
                    .text
                    .as_deref()
                    .unwrap_or(self.preview.as_str())
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("(空文本)");
                truncate_chars(first.trim(), 80)
            }
            ClipKind::Image => format!(
                "图片 {}×{}",
                self.width.unwrap_or(0),
                self.height.unwrap_or(0)
            ),
            ClipKind::Files => {
                let first = self
                    .files
                    .first()
                    .map(FileEntry::display_name)
                    .unwrap_or_else(|| "(文件)".to_string());
                if self.files.len() > 1 {
                    format!("{first} 等 {} 个文件", self.files.len())
                } else {
                    first
                }
            }
        }
    }

    /// 列表第二行：单行摘要。
    pub fn subtitle(&self) -> String {
        match self.kind {
            ClipKind::Text => {
                let full = self.text.as_deref().unwrap_or(self.preview.as_str());
                let mut lines = full.lines().filter(|l| !l.trim().is_empty());
                let _ = lines.next();
                let rest: String = lines.collect::<Vec<_>>().join(" ");
                if rest.trim().is_empty() {
                    format!("{} 字", full.chars().count())
                } else {
                    truncate_chars(rest.trim(), 120)
                }
            }
            ClipKind::Image => format!(
                "{} · {}×{}",
                fmt_size(self.byte_len),
                self.width.unwrap_or(0),
                self.height.unwrap_or(0)
            ),
            ClipKind::Files => {
                if self.files.len() == 1 {
                    format!("{} · {}", self.files[0].path, fmt_size(self.files[0].size))
                } else {
                    truncate_chars(
                        &self
                            .files
                            .iter()
                            .map(FileEntry::display_name)
                            .collect::<Vec<_>>()
                            .join("、"),
                        120,
                    )
                }
            }
        }
    }

}

/// 采集后送入 Store 的归一化输入（由 Sys 线程构造）。
#[derive(Clone, Debug)]
pub struct ClipInput {
    pub kind: ClipKind,
    /// SHA-256(kind + 归一化内容)。
    pub hash: String,
    pub preview: String,
    pub text: Option<String>,
    /// 图片的 PNG 字节（Store 负责落盘）。
    pub image_png: Option<Vec<u8>>,
    pub files: Vec<FileEntry>,
    pub byte_len: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    pub captured_at: i64,
}

/// 回写剪贴板所需的载荷（由 Store 提供 blob 绝对路径）。
#[derive(Clone, Debug, Default)]
pub struct PastePayload {
    pub clip_id: ClipId,
    pub kind: ClipKind,
    pub text: Option<String>,
    /// 图片文件路径（PNG）。
    pub png_path: Option<PathBuf>,
    pub files: Vec<FileEntry>,
}

// ---------------------------------------------------------------------------
// 查询 / 过滤
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ClipFilter {
    #[default]
    All,
    Text,
    Image,
    Files,
    Pinned,
}

impl ClipFilter {
    pub fn label(self) -> &'static str {
        match self {
            ClipFilter::All => "全部",
            ClipFilter::Text => "文本",
            ClipFilter::Image => "图片",
            ClipFilter::Files => "文件",
            ClipFilter::Pinned => "收藏",
        }
    }

    pub fn all() -> [ClipFilter; 5] {
        [
            ClipFilter::All,
            ClipFilter::Text,
            ClipFilter::Image,
            ClipFilter::Files,
            ClipFilter::Pinned,
        ]
    }

    pub fn next(self) -> ClipFilter {
        match self {
            ClipFilter::All => ClipFilter::Text,
            ClipFilter::Text => ClipFilter::Image,
            ClipFilter::Image => ClipFilter::Files,
            ClipFilter::Files => ClipFilter::Pinned,
            ClipFilter::Pinned => ClipFilter::All,
        }
    }

    pub fn prev(self) -> ClipFilter {
        match self {
            ClipFilter::All => ClipFilter::Pinned,
            ClipFilter::Pinned => ClipFilter::Files,
            ClipFilter::Files => ClipFilter::Image,
            ClipFilter::Image => ClipFilter::Text,
            ClipFilter::Text => ClipFilter::All,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchQuery {
    pub text: String,
    pub filter: ClipFilter,
    pub offset: usize,
    pub limit: usize,
    /// 请求代号：UI 丢弃过期代号的返回结果。
    pub gen: u64,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            filter: ClipFilter::All,
            offset: 0,
            limit: 200,
            gen: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SearchPage {
    pub gen: u64,
    pub total: usize,
    pub items: Vec<ClipItem>,
    /// 查询耗时（毫秒），用于诊断/性能验收。
    pub elapsed_ms: u64,
}

/// 容量/统计信息。
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub total: usize,
    pub text: usize,
    pub image: usize,
    pub files: usize,
    pub pinned: usize,
    pub image_bytes: u64,
    pub favorite_file_bytes: u64,
    pub db_bytes: u64,
}

// ---------------------------------------------------------------------------
// 快捷键
// ---------------------------------------------------------------------------

/// 一个键盘组合：修饰键 + 虚拟键码（Windows VK_*）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HotkeyChord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u32,
}

impl Default for HotkeyChord {
    fn default() -> Self {
        // 默认备用快捷键 Ctrl+`（VK_OEM_3）
        Self {
            ctrl: true,
            alt: false,
            shift: false,
            win: false,
            vk: 0xC0,
        }
    }
}

impl HotkeyChord {

    pub fn new(ctrl: bool, alt: bool, shift: bool, win: bool, vk: u32) -> Self {
        Self {
            ctrl,
            alt,
            shift,
            win,
            vk,
        }
    }

    pub fn is_set(&self) -> bool {
        self.vk != 0
    }

    pub fn is_win_v(&self) -> bool {
        self.win && !self.ctrl && !self.alt && !self.shift && self.vk == 0x56
    }

    /// 显示文本，例如 `Ctrl+Shift+V`、`Win+V`、`Ctrl+\``。
    pub fn label(&self) -> String {
        if !self.is_set() {
            return "未设置".to_string();
        }
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.win {
            parts.push("Win");
        }
        let key = vk_label(self.vk);
        if parts.is_empty() {
            key
        } else {
            format!("{}+{}", parts.join("+"), key)
        }
    }
}

/// 虚拟键码 → 显示名（覆盖常用键；未知键显示十六进制）。
pub fn vk_label(vk: u32) -> String {
    match vk {
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x25 => "←".into(),
        0x26 => "↑".into(),
        0x27 => "→".into(),
        0x28 => "↓".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x21 => "PgUp".into(),
        0x22 => "PgDn".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0xC0 => "`".into(),
        0xBD => "-".into(),
        0xBB => "=".into(),
        0xDB => "[".into(),
        0xDD => "]".into(),
        0xDC => "\\".into(),
        0xBA => ";".into(),
        0xDE => "'".into(),
        0xBC => ",".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        v if (0x30..=0x39).contains(&v) => char::from_u32(v).unwrap_or('?').to_string(),
        v if (0x41..=0x5A).contains(&v) => char::from_u32(v).unwrap_or('?').to_string(),
        v if (0x70..=0x87).contains(&v) => format!("F{}", v - 0x6F),
        _ => format!("VK_{vk:02X}"),
    }
}

// ---------------------------------------------------------------------------
// 行为设置
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteKey {
    #[default]
    CtrlV,
    ShiftInsert,
}

/// 外观皮肤（深色磨砂玻璃 / 浅色）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Dark => "深色",
            ThemeMode::Light => "浅色",
        }
    }
}

impl PasteKey {
    pub fn label(self) -> &'static str {
        match self {
            PasteKey::CtrlV => "Ctrl+V",
            PasteKey::ShiftInsert => "Shift+Insert",
        }
    }
}

/// 用户设置（持久化到 settings.json；未知字段忽略、缺失字段取默认值）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 外观皮肤：深色 / 浅色。
    pub theme: ThemeMode,
    /// 备用全局快捷键（Win+V 被接管时仍保留）。
    pub hotkey_picker: HotkeyChord,
    pub pause_capture: bool,
    /// 忽略的应用进程名（小写、含 .exe）。
    pub ignore_apps: Vec<String>,

    pub max_items: usize,
    pub max_image_bytes: u64,
    pub max_favorite_file_bytes: u64,
    pub max_text_bytes: u64,
    pub max_file_bytes: u64,
    pub max_files_per_clip: usize,
    pub max_image_pixels: u32,

    pub picker_size: [f32; 2],
    pub manager_size: [f32; 2],
    pub hide_on_blur: bool,

    pub keep_clipboard_after_paste: bool,
    pub move_to_top_after_paste: bool,
    pub paste_key: PasteKey,

    pub autostart: bool,
    pub log_level: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            hotkey_picker: HotkeyChord::default(),
            pause_capture: false,
            ignore_apps: Vec::new(),

            max_items: 5000,
            max_image_bytes: 200 * 1024 * 1024,
            max_favorite_file_bytes: 200 * 1024 * 1024,
            max_text_bytes: 2 * 1024 * 1024,
            max_file_bytes: 50 * 1024 * 1024,
            max_files_per_clip: 100,
            max_image_pixels: 40_000_000,

            picker_size: [620.0, 560.0],
            manager_size: [1060.0, 700.0],
            hide_on_blur: true,

            keep_clipboard_after_paste: true,
            move_to_top_after_paste: true,
            paste_key: PasteKey::CtrlV,

            autostart: false,
            log_level: "info".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// 线程间消息
// ---------------------------------------------------------------------------

/// 后台 → UI 事件。
#[derive(Clone, Debug)]
pub enum Event {
    /// 请求打开 Picker；`target_hwnd` 是呼出前的目标前台窗口。
    PickerRequested { target_hwnd: isize },
    /// 请求打开管理台。
    ManagerRequested,
    /// 请求打开管理台并定位到「设置」页（托盘菜单「设置」）。
    SettingsRequested,
    /// 请求退出程序（托盘菜单）。
    QuitRequested,
    /// 历史列表发生变化（Store 处理完一次写入/删除后发送）。
    ClipsChanged,
    /// 搜索/分页结果。
    SearchPage(SearchPage),
    /// 某条记录的粘贴载荷。
    PastePayload(PastePayload),
    /// 统计信息刷新。
    Stats(Stats),
    /// 自动粘贴结果。
    PasteResult { ok: bool, reason: Option<String> },
    /// 轻提示（UI toast）。
    Toast(String),
    /// 备用全局快捷键注册状态。
    HotkeyStatus { ok: bool, reason: Option<String> },
    /// 剪贴板监听线程捕获到新内容（正常情况下 Sys 发 `Event::Captured`，由 UI 线程转发 `StoreCmd::Upsert`）。
    Captured(ClipInput),
}

/// UI → Store 命令。
#[derive(Clone, Debug)]
pub enum StoreCmd {
    Upsert(ClipInput),
    Search(SearchQuery),
    SetPinned {
        id: ClipId,
        pinned: bool,
    },
    Delete(Vec<ClipId>),
    Clear {
        delete_blobs: bool,
    },
    GetPastePayload(ClipId),
    /// 把某条文件的实体复制进数据目录（收藏时调用）。
    SaveFavoriteFiles(ClipId),
    RefreshStats,
    ExportJson {
        path: PathBuf,
    },
    Shutdown,
}

/// UI → Sys 命令。
#[derive(Clone, Debug)]
pub enum SysCmd {
    /// 写剪贴板并自动粘贴到 `target_hwnd`（UI 已先隐藏窗口）。
    Paste {
        payload: PastePayload,
        target_hwnd: isize,
    },
    /// 只写剪贴板，不粘贴。
    WriteClipboard(PastePayload),
    /// 应用新的快捷键配置。
    SetHotkeys { picker: HotkeyChord },
    /// 暂停/恢复剪贴板监听。
    SetPause(bool),
    /// 设置开机自启。
    SetAutostart(bool),
    /// 请求立即退出（UI 侧已在退出流程中）。
    Shutdown,
}

// ---------------------------------------------------------------------------
// 格式化辅助（全中文 UI 共用）
// ---------------------------------------------------------------------------

/// 截断到 `max` 个字符，超出加 `…`。
pub fn truncate_chars(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(max * 2);
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// 人类可读大小：1.2 KB / 3.4 MB / 1.1 GB。
pub fn fmt_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 相对时间：刚刚 / N 分钟前 / N 小时前 / 昨天 HH:MM / MM-DD HH:MM。
pub fn fmt_relative_time(ms: i64) -> String {
    let now = now_ms();
    let diff = (now - ms).max(0);
    let minute = 60_000;
    let hour = 60 * minute;
    let day = 24 * hour;
    if diff < minute {
        return "刚刚".into();
    }
    if diff < hour {
        return format!("{} 分钟前", diff / minute);
    }
    if diff < day {
        return format!("{} 小时前", diff / hour);
    }
    if diff < 2 * day {
        let dt = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_else(chrono::Utc::now);
        let local = dt.with_timezone(&chrono::Local);
        return format!("昨天 {}", local.format("%H:%M"));
    }
    let dt = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_else(chrono::Utc::now);
    let local = dt.with_timezone(&chrono::Local);
    if local.format("%Y").to_string() == chrono::Local::now().format("%Y").to_string() {
        format!("{}", local.format("%m-%d %H:%M"))
    } else {
        format!("{}", local.format("%Y-%m-%d"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_match_spec() {
        let s = Settings::default();
        assert_eq!(s.max_items, 5000);
        assert_eq!(s.max_image_bytes, 200 * 1024 * 1024);
        assert_eq!(s.max_favorite_file_bytes, 200 * 1024 * 1024);
        assert_eq!(s.max_text_bytes, 2 * 1024 * 1024);
        assert_eq!(s.max_file_bytes, 50 * 1024 * 1024);
        assert_eq!(s.picker_size, [620.0, 560.0]);
        assert_eq!(s.hotkey_picker.label(), "Ctrl+`");
        assert!(s.hotkey_picker.is_set());
        assert!(!s.autostart);
    }

    #[test]
    fn hotkey_labels() {
        assert_eq!(
            HotkeyChord::new(true, false, true, false, 0x56).label(),
            "Ctrl+Shift+V"
        );
        assert_eq!(
            HotkeyChord::new(false, false, false, true, 0x56).label(),
            "Win+V"
        );
        assert_eq!(
            HotkeyChord::new(true, false, false, false, 0x70).label(),
            "Ctrl+F1"
        );
        assert_eq!(
            HotkeyChord::new(false, false, false, true, 0x56).is_win_v(),
            true
        );
        assert!(!HotkeyChord::default().is_win_v());
    }

    #[test]
    fn settings_json_roundtrip_and_defaults() {
        let s = Settings::default();
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_items, s.max_items);
        // 缺失字段走默认值
        let partial: Settings = serde_json::from_str(r#"{"max_items": 42}"#).unwrap();
        assert_eq!(partial.max_items, 42);
        assert_eq!(partial.max_image_bytes, 200 * 1024 * 1024);
    }

    #[test]
    fn kind_roundtrip() {
        for k in [ClipKind::Text, ClipKind::Image, ClipKind::Files] {
            assert_eq!(ClipKind::from_i32(k.as_i32()), Some(k));
        }
        assert_eq!(ClipKind::from_i32(9), None);
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(fmt_size(0), "0 B");
        assert_eq!(fmt_size(1024), "1.0 KB");
        assert_eq!(fmt_size(1024 * 1024), "1.00 MB");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
        assert_eq!(truncate_chars("ab", 3), "ab");
        assert_eq!(fmt_relative_time(now_ms()), "刚刚");
    }

    #[test]
    fn clip_item_titles() {
        let mut item = ClipItem {
            kind: ClipKind::Text,
            text: Some("第一行\n第二行内容".into()),
            ..Default::default()
        };
        assert_eq!(item.title(), "第一行");
        assert!(item.subtitle().contains("第二行"));
        item.kind = ClipKind::Image;
        item.width = Some(1920);
        item.height = Some(1080);
        assert_eq!(item.title(), "图片 1920×1080");
    }
}

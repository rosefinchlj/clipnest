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
pub const SCHEMA_VERSION: i64 = 4;

/// 回收站保留时长：手动删除后 2 天自动彻底删除。
pub const TRASH_RETENTION_MS: i64 = 2 * 24 * 60 * 60 * 1000;

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

/// 收藏文件实体副本的保存状态（仅收藏的文件条目需要）。
///
/// 旧版 `files_json` 没有该字段，反序列化时回退为 `None`（未知），
/// 由 UI 再根据 `stored`/`exists` 兜底展示。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyState {
    /// 正在后台复制。
    Pending,
    /// 已保存到数据目录。
    Stored,
    /// 复制失败（权限/磁盘等），可重试。
    Failed,
    /// 超出单文件/总量上限，不再尝试。
    TooLarge,
    /// 源文件已不存在。
    Missing,
    /// 不需要副本（例如非收藏项或尚未请求）。
    NotNeeded,
}

impl CopyState {
    /// 中文状态文案（列表/详情徽标）。
    pub fn label(self) -> &'static str {
        match self {
            CopyState::Pending => "正在保存副本…",
            CopyState::Stored => "已保存副本",
            CopyState::Failed => "副本保存失败",
            CopyState::TooLarge => "超出上限，未保存副本",
            CopyState::Missing => "源文件不存在",
            CopyState::NotNeeded => "仅路径",
        }
    }

    /// 是否值得提供「重试保存副本」。
    pub fn retryable(self) -> bool {
        matches!(
            self,
            CopyState::Failed | CopyState::TooLarge | CopyState::Missing
        )
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
    /// 实体副本保存状态；旧版 JSON 无此字段时为 `None`。
    #[serde(default)]
    pub copy_state: Option<CopyState>,
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
            copy_state: None,
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
    /// 富文本 HTML fragment（已 sanitize；旧数据/旧 JSON 为 None）。
    pub html: Option<String>,
    /// RTF 原文（已做基本控制字过滤；旧数据/旧 JSON 为 None）。
    pub rtf: Option<String>,
    pub files: Vec<FileEntry>,
    pub blob_id: Option<String>,
    pub byte_len: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub created_at: i64,
    pub last_used_at: i64,
    pub use_count: u32,
    /// 是否收藏（独立于置顶）；收藏项不会被容量清理。
    pub favorited: bool,
    /// 是否在列表中置顶；只有收藏项可置顶。
    pub pinned: bool,
    pub source_app: Option<String>,
    pub source_title: Option<String>,
    /// 手动删除进入回收站的时间；`None` 表示仍在历史/收藏中。
    pub deleted_at: Option<i64>,
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
    /// 富文本 HTML fragment（来自 CF_HTML，已 sanitize）。
    pub html: Option<String>,
    /// RTF 原文（来自 "Rich Text Format"，已做基本过滤）。
    pub rtf: Option<String>,
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
    /// 富文本 HTML fragment（回写前会再次 sanitize）。
    pub html: Option<String>,
    /// RTF 原文（回写前会再次做基本过滤）。
    pub rtf: Option<String>,
    /// 图片文件路径（PNG）。
    pub png_path: Option<PathBuf>,
    pub files: Vec<FileEntry>,
}

// ---------------------------------------------------------------------------
// 查询 / 过滤
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ClipFilter {
    Favorites,
    #[default]
    All,
    Text,
    Image,
    Files,
}

impl ClipFilter {
    pub fn label(self) -> &'static str {
        match self {
            ClipFilter::Favorites => "收藏",
            ClipFilter::All => "全部",
            ClipFilter::Text => "文本",
            ClipFilter::Image => "图片",
            ClipFilter::Files => "文件",
        }
    }

    pub fn all() -> [ClipFilter; 5] {
        [
            ClipFilter::Favorites,
            ClipFilter::All,
            ClipFilter::Text,
            ClipFilter::Image,
            ClipFilter::Files,
        ]
    }

    pub fn next(self) -> ClipFilter {
        match self {
            ClipFilter::Favorites => ClipFilter::All,
            ClipFilter::All => ClipFilter::Text,
            ClipFilter::Text => ClipFilter::Image,
            ClipFilter::Image => ClipFilter::Files,
            ClipFilter::Files => ClipFilter::Favorites,
        }
    }

    pub fn prev(self) -> ClipFilter {
        match self {
            ClipFilter::Favorites => ClipFilter::Files,
            ClipFilter::All => ClipFilter::Favorites,
            ClipFilter::Text => ClipFilter::All,
            ClipFilter::Image => ClipFilter::Text,
            ClipFilter::Files => ClipFilter::Image,
        }
    }
}

/// 多关键词匹配模式。
///
/// 搜索文本按空白拆成多个关键词（双引号内的短语不拆）；`Any` 任一命中即可，
/// `All` 要求每个关键词都命中（AND）。默认 `Any` 保持旧行为。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MatchMode {
    #[default]
    Any,
    All,
}

impl MatchMode {
    pub fn label(self) -> &'static str {
        match self {
            MatchMode::Any => "任一关键词",
            MatchMode::All => "全部关键词",
        }
    }

    pub fn all() -> [MatchMode; 2] {
        [MatchMode::Any, MatchMode::All]
    }
}

/// 列表排序方式。`Trash` 页固定按删除时间倒序，与此无关。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortOrder {
    #[default]
    Recent,
    Oldest,
    Largest,
    Type,
    MostUsed,
}

impl SortOrder {
    pub fn label(self) -> &'static str {
        match self {
            SortOrder::Recent => "最近使用",
            SortOrder::Oldest => "最早使用",
            SortOrder::Largest => "最大文件",
            SortOrder::Type => "类型",
            SortOrder::MostUsed => "使用最多",
        }
    }

    pub fn all() -> [SortOrder; 5] {
        [
            SortOrder::Recent,
            SortOrder::Oldest,
            SortOrder::Largest,
            SortOrder::Type,
            SortOrder::MostUsed,
        ]
    }
}

/// 列表时间范围筛选。按 [`TimeBasis`] 指定的时间列（最近使用 / 复制时间）计算。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TimeRange {
    #[default]
    All,
    Today,
    Last7Days,
    Last30Days,
    Last90Days,
    Custom,
}

impl TimeRange {
    pub fn label(self) -> &'static str {
        match self {
            TimeRange::All => "全部时间",
            TimeRange::Today => "今天",
            TimeRange::Last7Days => "近7天",
            TimeRange::Last30Days => "近30天",
            TimeRange::Last90Days => "近90天",
            TimeRange::Custom => "指定日期",
        }
    }

    pub fn all() -> [TimeRange; 6] {
        [
            TimeRange::All,
            TimeRange::Today,
            TimeRange::Last7Days,
            TimeRange::Last30Days,
            TimeRange::Last90Days,
            TimeRange::Custom,
        ]
    }

    /// 起始时间戳（含）。`All` 返回 `None`。
    #[cfg(test)]
    pub fn cutoff_ms(self, now: i64) -> Option<i64> {
        let day = 24 * 60 * 60 * 1000;
        match self {
            TimeRange::All => None,
            TimeRange::Today => Some(local_midnight_ms(now).unwrap_or(now - day)),
            TimeRange::Last7Days => Some(now - 7 * day),
            TimeRange::Last30Days => Some(now - 30 * day),
            TimeRange::Last90Days => Some(now - 90 * day),
            TimeRange::Custom => None,
        }
    }

    /// 时间窗口 `[start, end)`；`Custom` 使用 `custom_day`（起始日）与 `custom_end_day`（结束日，均本地 00:00）。
    pub fn bounds(
        self,
        now: i64,
        custom_day: Option<i64>,
        custom_end_day: Option<i64>,
    ) -> Option<(i64, Option<i64>)> {
        let day = 24 * 60 * 60 * 1000;
        match self {
            TimeRange::All => None,
            TimeRange::Today => Some((local_midnight_ms(now).unwrap_or(now - day), None)),
            TimeRange::Last7Days => Some((now - 7 * day, None)),
            TimeRange::Last30Days => Some((now - 30 * day, None)),
            TimeRange::Last90Days => Some((now - 90 * day, None)),
            TimeRange::Custom => {
                let start = custom_day?;
                let end_day = custom_end_day.unwrap_or(start);
                let (start, end_day) = if end_day < start {
                    (end_day, start)
                } else {
                    (start, end_day)
                };
                let end = local_next_midnight(end_day).unwrap_or(end_day + day);
                Some((start, Some(end)))
            }
        }
    }
}

/// 时间筛选依据：按「最近使用」或「复制时间」过滤。
///
/// 默认按最近使用，与旧版本行为一致；设为「复制时间」时改用 `created_at`。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBasis {
    #[default]
    LastUsed,
    Captured,
}

impl TimeBasis {
    pub fn label(self) -> &'static str {
        match self {
            TimeBasis::LastUsed => "最近使用",
            TimeBasis::Captured => "复制时间",
        }
    }

    pub fn all() -> [TimeBasis; 2] {
        [TimeBasis::LastUsed, TimeBasis::Captured]
    }

    /// 对应 SQL 时间列（查询时拼接进时间范围条件）。
    pub fn column(self) -> &'static str {
        match self {
            TimeBasis::LastUsed => "clips.last_used_at",
            TimeBasis::Captured => "clips.created_at",
        }
    }
}

/// 本地时间戳所在日期次日 00:00。
fn local_next_midnight(day_start: i64) -> Option<i64> {
    use chrono::{Days, Local, TimeZone};
    let local = Local.timestamp_millis_opt(day_start).single()?;
    let next = local.date_naive().checked_add_days(Days::new(1))?;
    let midnight = next.and_hms_opt(0, 0, 0)?;
    Local
        .from_local_datetime(&midnight)
        .single()
        .map(|dt| dt.timestamp_millis())
}

/// 本地时区当天 00:00 的时间戳；时区异常时返回 `None`。
fn local_midnight_ms(now: i64) -> Option<i64> {
    use chrono::{Local, TimeZone};
    let local = Local.timestamp_millis_opt(now).single()?;
    let midnight = local.date_naive().and_hms_opt(0, 0, 0)?;
    Local
        .from_local_datetime(&midnight)
        .single()
        .map(|dt| dt.timestamp_millis())
}

#[derive(Clone, Debug)]
pub struct SearchQuery {
    pub text: String,
    pub filter: ClipFilter,
    pub time_range: TimeRange,
    /// `TimeRange::Custom` 起始日的本地 00:00 时间戳。
    pub custom_day: Option<i64>,
    /// `TimeRange::Custom` 结束日的本地 00:00 时间戳；与起始日相同表示单日。
    pub custom_end_day: Option<i64>,
    /// 时间范围按哪一列计算：最近使用或复制时间。
    pub time_basis: TimeBasis,
    /// 多关键词匹配模式（任一 / 全部）。
    pub match_mode: MatchMode,
    /// 来源应用筛选（文件/进程名，如 `chrome.exe`）；`None` 表示全部来源。
    pub source_app: Option<String>,
    /// 排序方式。
    pub sort: SortOrder,
    pub offset: usize,
    pub limit: usize,
    /// true 时查询回收站（deleted_at 非空）。
    pub trash: bool,
    /// 请求代号：UI 丢弃过期代号的返回结果。
    pub gen: u64,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            filter: ClipFilter::All,
            time_range: TimeRange::All,
            custom_day: None,
            custom_end_day: None,
            time_basis: TimeBasis::LastUsed,
            match_mode: MatchMode::Any,
            source_app: None,
            sort: SortOrder::Recent,
            offset: 0,
            limit: 200,
            trash: false,
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
    pub favorited: usize,
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

/// 候选窗选中态配色（与操作蓝分开，避免整屏都是蓝色）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPreset {
    /// 玉石青绿（默认）。
    #[default]
    Jade,
    /// 靛蓝。
    Indigo,
    /// 琥珀。
    Amber,
}

impl SelectionPreset {
    pub const ALL: [SelectionPreset; 3] = [
        SelectionPreset::Jade,
        SelectionPreset::Indigo,
        SelectionPreset::Amber,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SelectionPreset::Jade => "玉石青绿",
            SelectionPreset::Indigo => "靛蓝",
            SelectionPreset::Amber => "琥珀",
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

/// 明确退出 ClipNest 时的自动清理策略。
///
/// 只在「托盘退出」/「管理台退出」这类明确退出动作触发；
/// 关闭候选窗、隐藏到托盘不触发，避免误删用户历史。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitClearPolicy {
    /// 不清理（默认，最安全）。
    #[default]
    Off,
    /// 清空未收藏记录，保留收藏与收藏文件副本。
    Unfavorited,
    /// 清空全部记录（含收藏）。
    All,
}

impl ExitClearPolicy {
    pub const ALL: [ExitClearPolicy; 3] = [
        ExitClearPolicy::Off,
        ExitClearPolicy::Unfavorited,
        ExitClearPolicy::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExitClearPolicy::Off => "不清理",
            ExitClearPolicy::Unfavorited => "清空未收藏",
            ExitClearPolicy::All => "全部清空",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ExitClearPolicy::Off => "退出后保留全部历史（默认，最安全）",
            ExitClearPolicy::Unfavorited => "退出后清除未收藏记录，收藏与收藏文件副本保留（推荐）",
            ExitClearPolicy::All => "退出后清空全部记录（含收藏，不可恢复）",
        }
    }

    /// 退出清理时是否包含收藏项。
    pub fn include_favorites(self) -> bool {
        matches!(self, ExitClearPolicy::All)
    }

    /// 是否需要执行清理。
    pub fn clears(self) -> bool {
        !matches!(self, ExitClearPolicy::Off)
    }
}

/// 运行期临时暂停（不写入 settings.json，重启即失效）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TempPause {
    /// 无临时暂停。
    #[default]
    Off,
    /// 暂停到指定 unix 毫秒时间戳。
    Timed { until_ms: i64 },
    /// 暂停直到本次运行结束（重启后恢复记录）。
    UntilRestart,
}

impl TempPause {
    /// 指定时刻是否仍处于暂停。
    pub fn is_active_at(self, now_ms: i64) -> bool {
        match self {
            TempPause::Off => false,
            TempPause::Timed { until_ms } => now_ms < until_ms,
            TempPause::UntilRestart => true,
        }
    }

    pub fn is_active(self) -> bool {
        self.is_active_at(now_ms())
    }

    /// 中文状态文案（设置页/托盘 tooltip）。
    pub fn label(self) -> String {
        match self {
            TempPause::Off => "未暂停".to_string(),
            TempPause::Timed { until_ms } => {
                let remaining = (until_ms - now_ms()).max(0);
                let minutes = (remaining + 59_999) / 60_000;
                let dt = chrono::DateTime::from_timestamp_millis(until_ms)
                    .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
                    .unwrap_or_else(|| "稍后".to_string());
                format!("已暂停（{} 后自动恢复，约至 {dt}）", fmt_minutes(minutes))
            }
            TempPause::UntilRestart => "已暂停（直到重启 ClipNest）".to_string(),
        }
    }
}

/// 分钟数 → 人话（45 分钟 / 1 小时 20 分钟）。
fn fmt_minutes(minutes: i64) -> String {
    let minutes = minutes.max(0);
    if minutes < 60 {
        format!("{minutes} 分钟")
    } else {
        let h = minutes / 60;
        let m = minutes % 60;
        if m == 0 {
            format!("{h} 小时")
        } else {
            format!("{h} 小时 {m} 分钟")
        }
    }
}

/// 常见密码管理器进程：新装默认忽略，避免把密码写进剪贴板历史。
/// 只在 settings.json 完全没有 `ignore_apps` 字段时应用；
/// 用户已有的名单（包括空列表）不会被覆盖。
pub fn default_ignore_apps() -> Vec<String> {
    [
        "1password.exe",
        "bitwarden.exe",
        "keepass.exe",
        "keepassxc.exe",
        "lastpass.exe",
        "dashlane.exe",
        "enpass.exe",
        "keeper.exe",
        "roboform.exe",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 用户设置（持久化到 settings.json；未知字段忽略、缺失字段取默认值）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 外观皮肤：深色 / 浅色。
    pub theme: ThemeMode,
    /// 候选窗选中态配色。
    pub selection_preset: SelectionPreset,
    /// 面板磨砂透明度（35–100，越低越透，类似终端）。
    pub glass_opacity: u8,
    /// 备用全局快捷键（Win+V 被接管时仍保留）。
    pub hotkey_picker: HotkeyChord,
    pub pause_capture: bool,
    /// 打开候选窗时保留上次的搜索关键词（默认关闭）。
    pub keep_search_on_open: bool,
    /// 忽略的应用进程名（小写、含 .exe）。
    ///
    /// 只有 settings.json 完全没有该字段时才注入常见密码管理器；
    /// 用户手动清空名单（`[]`）后不会被覆盖。
    #[serde(default = "default_ignore_apps")]
    pub ignore_apps: Vec<String>,
    /// 明确退出时的自动清理策略（默认不清理）。
    pub exit_clear_policy: ExitClearPolicy,
    /// 首次引导是否已完成（欢迎提示 + 自启选项只出现一次）。
    pub first_run_done: bool,

    pub max_items: usize,
    pub max_image_bytes: u64,
    pub max_favorite_file_bytes: u64,
    pub max_text_bytes: u64,
    pub max_file_bytes: u64,
    pub max_files_per_clip: usize,
    pub max_image_pixels: u32,

    pub picker_size: [f32; 2],
    pub manager_size: [f32; 2],
    /// 界面缩放（0.8–1.5；1.0 为 100%）。启动与保存设置后立即生效。
    pub ui_scale: f32,
    /// 时间范围筛选依据：最近使用（默认）或复制时间。
    pub time_filter_basis: TimeBasis,
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
            selection_preset: SelectionPreset::Jade,
            glass_opacity: 75,
            hotkey_picker: HotkeyChord::default(),
            pause_capture: false,
            ignore_apps: default_ignore_apps(),
            exit_clear_policy: ExitClearPolicy::Off,
            first_run_done: false,
            keep_search_on_open: false,

            max_items: 5000,
            max_image_bytes: 200 * 1024 * 1024,
            max_favorite_file_bytes: 200 * 1024 * 1024,
            max_text_bytes: 2 * 1024 * 1024,
            max_file_bytes: 50 * 1024 * 1024,
            max_files_per_clip: 100,
            max_image_pixels: 40_000_000,

            picker_size: [620.0, 560.0],
            manager_size: [1060.0, 700.0],
            ui_scale: 1.0,
            time_filter_basis: TimeBasis::LastUsed,
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
// 完整备份/恢复（单文件 .cnbak）
// ---------------------------------------------------------------------------

/// 备份文件内的格式版本，与数据库 schema 版本互相独立。
pub const BACKUP_FORMAT_VERSION: u32 = 1;
/// 备份文件扩展名。
pub const BACKUP_EXTENSION: &str = "cnbak";
/// 备份文件内的 manifest 文件名。
pub const BACKUP_MANIFEST: &str = "manifest.json";
/// 备份文件内的数据库快照文件名。
pub const BACKUP_DB_ENTRY: &str = "data.db";

/// 检查备份后的摘要，用于恢复前预览。
#[derive(Clone, Debug, Default)]
pub struct BackupPreview {
    pub path: PathBuf,
    pub format_version: u32,
    pub app_version: String,
    pub created_at: i64,
    pub clips: usize,
    pub images: usize,
    pub favorite_files: usize,
    pub file_count: usize,
    pub total_bytes: u64,
    /// 每个文件的大小与 sha256 是否全部通过。
    pub checksum_ok: bool,
    pub checksum_errors: usize,
    /// 第一个校验失败的原因（中文），全部通过时为空。
    pub checksum_note: String,
}

/// 创建备份的结果。
#[derive(Clone, Debug, Default)]
pub struct BackupReport {
    pub clips: usize,
    pub images: usize,
    pub favorite_files: usize,
    pub bytes: u64,
}

/// 从备份合并恢复的结果。
#[derive(Clone, Debug, Default)]
pub struct RestoreReport {
    pub inserted: usize,
    pub skipped: usize,
    pub images: usize,
    pub favorite_files: usize,
    pub bytes: u64,
}

/// 备份/恢复任务完成后回传 UI 的事件。
#[derive(Clone, Debug)]
pub enum BackupEvent {
    /// 备份创建完成。
    Created(BackupReport),
    /// 备份检查完成（恢复预览）。
    Inspected(Box<BackupPreview>),
    /// 合并恢复完成。
    Restored(RestoreReport),
    /// 任务失败（中文原因）。
    Failed(String),
}

// ---------------------------------------------------------------------------
// 线程间消息
// ---------------------------------------------------------------------------

/// 界面缩放下限（80%）。
pub const UI_SCALE_MIN: f32 = 0.8;
/// 界面缩放上限（150%）。
pub const UI_SCALE_MAX: f32 = 1.5;
/// 界面缩放步长。
pub const UI_SCALE_STEP: f32 = 0.05;

/// 把界面缩放缓动到合法区间；NaN 回退 100%（无穷大按上下限夹取）。
pub fn clamp_ui_scale(scale: f32) -> f32 {
    if scale.is_nan() {
        return 1.0;
    }
    scale.clamp(UI_SCALE_MIN, UI_SCALE_MAX)
}

/// 候选窗尺寸预设：紧凑 / 标准 / 宽大。
pub const PICKER_SIZE_PRESETS: [(&str, [f32; 2]); 3] = [
    ("紧凑", [520.0, 480.0]),
    ("标准", [620.0, 560.0]),
    ("宽大", [760.0, 640.0]),
];

/// 后台 → UI 事件。
#[derive(Clone, Debug)]
pub enum Event {
    /// 请求打开 Picker；`target_hwnd` 是呼出前的目标前台窗口。
    PickerRequested { target_hwnd: isize },
    /// 请求打开管理台（独立窗口进程）。
    ManagerRequested { target_hwnd: isize },
    /// 请求打开管理台并定位到「设置」页（托盘菜单「设置」）。
    SettingsRequested { target_hwnd: isize },
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
    /// 历史记录中出现的来源应用名（去重、非空、按名称排序）。
    SourceApps(Vec<String>),
    /// 自动粘贴结果。
    PasteResult { ok: bool, reason: Option<String> },
    /// 轻提示（UI toast）。
    Toast(String),
    /// 子进程（管理台）发来的设置更新（避免仅依赖 settings.json 轮询）。
    SettingsUpdated(Box<Settings>),
    /// 备用全局快捷键注册状态。
    HotkeyStatus { ok: bool, reason: Option<String> },
    /// 运行期临时暂停状态变化（托盘菜单 / 设置页共用）。
    TempPauseChanged(TempPause),
    /// 收藏文件副本状态变化（A4）：Pending 先发、完成后再发最终状态。
    FileCopyState {
        clip_id: ClipId,
        state: CopyState,
    },
    /// 剪贴板监听线程捕获到新内容（正常情况下 Sys 发 `Event::Captured`，由 UI 线程转发 `StoreCmd::Upsert`）。
    Captured(ClipInput),
    /// 完整备份/恢复任务进度与结果（Store 线程串行执行）。
    Backup(BackupEvent),
}

/// UI → Store 命令。
#[derive(Clone, Debug)]
pub enum StoreCmd {
    Upsert(ClipInput),
    Search(SearchQuery),
    /// 收藏/取消收藏；不改变列表排序。
    SetFavorited {
        id: ClipId,
        favorited: bool,
    },
    /// 置顶/取消置顶；只对收藏项生效。
    SetPinned {
        id: ClipId,
        pinned: bool,
    },
    Delete(Vec<ClipId>),
    /// 从回收站恢复。
    Restore(Vec<ClipId>),
    /// 永久删除回收站中的指定条目。
    Purge(Vec<ClipId>),
    /// 永久清空回收站。
    EmptyTrash,
    /// 清空历史。
    ///
    /// `include_favorites=false`（默认）只删未收藏记录并保留收藏文件副本；
    /// `include_favorites=true` 连同收藏一起删除（调用方必须先做输入确认）。
    Clear {
        delete_blobs: bool,
        include_favorites: bool,
    },
    /// 退出前按策略清理历史；Store 处理完会回发 `Event::ClipsChanged`，
    /// UI 收到后即可安全退出（保证清理真正落盘）。
    ClearForExit(ExitClearPolicy),
    GetPastePayload(ClipId),
    /// 把某条文件的实体复制进数据目录（收藏时调用）。
    SaveFavoriteFiles(ClipId),
    RefreshStats,
    /// 查询历史记录中出现过的来源应用（供「来源」下拉使用）。
    ListSourceApps,
    ExportJson {
        path: PathBuf,
    },
    /// 创建单文件完整备份（SQLite 快照 + 图片/缩略图 + 收藏文件副本）。
    /// 结果通过 `Event::Backup(BackupEvent::Created/Failed)` 回传。
    CreateBackup {
        path: PathBuf,
    },
    /// 检查备份文件（解析 manifest、逐文件校验 sha256）。
    /// 结果通过 `Event::Backup(BackupEvent::Inspected/Failed)` 回传。
    InspectBackup {
        path: PathBuf,
    },
    /// 以「合并导入」方式从备份恢复：hash 去重、失败回滚。
    /// 结果通过 `Event::Backup(BackupEvent::Restored/Failed)` 回传。
    RestoreBackup {
        path: PathBuf,
    },
    /// 图片另存为：把该条记录的图片 blob 复制到用户选择的位置。
    SaveImageAs {
        id: ClipId,
        path: PathBuf,
    },
    /// 文本另存为：把该条记录的文本内容以 UTF-8 写入用户选择的位置。
    SaveTextAs {
        id: ClipId,
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
    /// 运行期临时暂停（不写入 settings.json）。
    SetTempPause(TempPause),
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
        assert_eq!(s.exit_clear_policy, ExitClearPolicy::Off);
        assert!(!s.first_run_done);
        assert!(!s.ignore_apps.is_empty());
        assert!(s.ignore_apps.iter().any(|a| a == "1password.exe"));
    }

    #[test]
    fn default_ignore_apps_are_lowercase_exe_names() {
        let apps = default_ignore_apps();
        assert!(apps.len() >= 6);
        for app in &apps {
            assert_eq!(*app, app.to_ascii_lowercase(), "{app} 必须是小写");
            assert!(app.ends_with(".exe"), "{app} 必须带 .exe");
        }
    }

    #[test]
    fn settings_json_adds_ignore_defaults_only_when_absent() {
        // 完全缺失该字段：注入常见密码管理器。
        let partial: Settings = serde_json::from_str(r#"{"max_items": 7}"#).unwrap();
        assert_eq!(partial.max_items, 7);
        assert!(!partial.ignore_apps.is_empty());
        // 用户显式清空成 []：必须尊重，不被默认名单覆盖。
        let emptied: Settings = serde_json::from_str(r#"{"ignore_apps": []}"#).unwrap();
        assert!(emptied.ignore_apps.is_empty());
        // 用户自定义名单：原样保留。
        let custom: Settings = serde_json::from_str(r#"{"ignore_apps": ["foo.exe"]}"#).unwrap();
        assert_eq!(custom.ignore_apps, vec!["foo.exe".to_string()]);
    }

    #[test]
    fn file_entry_legacy_json_without_copy_state() {
        // 旧版 files_json 没有 copy_state，必须能反序列化（serde default）。
        let legacy = r#"{"name":"a.txt","path":"C:\\a.txt","size":12,"exists":true,"stored":true}"#;
        let entry: FileEntry = serde_json::from_str(legacy).unwrap();
        assert_eq!(entry.copy_state, None);
        assert!(entry.stored);
        // 新字段可往返。
        let mut entry = entry;
        entry.copy_state = Some(CopyState::Failed);
        let json = serde_json::to_string(&entry).unwrap();
        let back: FileEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.copy_state, Some(CopyState::Failed));
        assert!(CopyState::Failed.retryable());
        assert!(!CopyState::Stored.retryable());
    }

    #[test]
    fn temp_pause_and_exit_policy() {
        assert!(!TempPause::Off.is_active());
        assert!(TempPause::UntilRestart.is_active());
        let until = now_ms() + 15 * 60_000;
        assert!(TempPause::Timed { until_ms: until }.is_active());
        assert!(!TempPause::Timed { until_ms: now_ms() - 1 }.is_active());
        assert!(TempPause::Timed { until_ms: until }.label().contains("暂停"));
        assert_eq!(ExitClearPolicy::Off.clears(), false);
        assert_eq!(ExitClearPolicy::Unfavorited.clears(), true);
        assert!(!ExitClearPolicy::Unfavorited.include_favorites());
        assert!(ExitClearPolicy::All.include_favorites());
        assert_eq!(ExitClearPolicy::ALL.len(), 3);
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
        // 旧版 settings.json 没有 selection_preset，应回退到默认玉石青绿。
        assert_eq!(partial.selection_preset, SelectionPreset::Jade);
        assert_eq!(
            serde_json::to_string(&SelectionPreset::Amber).unwrap(),
            "\"amber\""
        );
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

    #[test]
    fn time_basis_labels_and_columns() {
        assert_eq!(TimeBasis::default(), TimeBasis::LastUsed);
        assert_eq!(TimeBasis::LastUsed.label(), "最近使用");
        assert_eq!(TimeBasis::Captured.label(), "复制时间");
        assert_eq!(TimeBasis::LastUsed.column(), "clips.last_used_at");
        assert_eq!(TimeBasis::Captured.column(), "clips.created_at");
        assert_eq!(TimeBasis::all().len(), 2);
        assert_eq!(
            serde_json::to_string(&TimeBasis::Captured).unwrap(),
            "\"captured\""
        );
        assert_eq!(
            serde_json::from_str::<TimeBasis>("\"last_used\"").unwrap(),
            TimeBasis::LastUsed
        );
    }

    #[test]
    fn ui_scale_clamped_and_defaults() {
        assert_eq!(clamp_ui_scale(0.5), UI_SCALE_MIN);
        assert_eq!(clamp_ui_scale(2.0), UI_SCALE_MAX);
        assert_eq!(clamp_ui_scale(1.1), 1.1);
        assert_eq!(clamp_ui_scale(f32::NAN), 1.0);
        assert_eq!(clamp_ui_scale(f32::INFINITY), UI_SCALE_MAX);
        let s = Settings::default();
        assert_eq!(s.ui_scale, 1.0);
        assert_eq!(s.time_filter_basis, TimeBasis::LastUsed);
        // 旧版 settings.json 缺少新字段时回退默认值。
        let old: Settings = serde_json::from_str(r#"{"max_items": 42}"#).unwrap();
        assert_eq!(old.ui_scale, 1.0);
        assert_eq!(old.time_filter_basis, TimeBasis::LastUsed);
        assert_eq!(PICKER_SIZE_PRESETS[1].1, [620.0, 560.0]);
    }
}

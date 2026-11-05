//! UI 域：主题、字体、Picker 候选窗、Manager 管理台、复用控件、缩略图缓存。
//!
//! 对外 API 见 `docs/CONTRACT.md` §4（由 Lead 在 `app.rs` 中接线）：
//! - [`install_theme`] / [`install_fonts`]：App 启动时各调用一次；
//! - [`UiState`] / [`UiAction`] / [`WindowMode`] / [`ManagerPage`]：每帧 `show()` 返回动作；
//! - [`ThumbCache`]：后台缩略图缓存；
//! - [`preview`]：`--ui-preview picker|manager|settings|about` 独立假数据预览。
//!
//! 数据流（spec §9.3）：UI 不直接碰数据库/剪贴板，所有副作用都通过 [`UiAction`] 交给 Lead。

#![allow(dead_code)]

pub(crate) mod export_dialog;
pub mod fonts;
pub mod keys;
pub mod manager;
pub mod pages;
pub mod paint;
pub mod picker;
pub mod preview;
pub mod row;
pub mod theme;
pub mod thumbs;
pub mod widgets;

pub use fonts::install_fonts;
pub use theme::install_theme;
pub use thumbs::ThumbCache;

use crate::model::{
    ClipFilter, ClipId, ClipItem, MatchMode, SearchPage, Settings, SortOrder, Stats, TimeRange,
};
use eframe::egui;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// 搜索防抖 80ms（spec §6.1/§11）。
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(80);
/// Toast 显示 1.6s（spec §6.3/§7.3）。
const TOAST_DURATION: Duration = Duration::from_millis(1600);
/// 翻页/加载更多时距列表底部多少像素触发。
const LOAD_MORE_MARGIN: f32 = 240.0;

// ---------------------------------------------------------------------------
// 对外类型（CONTRACT §4）
// ---------------------------------------------------------------------------

/// 窗口模式：候选窗 / 管理台（同一个原生窗口运行时切换）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowMode {
    Picker,
    Manager,
}

/// 管理台页面。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ManagerPage {
    History,
    Favorites,
    Trash,
    Settings,
    About,
}

impl ManagerPage {
    /// 命令行/进程参数用的稳定 key。
    pub fn key(self) -> &'static str {
        match self {
            ManagerPage::History => "history",
            ManagerPage::Favorites => "favorites",
            ManagerPage::Trash => "trash",
            ManagerPage::Settings => "settings",
            ManagerPage::About => "about",
        }
    }

    /// 导航显示名。
    pub fn label(self) -> &'static str {
        match self {
            ManagerPage::History => "历史",
            ManagerPage::Favorites => "收藏",
            ManagerPage::Trash => "回收站",
            ManagerPage::Settings => "设置",
            ManagerPage::About => "关于",
        }
    }
}

/// UI → Lead 的动作（Lead 负责映射为 Store/Sys 命令与窗口命令）。
#[derive(Clone, Debug)]
pub enum UiAction {
    /// 查询/翻页；`gen` 用于丢弃过期结果。
    Search {
        text: String,
        filter: ClipFilter,
        time_range: TimeRange,
        custom_day: Option<i64>,
        custom_end_day: Option<i64>,
        match_mode: MatchMode,
        source_app: Option<String>,
        sort: SortOrder,
        offset: usize,
        trash: bool,
        gen: u64,
    },
    /// 请求历史记录中出现过的来源应用（供「来源」下拉）。
    ListSourceApps,
    /// 粘贴选中项（`plain=true` 只复制到剪贴板）。
    Paste { id: ClipId, plain: bool },
    /// 粘贴前先取载荷（Lead 发 `StoreCmd::GetPastePayload`）。
    RequestPastePayload(ClipId),
    /// 收藏/取消收藏（不改变排序）。
    Favorite { id: ClipId, favorited: bool },
    /// 置顶/取消置顶（仅收藏项）。
    Pin { id: ClipId, pinned: bool },
    /// 删除一批记录（移入回收站）。
    Delete { ids: Vec<ClipId> },
    /// 从回收站恢复。
    RestoreTrash { ids: Vec<ClipId> },
    /// 永久删除回收站中的记录。
    PurgeTrash { ids: Vec<ClipId> },
    /// 清空回收站。
    EmptyTrash,
    /// 清空历史。
    Clear { delete_blobs: bool },
    /// 把文件条目实体复制进数据目录（收藏时）。
    SaveFavoriteFiles(ClipId),
    /// 打开管理台。
    OpenManager,
    /// 回到候选窗。
    OpenPicker,
    /// 隐藏窗口（隐藏而非退出）。
    HideWindow,
    /// 退出程序。
    Quit,
    /// 保存设置（Lead 落盘并应用快捷键/自启等）。
    SetSettings(Box<Settings>),
    /// 设置开机自启。
    SetAutostart(bool),
    /// 暂停/恢复记录。
    SetPause(bool),
    /// 导出 JSON。
    ExportJson,
    /// 打开数据目录。
    OpenDataDir,
    /// 刷新统计。
    RefreshStats,
    /// 切换管理台页面。
    SelectManagerPage(ManagerPage),
    /// 实时预览磨砂透明度（只改运行时，不落盘；点保存才持久化）。
    PreviewGlassOpacity(u8),
}

// ---------------------------------------------------------------------------
// 内部状态
// ---------------------------------------------------------------------------

/// 待发送的搜索请求（防抖用）。
struct PendingSearch {
    gen: u64,
    due: Instant,
    offset: usize,
}

/// Toast 状态。
struct ToastState {
    text: String,
    until: Instant,
}

/// 确认框要执行的动作。
enum ConfirmKind {
    Delete(Vec<ClipId>),
    PurgeTrash(Vec<ClipId>),
    EmptyTrash,
    Clear,
}

/// 确认框（内容 + 待执行动作）。
struct Confirm {
    state: widgets::ConfirmState,
    kind: ConfirmKind,
}

/// UI 全局状态（每帧由 [`UiState::show`] 渲染）。
pub struct UiState {
    // ---- 模式与页面 ----
    mode: WindowMode,
    manager_page: ManagerPage,
    preview: bool,

    // ---- 设置副本与草稿 ----
    settings: Settings,
    settings_draft: Settings,

    // ---- 列表数据 ----
    items: Vec<ClipItem>,
    /// 当前已加载条目的 id 集合，翻页追加时 O(1) 判重。
    loaded_ids: HashSet<ClipId>,
    total: usize,
    loading: bool,
    active_gen: u64,
    next_gen: u64,
    append_gen: Option<u64>,
    append_requested_len: usize,
    selected: Option<usize>,
    /// 键盘导航后需要滚动到的行下标（下一次渲染列表时应用一次）。
    pending_scroll_row: Option<usize>,
    /// 候选窗列表当前滚动偏移（上一帧 `ScrollArea` 输出，用于最小化滚动）。
    picker_scroll_offset: f32,
    /// 候选窗本次出现的时间（用于 160ms 淡入；不改变窗口本身的位置）。
    picker_appear_at: Option<Instant>,
    stats: Stats,

    // ---- 搜索 ----
    search_text: String,
    search_focus: bool,
    search_focused: bool,
    /// 搜索框是否正在中文/日文输入法组合中（Preedit 未上屏）。
    ime_composing: bool,
    pending: Option<PendingSearch>,
    picker_filter: ClipFilter,
    manager_filter: ClipFilter,
    /// 多关键词匹配模式（任一 / 全部），Picker 与 Manager 共用。
    match_mode: MatchMode,
    /// 排序方式（Picker 与 Manager 共用；回收站固定按删除时间倒序）。
    sort: SortOrder,
    /// 来源应用筛选；`None` 表示全部来源。
    source_app: Option<String>,
    /// 历史记录中出现过的来源应用（「来源」下拉选项，由 Store 推送）。
    source_apps: Vec<String>,
    /// 下次开始帧是否需要请求来源列表。
    source_apps_due: bool,
    picker_time_range: TimeRange,
    manager_time_range: TimeRange,
    /// 时间范围筛选行是否展开（默认隐藏，由「筛选」按钮控制）。
    picker_time_open: bool,
    manager_time_open: bool,
    picker_custom_day: Option<i64>,
    manager_custom_day: Option<i64>,
    picker_custom_end: Option<i64>,
    manager_custom_end: Option<i64>,
    date_picker_open: bool,
    date_picker_month: Option<i64>,
    date_picker_start: Option<i64>,
    date_picker_end: Option<i64>,

    // ---- 反馈 ----
    toast: Option<ToastState>,
    paused: bool,
    capturing: bool,

    // ---- 状态展示 ----
    hotkey_status: Option<(bool, Option<String>)>,

    // ---- Manager ----
    detail_open: bool,
    multi_selected: HashSet<ClipId>,
    last_click_index: Option<usize>,
    confirm: Option<Confirm>,
    manager_search_focus: bool,

    // ---- 设置页 ----
    rec: widgets::HotkeyRecorderState,
    ignore_input: String,
    clear_delete_blobs: bool,
}

impl UiState {
    /// 创建 UI 状态（Lead 在 App 启动时调用一次）。
    pub fn new(_ctx: &egui::Context, settings: &Settings) -> Self {
        let mut s = Self {
            mode: WindowMode::Picker,
            manager_page: ManagerPage::History,
            preview: false,
            settings: settings.clone(),
            settings_draft: settings.clone(),
            items: Vec::new(),
            loaded_ids: HashSet::new(),
            total: 0,
            loading: false,
            active_gen: 0,
            next_gen: 0,
            append_gen: None,
            append_requested_len: 0,
            selected: None,
            pending_scroll_row: None,
            picker_scroll_offset: 0.0,
            picker_appear_at: None,
            stats: Stats::default(),
            search_text: String::new(),
            search_focus: true,
            search_focused: false,
            ime_composing: false,
            pending: None,
            picker_filter: ClipFilter::All,
            manager_filter: ClipFilter::All,
            match_mode: MatchMode::Any,
            sort: SortOrder::Recent,
            source_app: None,
            source_apps: Vec::new(),
            source_apps_due: false,
            picker_time_range: TimeRange::All,
            manager_time_range: TimeRange::All,
            picker_time_open: false,
            manager_time_open: false,
            picker_custom_day: None,
            manager_custom_day: None,
            picker_custom_end: None,
            manager_custom_end: None,
            date_picker_open: false,
            date_picker_month: None,
            date_picker_start: None,
            date_picker_end: None,
            toast: None,
            paused: settings.pause_capture,
            capturing: false,
            hotkey_status: None,
            detail_open: true,
            multi_selected: HashSet::new(),
            last_click_index: None,
            confirm: None,
            manager_search_focus: false,
            rec: widgets::HotkeyRecorderState::default(),
            ignore_input: String::new(),
            clear_delete_blobs: true,
        };
        s.schedule_search(true);
        s
    }

    /// 应用 Store 返回的搜索页（gen 过期自动丢弃；翻页结果追加）。
    pub fn apply_search_page(&mut self, page: SearchPage) {
        if page.gen == self.active_gen {
            self.items = page.items;
            self.loaded_ids = self.items.iter().map(|i| i.id).collect();
            self.total = page.total;
            self.loading = false;
            self.append_gen = None;
            self.append_requested_len = 0;
            // 修正选择位置 + 清理失效的多选
            if self.items.is_empty() {
                self.selected = None;
            } else {
                let idx = self.selected.unwrap_or(0).min(self.items.len() - 1);
                self.selected = Some(idx);
            }
            let ids: HashSet<ClipId> = self.items.iter().map(|i| i.id).collect();
            self.multi_selected.retain(|id| ids.contains(id));
            self.last_click_index = None;
        } else if Some(page.gen) == self.append_gen {
            // 大列表滚动加载：复用持久 id 集合，追加页保持 O(1) 判重。
            for item in page.items {
                if self.loaded_ids.insert(item.id) {
                    self.items.push(item);
                }
            }
            self.total = page.total;
            self.append_gen = None;
            self.loading = false;
        }
        // 其它 gen：过期结果，直接丢弃
    }

    /// 应用统计信息。
    pub fn apply_stats(&mut self, stats: Stats) {
        self.stats = stats;
    }

    /// 应用 Win+V 接管状态。
    /// 应用备用快捷键注册状态。
    pub fn apply_hotkey_status(&mut self, ok: bool, reason: Option<String>) {
        if !ok {
            self.toast(format!(
                "备用快捷键注册失败：{}",
                reason
                    .clone()
                    .unwrap_or_else(|| "可能被其他程序占用".to_string())
            ));
        }
        self.hotkey_status = Some((ok, reason));
    }

    /// 同步最新设置（Lead 保存/加载后调用）。
    pub fn set_settings(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        self.settings_draft = settings.clone();
        self.paused = settings.pause_capture;
    }

    /// 弹一条 toast。
    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some(ToastState {
            text: msg.into(),
            until: Instant::now() + TOAST_DURATION,
        });
    }

    /// 当前模式。
    pub fn mode(&self) -> WindowMode {
        self.mode
    }

    /// 进入管理台指定页（Lead 处理托盘「管理历史」/首次运行等）。
    pub fn enter_manager(&mut self, page: ManagerPage) {
        self.mode = WindowMode::Manager;
        self.manager_page = page;
        self.manager_search_focus = false;
        self.search_focus = false;
        // 「保留上次搜索」开启时，管理台也沿用上次的筛选条件与时间范围。
        if !self.settings.keep_search_on_open {
            self.manager_time_range = TimeRange::All;
            self.manager_time_open = false;
            self.manager_custom_day = None;
            self.manager_custom_end = None;
            self.date_picker_open = false;
        }
        // 来源列表随进入列表页刷新（新应用捕获到的记录才能出现在下拉里）。
        self.source_apps_due = true;
        if matches!(
            page,
            ManagerPage::History | ManagerPage::Favorites | ManagerPage::Trash
        ) {
            // 管理台列表默认选中第一行，工具栏的「置顶 / 取消置顶」等动作才有明确目标。
            if self.selected.is_none() {
                self.selected = Some(0);
            }
            self.schedule_search(true);
        }
    }

    /// 回到候选窗：清空搜索与过滤、聚焦搜索框、立即查询（Lead 在呼出面板时调用）。
    ///
    /// 「保留上次搜索关键词」开启时沿用上次的搜索词、筛选、来源与时间范围（设置项，默认关闭）。
    pub fn enter_picker(&mut self) {
        self.mode = WindowMode::Picker;
        if !self.settings.keep_search_on_open {
            self.search_text.clear();
            self.picker_filter = ClipFilter::All;
            self.source_app = None;
            self.picker_time_range = TimeRange::All;
            self.picker_time_open = false;
            self.picker_custom_day = None;
            self.picker_custom_end = None;
            self.date_picker_open = false;
        }
        self.source_apps_due = true;
        self.selected = Some(0);
        self.pending_scroll_row = Some(0);
        self.picker_scroll_offset = 0.0;
        self.picker_appear_at = Some(Instant::now());
        self.search_focus = true;
        self.pending = None;
        self.append_gen = None;
        self.schedule_search(true);
    }

    /// 立即重发当前查询（ClipsChanged / 粘贴成功 / 设置变更后调用）。
    pub fn refresh(&mut self) {
        self.append_gen = None;
        self.append_requested_len = 0;
        self.source_apps_due = true;
        self.schedule_search(true);
    }

    /// 当前是否在文本输入/录制快捷键（Lead 用于决定失焦是否隐藏）。
    pub fn is_capturing_input(&self) -> bool {
        self.capturing
    }

    /// 本帧产生的的动作是否包含隐藏/退出（预览模式用于示意）。
    pub fn preview_mode(&mut self, on: bool) {
        self.preview = on;
    }

    // -----------------------------------------------------------------------
    // 每帧入口
    // -----------------------------------------------------------------------

    /// 每帧渲染；返回本帧产生的动作（Lead 负责执行并清空）。
    pub fn show(&mut self, ctx: &egui::Context, thumbs: &mut ThumbCache) -> Vec<UiAction> {
        let mut actions = Vec::new();
        self.begin_frame(ctx, &mut actions);
        // 整个窗口（含 SidePanel/透明区域）统一铺半透明底：Acrylic 模糊才能均匀透出，
        // 否则 SidePanel 没填充的区域会直接露出桌面、显得像破洞。
        let bg_painter = ctx.layer_painter(egui::LayerId::background());
        let screen = ctx.screen_rect();
        bg_painter.rect_filled(
            screen,
            egui::CornerRadius::same(theme::RADIUS_WINDOW),
            theme::window_bg(),
        );
        if theme::glass_enabled() {
            // 顶部轻微高光 + 渐隐，给磨砂玻璃一点"厚度"。
            let dark = !theme::is_light();
            let sheen = if dark {
                egui::Color32::from_white_alpha(12)
            } else {
                egui::Color32::from_black_alpha(7)
            };
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(screen.left_top(), sheen);
            mesh.colored_vertex(screen.right_top(), sheen);
            mesh.colored_vertex(screen.right_bottom(), egui::Color32::TRANSPARENT);
            mesh.colored_vertex(screen.left_bottom(), egui::Color32::TRANSPARENT);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            bg_painter.add(egui::Shape::mesh(mesh));
            let edge = if dark {
                egui::Color32::from_white_alpha(26)
            } else {
                egui::Color32::from_black_alpha(12)
            };
            bg_painter.line_segment(
                [
                    egui::Pos2::new(screen.left() + 14.0, screen.top() + 0.5),
                    egui::Pos2::new(screen.right() - 14.0, screen.top() + 0.5),
                ],
                egui::Stroke::new(1.0_f32, edge),
            );
        }
        match self.mode {
            WindowMode::Picker => {
                egui::CentralPanel::default()
                    .frame(theme::picker_frame())
                    .show(ctx, |ui| picker::show(self, ui, thumbs, &mut actions));
            }
            WindowMode::Manager => {
                egui::CentralPanel::default()
                    .frame(theme::manager_frame())
                    .show(ctx, |ui| manager::show(self, ui, thumbs, &mut actions));
            }
        }
        self.show_confirm(ctx, &mut actions);
        self.end_frame(ctx);
        actions
    }

    /// 渲染到指定 `Ui`（预览模式用：外层窗口由 preview 控制）。
    pub fn render_into(&mut self, ui: &mut egui::Ui, thumbs: &mut ThumbCache) -> Vec<UiAction> {
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        self.begin_frame(&ctx, &mut actions);
        match self.mode {
            WindowMode::Picker => picker::show(self, ui, thumbs, &mut actions),
            WindowMode::Manager => manager::show(self, ui, thumbs, &mut actions),
        }
        self.show_confirm(&ctx, &mut actions);
        self.end_frame(&ctx);
        actions
    }

    /// 每帧开始：处理 toast 过期、搜索防抖到期、重绘调度。
    fn begin_frame(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        if matches!(&self.toast, Some(t) if Instant::now() >= t.until) {
            self.toast = None;
        }
        if let Some(pending) = &self.pending {
            let now = Instant::now();
            if now >= pending.due {
                let pending = self.pending.take().expect("pending 已判定存在");
                self.active_gen = pending.gen;
                self.loading = true;
                actions.push(UiAction::Search {
                    text: self.search_text.clone(),
                    filter: self.effective_filter(),
                    time_range: self.effective_time_range(),
                    custom_day: self.effective_custom_day(),
                    custom_end_day: self.effective_custom_end(),
                    match_mode: self.match_mode,
                    source_app: self.effective_source_app(),
                    sort: self.sort,
                    offset: pending.offset,
                    trash: self.search_trash(),
                    gen: pending.gen,
                });
            }
        }
        if self.source_apps_due {
            self.source_apps_due = false;
            actions.push(UiAction::ListSourceApps);
        }
        if let Some(pending) = &self.pending {
            ctx.request_repaint_after(pending.due.saturating_duration_since(Instant::now()));
        }
        if self.toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.rec.recording {
            ctx.request_repaint();
        }
        self.capturing = false;
    }

    /// 每帧结束：记录是否在输入中，必要时请求重绘。
    fn end_frame(&mut self, ctx: &egui::Context) {
        let focused = ctx.memory(|m| m.focused().is_some());
        self.capturing = focused || self.rec.recording;
        // 呼吸动画/淡出需要持续重绘
        if self.toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    // -----------------------------------------------------------------------
    // 查询调度
    // -----------------------------------------------------------------------

    /// 当前生效的过滤条件（收藏页强制只看收藏）。
    pub(crate) fn effective_filter(&self) -> ClipFilter {
        match self.mode {
            WindowMode::Picker => self.picker_filter,
            WindowMode::Manager => match self.manager_page {
                ManagerPage::Favorites => ClipFilter::Favorites,
                ManagerPage::Trash => ClipFilter::All,
                _ => self.manager_filter,
            },
        }
    }

    pub(crate) fn search_trash(&self) -> bool {
        self.mode == WindowMode::Manager && self.manager_page == ManagerPage::Trash
    }

    /// 当前生效的时间范围。
    pub(crate) fn effective_time_range(&self) -> TimeRange {
        match self.mode {
            WindowMode::Picker => self.picker_time_range,
            WindowMode::Manager => self.manager_time_range,
        }
    }

    pub(crate) fn effective_custom_day(&self) -> Option<i64> {
        match self.mode {
            WindowMode::Picker => self.picker_custom_day,
            WindowMode::Manager => self.manager_custom_day,
        }
    }

    pub(crate) fn effective_custom_end(&self) -> Option<i64> {
        match self.mode {
            WindowMode::Picker => self.picker_custom_end,
            WindowMode::Manager => self.manager_custom_end,
        }
    }

    /// 当前生效的来源筛选：回收站页不按来源过滤（页面上没有清除入口，避免隐藏筛选）。
    pub(crate) fn effective_source_app(&self) -> Option<String> {
        if self.search_trash() {
            None
        } else {
            self.source_app.clone()
        }
    }

    /// 设置排序方式并立即重新查询；返回是否发生变化。
    pub(crate) fn set_sort(&mut self, sort: SortOrder) -> bool {
        if self.sort == sort {
            return false;
        }
        self.sort = sort;
        self.schedule_search(true);
        true
    }

    /// 设置来源筛选并立即重新查询；返回是否发生变化。
    pub(crate) fn set_source_app(&mut self, app: Option<String>) -> bool {
        if self.source_app == app {
            return false;
        }
        self.source_app = app;
        self.schedule_search(true);
        true
    }

    /// 设置多关键词匹配模式并立即重新查询；返回是否发生变化。
    pub(crate) fn set_match_mode(&mut self, mode: MatchMode) -> bool {
        if self.match_mode == mode {
            return false;
        }
        self.match_mode = mode;
        self.schedule_search(true);
        true
    }

    /// 应用 Store 推送的来源列表；当前筛选值即使已不存在也保留，避免筛选被悄悄重置。
    pub fn apply_source_apps(&mut self, apps: Vec<String>) {
        let mut apps = apps;
        if let Some(current) = self.source_app.clone() {
            if !apps.iter().any(|app| app.eq_ignore_ascii_case(&current)) {
                apps.push(current);
                apps.sort_by_key(|app| app.to_lowercase());
            }
        }
        self.source_apps = apps;
    }

    pub(crate) fn set_effective_custom_day(&mut self, day: Option<i64>) {
        match self.mode {
            WindowMode::Picker => self.picker_custom_day = day,
            WindowMode::Manager => self.manager_custom_day = day,
        }
    }

    pub(crate) fn set_effective_custom_end(&mut self, day: Option<i64>) {
        match self.mode {
            WindowMode::Picker => self.picker_custom_end = day,
            WindowMode::Manager => self.manager_custom_end = day,
        }
    }

    /// 单次查询条数：候选窗 200 / 管理台 500（spec §11）。
    fn list_limit(&self) -> usize {
        match self.mode {
            WindowMode::Picker => 200,
            WindowMode::Manager => 500,
        }
    }

    /// 安排一次搜索（`immediate` 时立即发，否则防抖 80ms）。
    fn schedule_search(&mut self, immediate: bool) {
        self.next_gen += 1;
        let gen = self.next_gen;
        let due = if immediate {
            Instant::now()
        } else {
            Instant::now() + SEARCH_DEBOUNCE
        };
        self.append_gen = None;
        self.append_requested_len = 0;
        self.pending = Some(PendingSearch {
            gen,
            due,
            offset: 0,
        });
    }

    /// 请求加载下一页（滚动到列表底部附近时调用）。
    pub(crate) fn request_more(&mut self, actions: &mut Vec<UiAction>) {
        if self.loading || self.pending.is_some() {
            return;
        }
        if self.items.len() >= self.total || self.items.is_empty() {
            return;
        }
        if self.append_requested_len == self.items.len() {
            return;
        }
        self.next_gen += 1;
        let gen = self.next_gen;
        self.append_gen = Some(gen);
        self.append_requested_len = self.items.len();
        self.loading = true;
        actions.push(UiAction::Search {
            text: self.search_text.clone(),
            filter: self.effective_filter(),
            time_range: self.effective_time_range(),
            custom_day: self.effective_custom_day(),
            custom_end_day: self.effective_custom_end(),
            match_mode: self.match_mode,
            source_app: self.effective_source_app(),
            sort: self.sort,
            offset: self.items.len(),
            trash: self.search_trash(),
            gen,
        });
    }

    // -----------------------------------------------------------------------
    // 动作辅助
    // -----------------------------------------------------------------------

    /// 当前选中的条目。
    pub(crate) fn selected_item(&self) -> Option<&ClipItem> {
        self.selected.and_then(|i| self.items.get(i))
    }

    /// 请求粘贴第 `index` 条：先要载荷，再发粘贴动作（Lead 按 plain 标志执行）。
    pub(crate) fn paste_item(&mut self, index: usize, plain: bool, actions: &mut Vec<UiAction>) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let id = item.id;
        actions.push(UiAction::RequestPastePayload(id));
        actions.push(UiAction::Paste { id, plain });
    }

    /// 收藏/取消收藏第 `index` 条（先乐观更新，Store 返回后会刷新）。
    pub(crate) fn toggle_favorite(&mut self, index: usize, actions: &mut Vec<UiAction>) {
        let Some(item) = self.items.get_mut(index) else {
            return;
        };
        item.favorited = !item.favorited;
        let favorited = item.favorited;
        if !favorited {
            item.pinned = false;
        }
        let id = item.id;
        actions.push(UiAction::Favorite { id, favorited });
        self.toast(if favorited {
            "已收藏"
        } else {
            "已取消收藏"
        });
    }

    /// 置顶/取消置顶第 `index` 条（仅收藏项可置顶）。
    pub(crate) fn toggle_pinned(&mut self, index: usize, actions: &mut Vec<UiAction>) {
        let Some(item) = self.items.get_mut(index) else {
            return;
        };
        if !item.favorited {
            self.toast("先收藏后才能置顶");
            return;
        }
        item.pinned = !item.pinned;
        let pinned = item.pinned;
        let id = item.id;
        actions.push(UiAction::Pin { id, pinned });
        self.toast(if pinned {
            "已置顶"
        } else {
            "已取消置顶"
        });
    }

    /// 删除一批记录（乐观移除，Store 结果刷新时校准）。
    pub(crate) fn delete_ids(&mut self, ids: Vec<ClipId>, actions: &mut Vec<UiAction>) {
        if ids.is_empty() {
            return;
        }
        let set: HashSet<ClipId> = ids.iter().copied().collect();
        let before = self.items.len();
        self.items.retain(|it| !set.contains(&it.id));
        self.loaded_ids.retain(|id| !set.contains(id));
        self.total = self.total.saturating_sub(before - self.items.len());
        self.multi_selected.retain(|id| !set.contains(id));
        if self.items.is_empty() {
            self.selected = None;
        } else if let Some(sel) = self.selected {
            self.selected = Some(sel.min(self.items.len() - 1));
        }
        actions.push(UiAction::Delete { ids });
        self.toast("已移入回收站，保留 2 天");
    }

    /// 从回收站恢复：乐观移除当前回收站行，Store 结果刷新时校准。
    pub(crate) fn restore_ids(&mut self, ids: Vec<ClipId>, actions: &mut Vec<UiAction>) {
        if ids.is_empty() {
            return;
        }
        let set: HashSet<ClipId> = ids.iter().copied().collect();
        let before = self.items.len();
        self.items.retain(|it| !set.contains(&it.id));
        self.loaded_ids.retain(|id| !set.contains(id));
        self.total = self.total.saturating_sub(before - self.items.len());
        if self.items.is_empty() {
            self.selected = None;
        } else if let Some(sel) = self.selected {
            self.selected = Some(sel.min(self.items.len() - 1));
        }
        actions.push(UiAction::RestoreTrash { ids });
        self.toast("已从回收站恢复");
    }

    /// 彻底删除回收站条目（乐观移除；Store 负责释放文件实体）。
    fn purge_ids(&mut self, ids: Vec<ClipId>, actions: &mut Vec<UiAction>) {
        if ids.is_empty() {
            return;
        }
        let set: HashSet<ClipId> = ids.iter().copied().collect();
        let before = self.items.len();
        self.items.retain(|it| !set.contains(&it.id));
        self.loaded_ids.retain(|id| !set.contains(id));
        self.total = self.total.saturating_sub(before - self.items.len());
        if self.items.is_empty() {
            self.selected = None;
        } else if let Some(sel) = self.selected {
            self.selected = Some(sel.min(self.items.len() - 1));
        }
        actions.push(UiAction::PurgeTrash { ids });
        self.toast("正在彻底删除…");
    }

    /// 请求彻底删除回收站条目（不可恢复，二次确认）。
    pub(crate) fn request_purge_ids(&mut self, ids: Vec<ClipId>, actions: &mut Vec<UiAction>) {
        if ids.is_empty() {
            return;
        }
        self.confirm = Some(Confirm {
            state: widgets::ConfirmState {
                title: "彻底删除记录".to_string(),
                message: format!(
                    "将永久删除选中的 {} 条记录，并释放对应文件实体，操作不可恢复。确定继续吗？",
                    ids.len()
                ),
                confirm_label: "彻底删除".to_string(),
                danger: true,
            },
            kind: ConfirmKind::PurgeTrash(ids),
        });
        let _ = actions;
    }

    /// 请求清空回收站。
    pub(crate) fn request_empty_trash(&mut self) {
        if self.total == 0 {
            return;
        }
        self.confirm = Some(Confirm {
            state: widgets::ConfirmState {
                title: "清空回收站".to_string(),
                message: format!(
                    "将永久删除回收站中的全部 {} 条记录，并释放对应文件实体，操作不可恢复。",
                    self.total
                ),
                confirm_label: "清空回收站".to_string(),
                danger: true,
            },
            kind: ConfirmKind::EmptyTrash,
        });
    }

    /// 删除当前选中（收藏项二次确认，spec §6.1）。
    pub(crate) fn request_delete_selected(&mut self, actions: &mut Vec<UiAction>) {
        let ids: Vec<ClipId> = self.selected_item().map(|i| i.id).into_iter().collect();
        self.request_delete_ids(ids, actions);
    }

    /// 删除一批记录：其中含收藏项时先弹确认框。
    pub(crate) fn request_delete_ids(&mut self, ids: Vec<ClipId>, actions: &mut Vec<UiAction>) {
        if ids.is_empty() {
            return;
        }
        let any_favorited = self
            .items
            .iter()
            .any(|item| ids.contains(&item.id) && item.favorited);
        if any_favorited {
            self.confirm = Some(Confirm {
                state: widgets::ConfirmState {
                    title: "删除收藏项".to_string(),
                    message: format!(
                        "选中的 {} 条记录里有收藏项；删除后会移入回收站保留 2 天，可在回收站恢复。确定删除吗？",
                        ids.len()
                    ),
                    confirm_label: "删除".to_string(),
                    danger: true,
                },
                kind: ConfirmKind::Delete(ids),
            });
        } else {
            self.delete_ids(ids, actions);
        }
    }

    /// 打开管理台某页（Picker 齿轮 / Ctrl+, 等）。
    pub(crate) fn request_manager(&mut self, page: ManagerPage, actions: &mut Vec<UiAction>) {
        self.mode = WindowMode::Manager;
        self.manager_page = page;
        self.search_focus = false;
        if matches!(
            page,
            ManagerPage::History | ManagerPage::Favorites | ManagerPage::Trash
        ) {
            self.schedule_search(true);
        }
        actions.push(UiAction::OpenManager);
        actions.push(UiAction::SelectManagerPage(page));
    }

    /// 管理台内部切页（不请求 Lead 切窗口，只同步页面）。
    pub(crate) fn go_manager_page(&mut self, page: ManagerPage, actions: &mut Vec<UiAction>) {
        if self.manager_page == page {
            return;
        }
        self.manager_page = page;
        self.multi_selected.clear();
        self.manager_time_range = TimeRange::All;
        self.manager_custom_day = None;
        self.manager_custom_end = None;
        self.date_picker_open = false;
        self.source_apps_due = true;
        if matches!(
            page,
            ManagerPage::History | ManagerPage::Favorites | ManagerPage::Trash
        ) {
            if self.selected.is_none() {
                self.selected = Some(0);
            }
            self.schedule_search(true);
        }
        actions.push(UiAction::SelectManagerPage(page));
    }

    /// 显示确认框（Lead 在错误/清空等场景也可调用 `toast`）。
    fn show_confirm(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        let Some(confirm) = &self.confirm else {
            return;
        };
        match widgets::confirm_dialog(ctx, &confirm.state) {
            Some(true) => {
                let confirm = self.confirm.take().expect("确认框存在");
                match confirm.kind {
                    ConfirmKind::Delete(ids) => self.delete_ids(ids, actions),
                    ConfirmKind::PurgeTrash(ids) => self.purge_ids(ids, actions),
                    ConfirmKind::EmptyTrash => {
                        let count = self.total;
                        self.items.clear();
                        self.loaded_ids.clear();
                        self.total = 0;
                        self.selected = None;
                        self.multi_selected.clear();
                        actions.push(UiAction::EmptyTrash);
                        self.toast(format!("正在清空回收站（{count} 条）…"));
                    }
                    ConfirmKind::Clear => {
                        actions.push(UiAction::Clear {
                            delete_blobs: self.clear_delete_blobs,
                        });
                        self.toast("正在清空历史…");
                    }
                }
            }
            Some(false) => {
                self.confirm = None;
            }
            None => {}
        }
    }

    /// 请求清空历史（设置页调用；二次确认）。
    pub(crate) fn request_clear(&mut self) {
        self.confirm = Some(Confirm {
            state: widgets::ConfirmState {
                title: "清空剪贴板历史".to_string(),
                message: if self.clear_delete_blobs {
                    "将删除全部历史记录（含回收站），并同时删除图片/文件副本，操作不可恢复。确定继续吗？"
                        .to_string()
                } else {
                    "将删除全部历史记录（含回收站，保留图片/文件副本文件）。操作不可恢复，确定继续吗？"
                        .to_string()
                },
                confirm_label: "清空".to_string(),
                danger: true,
            },
            kind: ConfirmKind::Clear,
        });
    }

    /// 设置页保存草稿。
    pub(crate) fn save_settings_draft(&mut self, actions: &mut Vec<UiAction>) {
        let draft = self.settings_draft.clone();
        self.settings = draft.clone();
        self.paused = draft.pause_capture;
        actions.push(UiAction::SetSettings(Box::new(draft)));
        self.toast("设置已保存");
    }

    // -----------------------------------------------------------------------
    // 预览支持（assert 用）
    // -----------------------------------------------------------------------
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

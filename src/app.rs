//! 应用外壳：窗口生命周期、模式切换（Picker / Manager）、后台事件泵、动作调度。
//!
//! 线程模型（spec §9.1）：
//! - 本文件运行在 UI 线程：唯一创建窗口/调用 egui 的地方；
//! - `StoreHandle` 在独立线程独占 SQLite，`SysHandle` 在独立线程跑 Win32 消息循环；
//! - UI 与两个后台线程只通过 `mpsc` 通道（Event 收、StoreCmd/SysCmd 发）通信，
//!   `Settings` 用 `Arc<Mutex<Settings>>` 共享给后台读取。
//!
//! `--ui-preview <mode>` 模式走独立的假数据预览入口，不启动 Store/Sys。

use crate::model::{ClipId, Event, SearchQuery, Settings, StoreCmd, SysCmd};
use crate::store::StoreHandle;
use crate::sys::SysHandle;
use crate::ui::{ManagerPage, ThumbCache, UiAction, UiState, WindowMode};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 失焦后延迟隐藏，避免点击托盘菜单/系统弹窗时误隐藏。
const HIDE_ON_BLUR_DELAY: Duration = Duration::from_millis(220);
/// 隐藏窗口到真正执行 SendInput 之间的等待（让 winit 先完成隐藏）。
const PASTE_HIDE_DELAY: Duration = Duration::from_millis(70);
/// 启动后请求一次统计的时间。
const FIRST_STATS_DELAY: Duration = Duration::from_millis(300);

pub struct ClipNestApp {
    // ---- 配置 ----
    settings: Settings,
    settings_shared: Arc<Mutex<Settings>>,
    preview: Option<String>,

    // ---- 后台 ----
    store: Option<StoreHandle>,
    sys: Option<SysHandle>,
    store_tx: Option<Sender<StoreCmd>>,
    sys_tx: Option<Sender<SysCmd>>,
    events: Receiver<Event>,

    // ---- UI ----
    ui: UiState,
    thumbs: ThumbCache,

    // ---- 原生窗口 ----
    hwnd: Option<isize>,
    backdrop: Option<&'static str>,
    positioned: bool,
    window_visible: bool,

    // ---- 流程状态 ----
    init_error: Option<String>,
    manager_mode: bool,
    /// 呼出面板前的前台窗口（粘贴目标）。
    target_hwnd: isize,
    /// 等待 Store 返回载荷的粘贴请求：(clip_id, plain)
    paste_pending: Option<(ClipId, bool)>,
    /// 已隐藏窗口、等待延时后执行粘贴。
    paste_due: Option<(Instant, crate::model::PastePayload, bool)>,
    unfocused_since: Option<Instant>,
    first_stats_at: Option<Instant>,
    quitting: bool,
}

impl ClipNestApp {
    pub fn new(cc: &eframe::CreationContext<'_>, preview: Option<String>) -> Self {
        let settings = crate::config::load_settings();
        crate::logging::init(&crate::config::logs_dir(), &settings.log_level);
        crate::ui::install_fonts(&cc.egui_ctx);
        crate::ui::install_theme(&cc.egui_ctx);

        let settings_shared = Arc::new(Mutex::new(settings.clone()));
        let (events_tx, events) = mpsc::channel::<Event>();

        let mut init_error = None;
        let mut store = None;
        let mut store_tx = None;
        let mut sys = None;
        let mut sys_tx = None;

        if preview.is_none() {
            match StoreHandle::spawn(
                events_tx.clone(),
                settings_shared.clone(),
                cc.egui_ctx.clone(),
            ) {
                Ok(handle) => {
                    store_tx = Some(handle.cmd_tx());
                    store = Some(handle);
                }
                Err(err) => {
                    crate::logging::error(&format!("Store 启动失败：{err}"));
                    init_error = Some(format!(
                        "剪贴板历史数据库启动失败：{err}\n\n数据目录：{}\n请检查磁盘权限或打开日志目录查看详情。",
                        crate::config::data_dir().display()
                    ));
                }
            }

            // 允许在 Sys 域未就绪时单独联调 Store/UI（仅开发用）。
            if std::env::var_os("CLIPNEST_NO_SYS").is_none() {
                match SysHandle::spawn(
                    events_tx.clone(),
                    settings_shared.clone(),
                    cc.egui_ctx.clone(),
                ) {
                    Ok(handle) => {
                        sys_tx = Some(handle.cmd_tx());
                        sys = Some(handle);
                    }
                    Err(err) => {
                        crate::logging::error(&format!("Sys 启动失败：{err}"));
                        if init_error.is_none() {
                            init_error = Some(format!(
                                "系统层启动失败：{err}\n\n剪贴板监听/快捷键可能不可用，详见日志。"
                            ));
                        }
                    }
                }
            } else {
                crate::logging::warn("CLIPNEST_NO_SYS 已设置：跳过 Sys 域（仅开发联调用）");
            }
        }

        let mut ui = UiState::new(&cc.egui_ctx, &settings);
        ui.preview_mode(preview.is_some());
        let thumbs = ThumbCache::new(&cc.egui_ctx);

        Self {
            settings,
            settings_shared,
            preview,
            store,
            sys,
            store_tx,
            sys_tx,
            events,
            ui,
            thumbs,
            hwnd: None,
            backdrop: None,
            positioned: false,
            window_visible: true,
            init_error,
            manager_mode: false,
            target_hwnd: 0,
            paste_pending: None,
            paste_due: None,
            unfocused_since: None,
            first_stats_at: Some(Instant::now() + FIRST_STATS_DELAY),
            quitting: false,
        }
    }

    // -----------------------------------------------------------------------
    // 原生窗口
    // -----------------------------------------------------------------------

    /// 首帧：取 HWND、应用背景材质、把面板放到鼠标所在屏幕中央。
    fn init_native_window(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let Some(hwnd) = crate::window::hwnd_of_frame(frame) else {
            crate::logging::warn("无法取得原生窗口句柄，跳过背景材质与定位");
            self.positioned = true;
            return;
        };
        self.hwnd = Some(hwnd);
        match crate::window::apply_backdrop(hwnd) {
            Ok(kind) => {
                crate::logging::log(&format!("窗口背景材质生效: {kind}"));
                self.backdrop = Some(kind);
            }
            Err(err) => {
                crate::logging::warn(&format!("{err}；回退自绘半透明深色"));
                self.backdrop = None;
            }
        }
        if self.preview.is_none() {
            self.move_picker_near_cursor(ctx);
        }
        self.positioned = true;
    }

    fn move_picker_near_cursor(&self, ctx: &egui::Context) {
        if let Some(pos) =
            crate::window::picker_position(self.settings.picker_size.into(), ctx.pixels_per_point())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
    }

    /// 显示并聚焦候选窗。
    fn show_picker_window(&mut self, ctx: &egui::Context) {
        self.manager_mode = false;
        self.ui.enter_picker();
        self.ui.refresh();
        self.window_visible = true;
        self.unfocused_since = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::AlwaysOnTop,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            self.settings.picker_size.into(),
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            crate::model::APP_TITLE.to_string(),
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.move_picker_near_cursor(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    /// 隐藏窗口（保留进程与托盘）。
    fn hide_window(&mut self, ctx: &egui::Context) {
        self.window_visible = false;
        self.unfocused_since = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.request_repaint();
    }

    /// 打开管理台（同一窗口切模式）。
    fn open_manager_window(&mut self, ctx: &egui::Context, page: ManagerPage) {
        self.manager_mode = true;
        self.ui.enter_manager(page);
        self.window_visible = true;
        self.unfocused_since = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::Normal,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            self.settings.manager_size.into(),
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "{} · 管理",
            crate::model::APP_TITLE
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    /// 回到候选窗模式（管理台里点"返回"）。
    fn back_to_picker(&mut self, ctx: &egui::Context) {
        self.manager_mode = false;
        self.ui.enter_picker();
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::AlwaysOnTop,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            self.settings.picker_size.into(),
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            crate::model::APP_TITLE.to_string(),
        ));
        self.move_picker_near_cursor(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    // -----------------------------------------------------------------------
    // 事件泵（后台 → UI）
    // -----------------------------------------------------------------------

    fn pump_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            self.handle_event(ctx, event);
        }
    }

    fn handle_event(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::PickerRequested { target_hwnd } => {
                if target_hwnd != 0 && target_hwnd != self.hwnd.unwrap_or(0) {
                    self.target_hwnd = target_hwnd;
                }
                self.show_picker_window(ctx);
            }
            Event::ManagerRequested => self.open_manager_window(ctx, ManagerPage::History),
            Event::QuitRequested => self.request_quit(ctx),
            Event::ClipsChanged { .. } => {
                self.ui.refresh();
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::RefreshStats);
                }
            }
            Event::SearchPage(page) => self.ui.apply_search_page(page),
            Event::Stats(stats) => self.ui.apply_stats(stats),
            Event::PastePayload(payload) => {
                if let Some((_, plain)) = self.paste_pending.take() {
                    self.hide_window(ctx);
                    // 延迟到窗口真正隐藏后再让 Sys 恢复前台并 SendInput。
                    self.paste_due = Some((Instant::now() + PASTE_HIDE_DELAY, payload, plain));
                }
            }
            Event::PasteResult { ok, reason } => {
                if ok {
                    self.ui.toast("已粘贴");
                    if self.settings.move_to_top_after_paste {
                        self.ui.refresh();
                    }
                } else {
                    self.ui.toast(format!(
                        "已复制，请手动粘贴（{}）",
                        reason.unwrap_or_else(|| "目标窗口不可用".into())
                    ));
                }
            }
            Event::Toast(message) => self.ui.toast(message),
            Event::WinVStatus(status) => self.ui.apply_winv(status),
            Event::HotkeyStatus { ok, reason } => self.ui.apply_hotkey_status(ok, reason),
            Event::Captured(input) => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::Upsert(input));
                }
            }
        }
    }

    /// 处理延时粘贴队列。
    fn flush_paste_due(&mut self, _ctx: &egui::Context) {
        let Some((due, payload, plain)) = self.paste_due.take() else {
            return;
        };
        if Instant::now() < due {
            self.paste_due = Some((due, payload, plain));
            return;
        }
        let Some(sys_tx) = &self.sys_tx else {
            self.ui.toast("系统层未就绪，已复制到剪贴板");
            return;
        };
        if plain {
            let _ = sys_tx.send(SysCmd::WriteClipboard(payload));
        } else {
            let _ = sys_tx.send(SysCmd::Paste {
                payload,
                target_hwnd: self.target_hwnd,
            });
        }
    }

    // -----------------------------------------------------------------------
    // UI 动作（UI → 后台）
    // -----------------------------------------------------------------------

    fn handle_ui_action(&mut self, ctx: &egui::Context, action: UiAction) {
        match action {
            UiAction::Search {
                text,
                filter,
                offset,
                gen,
            } => {
                if let Some(tx) = &self.store_tx {
                    let limit = if self.ui.mode() == WindowMode::Picker {
                        200
                    } else {
                        500
                    };
                    let _ = tx.send(StoreCmd::Search(SearchQuery {
                        text,
                        filter,
                        offset,
                        limit,
                        gen,
                    }));
                }
            }
            UiAction::Paste { id, plain } => {
                self.paste_pending = Some((id, plain));
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::GetPastePayload(id));
                }
            }
            UiAction::RequestPastePayload(id) => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::GetPastePayload(id));
                }
            }
            UiAction::Pin { id, pinned } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::SetPinned { id, pinned });
                    if pinned {
                        let _ = tx.send(StoreCmd::SaveFavoriteFiles(id));
                    }
                }
            }
            UiAction::Delete { ids } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::Delete(ids));
                }
                self.ui.refresh();
            }
            UiAction::Clear { delete_blobs } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::Clear { delete_blobs });
                }
                self.ui.refresh();
            }
            UiAction::SaveFavoriteFiles(id) => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::SaveFavoriteFiles(id));
                }
            }
            UiAction::OpenManager => self.open_manager_window(ctx, ManagerPage::History),
            UiAction::OpenPicker => self.back_to_picker(ctx),
            UiAction::HideWindow => {
                if self.manager_mode {
                    self.back_to_picker(ctx);
                } else {
                    self.hide_window(ctx);
                }
            }
            UiAction::Quit => self.request_quit(ctx),
            UiAction::SetSettings(next) => self.apply_settings(ctx, *next),
            UiAction::SetAutostart(enabled) => {
                self.settings.autostart = enabled;
                if let Some(tx) = &self.sys_tx {
                    let _ = tx.send(SysCmd::SetAutostart(enabled));
                }
                self.persist_settings();
                self.ui.toast(if enabled {
                    "已开启开机自启"
                } else {
                    "已关闭开机自启"
                });
            }
            UiAction::SetPause(paused) => {
                self.settings.pause_capture = paused;
                if let Some(tx) = &self.sys_tx {
                    let _ = tx.send(SysCmd::SetPause(paused));
                }
                self.persist_settings();
                self.ui.toast(if paused {
                    "已暂停记录"
                } else {
                    "已恢复记录"
                });
            }
            UiAction::ExportJson => {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                let path = crate::config::data_dir().join(format!("export-{stamp}.json"));
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::ExportJson { path: path.clone() });
                    self.ui.toast(format!("正在导出到 {}", path.display()));
                }
            }
            UiAction::OpenDataDir => {
                let dir = crate::config::data_dir();
                let _ = std::process::Command::new("explorer.exe")
                    .arg(dir.as_os_str())
                    .spawn();
            }
            UiAction::RefreshStats => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::RefreshStats);
                }
            }
            UiAction::SelectManagerPage(page) => {
                self.ui.enter_manager(page);
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::RefreshStats);
                }
            }
        }
    }

    fn apply_settings(&mut self, ctx: &egui::Context, next: Settings) {
        self.settings = next.clone();
        if let Ok(mut shared) = self.settings_shared.lock() {
            *shared = next.clone();
        }
        if let Some(tx) = &self.sys_tx {
            let _ = tx.send(SysCmd::SetHotkeys {
                picker: next.hotkey_picker,
                winv_mode: next.winv_mode,
            });
            let _ = tx.send(SysCmd::SetPause(next.pause_capture));
        }
        self.ui.set_settings(&next);
        self.persist_settings();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            if self.manager_mode {
                next.manager_size.into()
            } else {
                next.picker_size.into()
            },
        ));
        self.ui.toast("设置已保存");
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreCmd::RefreshStats);
        }
    }

    fn persist_settings(&mut self) {
        match crate::config::save_settings(&self.settings) {
            Ok(()) => crate::logging::log("设置已保存"),
            Err(err) => {
                crate::logging::error(&format!("保存设置失败：{err}"));
                self.ui.toast(format!("保存设置失败：{err}"));
            }
        }
    }

    fn request_quit(&mut self, ctx: &egui::Context) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        crate::logging::log("退出 ClipNest");
        self.persist_settings();
        if let Some(tx) = &self.sys_tx {
            let _ = tx.send(SysCmd::Shutdown);
        }
        // Drop 会发送 Shutdown 并 join Store 线程。
        self.store = None;
        self.store_tx = None;
        self.sys = None;
        self.sys_tx = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    // -----------------------------------------------------------------------
    // 失焦自动隐藏
    // -----------------------------------------------------------------------

    fn handle_focus_autohide(&mut self, ctx: &egui::Context) {
        if self.manager_mode || !self.settings.hide_on_blur || !self.window_visible {
            self.unfocused_since = None;
            return;
        }
        let focused = ctx.input(|i| i.focused);
        if focused {
            self.unfocused_since = None;
            return;
        }
        if self.ui.is_capturing_input() {
            self.unfocused_since = None;
            return;
        }
        match self.unfocused_since {
            None => {
                self.unfocused_since = Some(Instant::now());
                ctx.request_repaint_after(HIDE_ON_BLUR_DELAY);
            }
            Some(since) if since.elapsed() >= HIDE_ON_BLUR_DELAY => {
                self.hide_window(ctx);
                self.unfocused_since = None;
            }
            Some(_) => ctx.request_repaint_after(HIDE_ON_BLUR_DELAY / 2),
        }
    }

    // -----------------------------------------------------------------------
    // 调试辅助（仅 debug）
    // -----------------------------------------------------------------------

    #[cfg(debug_assertions)]
    fn dev_hotkeys(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::F8)) {
            self.hide_window(ctx);
            crate::logging::log("dev: hide picker");
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F9)) {
            self.show_picker_window(ctx);
            crate::logging::log("dev: show picker");
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F10)) {
            if self.manager_mode {
                self.back_to_picker(ctx);
                crate::logging::log("dev: enter picker mode");
            } else {
                self.open_manager_window(ctx, ManagerPage::History);
                crate::logging::log("dev: enter manager mode");
            }
        }
    }

    /// 自动化窗口模式测试（仅 debug，环境变量 `CLIPNEST_DEV_SWITCH=1` 时启用）：
    /// 启动 1.5s 切 Manager、4s 切回 Picker，供脚本定时截图验证窗口命令链路。
    #[cfg(debug_assertions)]
    fn dev_auto_switch(&mut self, ctx: &egui::Context) {
        if std::env::var_os("CLIPNEST_DEV_SWITCH").is_none() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(100));
        let t = ctx.input(|i| i.time);
        if (1.5..4.0).contains(&t) && !self.manager_mode {
            self.open_manager_window(ctx, ManagerPage::History);
            crate::logging::log("dev-switch: enter manager mode");
        } else if t >= 4.0 && self.manager_mode {
            self.back_to_picker(ctx);
            crate::logging::log("dev-switch: enter picker mode");
        }
    }

    fn render_init_error(&self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgba_unmultiplied(13, 17, 23, 235))
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.heading(
                    egui::RichText::new("ClipNest 启动异常")
                        .size(18.0)
                        .color(egui::Color32::from_rgb(255, 107, 107)),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(self.init_error.as_deref().unwrap_or("未知错误"))
                        .size(13.0)
                        .color(egui::Color32::from_rgb(230, 237, 243)),
                );
                ui.add_space(16.0);
                if ui
                    .button(egui::RichText::new("打开数据目录").size(13.0))
                    .clicked()
                {
                    let _ = std::process::Command::new("explorer.exe")
                        .arg(crate::config::data_dir().as_os_str())
                        .spawn();
                }
            });
    }
}

impl eframe::App for ClipNestApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.positioned {
            self.init_native_window(ctx, frame);
        }

        #[cfg(debug_assertions)]
        self.dev_hotkeys(ctx);
        #[cfg(debug_assertions)]
        self.dev_auto_switch(ctx);

        if let Some(mode) = self.preview.clone() {
            crate::ui::preview::show(ctx, &mode);
            return;
        }

        self.pump_events(ctx);
        if self.quitting {
            return;
        }

        if let Some(at) = self.first_stats_at {
            if Instant::now() >= at {
                self.first_stats_at = None;
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::RefreshStats);
                }
            } else {
                ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
            }
        }

        if self.init_error.is_some() {
            // 启动异常必须让用户看到，即使默认隐藏启动。
            if !self.window_visible {
                self.window_visible = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            self.render_init_error(ctx);
            return;
        }

        self.handle_focus_autohide(ctx);
        self.flush_paste_due(ctx);

        let actions = self.ui.show(ctx, &mut self.thumbs);
        for action in actions {
            self.handle_ui_action(ctx, action);
        }
    }
}

impl Drop for ClipNestApp {
    fn drop(&mut self) {
        // 由 request_quit 显式关闭；异常退出兜底：Drop 顺序保证 Sys→Store 都发 Shutdown。
        if let Some(tx) = &self.sys_tx {
            let _ = tx.send(SysCmd::Shutdown);
        }
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreCmd::Shutdown);
        }
    }
}

//! UI 会话进程：按需启动的候选窗（`--ui-picker`）或管理台（`--ui-manager`）。
//!
//! 架构（低内存关键）：
//! - 常驻部分在 [`crate::daemon`]：托盘/热键/剪贴板监听/SQLite 采集，**不加载 GUI**；
//! - 本模块只在用户唤出界面时由守护进程 spawn 成子进程；关闭/粘贴后进程退出，
//!   eframe/OpenGL/字体等内存全部归还系统；
//! - 子进程自己打开 SQLite（WAL 多进程安全）做搜索/收藏/删除，粘贴时直接调用
//!   [`crate::clipboard::write_clipboard`] + [`crate::sys::paste::restore_and_paste`]；
//! - 设置保存写 settings.json，守护进程 1s 内轮询应用（快捷键/暂停）。

use crate::daemon::UiKind;
use crate::model::{ClipId, Event, SearchQuery, Settings, StoreCmd};
use crate::store::StoreHandle;
use crate::ui::{ManagerPage, ThumbCache, UiAction, UiState, WindowMode};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 显示后抢前台的第二次尝试延迟。
const FOCUS_RETRY: Duration = Duration::from_millis(60);
/// 失焦后关闭候选窗的延迟。
const HIDE_ON_BLUR_DELAY: Duration = Duration::from_millis(220);
/// 显示后多久内不因失焦关闭（避免抢焦点/窗口切换竞态）。
const FOCUS_GRACE: Duration = Duration::from_millis(500);
/// 仅复制（Ctrl+Enter）后停留展示 toast 的时间。
const PLAIN_COPY_LINGER: Duration = Duration::from_millis(420);
/// 粘贴前让窗口先隐藏的等待（避免 SendInput 打到自身）。
const PRE_PASTE_HIDE: Duration = Duration::from_millis(90);

/// UI 子进程启动参数。
pub struct UiSessionOptions {
    pub kind: UiKind,
    pub target_hwnd: isize,
    pub page: ManagerPage,
}

/// 启动一次 UI 会话（阻塞直到窗口关闭；返回后进程即可退出）。
pub fn run_ui_session(opts: UiSessionOptions) -> eframe::Result<()> {
    let settings = crate::config::load_settings();
    let is_picker = opts.kind == UiKind::Picker;
    let size = if is_picker {
        settings.picker_size
    } else {
        settings.manager_size
    };
    let title = if is_picker {
        crate::model::APP_TITLE.to_string()
    } else {
        format!("{} · 管理", crate::model::APP_TITLE)
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size(if is_picker {
            size
        } else {
            [880.0, 560.0]
        })
        .with_title(title)
        .with_transparent(true)
        .with_decorations(false)
        .with_taskbar(false)
        .with_resizable(!is_picker)
        // 先隐藏，首帧定位/抢前台后再显示，避免窗口在默认位置闪一下。
        .with_visible(false);
    viewport = if is_picker {
        viewport.with_always_on_top()
    } else {
        viewport.with_window_level(egui::WindowLevel::Normal)
    };

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "ClipNestUI",
        options,
        Box::new(move |cc| Ok(Box::new(ClipNestApp::new(cc, opts)))),
    )
}

/// `--ui-preview <mode>`：纯 UI 预览，不启动 Store、不读写真实数据。
pub fn run_preview(mode: String) -> eframe::Result<()> {
    struct PreviewApp {
        mode: String,
    }
    impl eframe::App for PreviewApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            crate::ui::preview::show(ctx, &self.mode);
        }
    }
    let is_picker = mode == "picker";
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size(if is_picker {
            [660.0, 620.0]
        } else {
            [1100.0, 720.0]
        })
        .with_title(format!("{} · UI 预览", crate::model::APP_NAME))
        .with_transparent(true)
        .with_visible(true)
        .with_decorations(!is_picker)
        .with_taskbar(!is_picker)
        .with_resizable(!is_picker);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "ClipNestPreview",
        options,
        Box::new(move |cc| {
            crate::ui::install_fonts(&cc.egui_ctx);
            crate::ui::install_theme(&cc.egui_ctx);
            Ok(Box::new(PreviewApp { mode }))
        }),
    )
}

pub struct ClipNestApp {
    kind: UiKind,
    settings: Settings,
    settings_shared: Arc<Mutex<Settings>>,
    store: Option<StoreHandle>,
    store_tx: Option<Sender<StoreCmd>>,
    events: Receiver<Event>,
    ui: UiState,
    thumbs: ThumbCache,

    hwnd: Option<isize>,
    positioned: bool,
    focus_due: Option<Instant>,
    shown_at: Option<Instant>,
    unfocused_since: Option<Instant>,

    target_hwnd: isize,
    paste_pending: Option<(ClipId, bool)>,
    closing_at: Option<Instant>,
    closed: bool,
    init_error: Option<String>,
}

impl ClipNestApp {
    fn new(cc: &eframe::CreationContext<'_>, opts: UiSessionOptions) -> Self {
        let settings = crate::config::load_settings();
        crate::logging::init(&crate::config::logs_dir(), &settings.log_level);
        crate::ui::install_fonts(&cc.egui_ctx);
        crate::ui::install_theme(&cc.egui_ctx);
        crate::logging::log(&format!(
            "UI 会话启动：kind={:?} target_hwnd={} page={:?}",
            opts.kind, opts.target_hwnd, opts.page
        ));

        let settings_shared = Arc::new(Mutex::new(settings.clone()));
        let (events_tx, events) = mpsc::channel::<Event>();
        let mut store = None;
        let mut store_tx = None;
        let mut init_error = None;
        match StoreHandle::spawn(
            events_tx,
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
                    "数据库打开失败：{err}\n\n数据目录：{}",
                    crate::config::data_dir().display()
                ));
            }
        }

        let mut ui = UiState::new(&cc.egui_ctx, &settings);
        match opts.kind {
            UiKind::Picker => {
                ui.enter_picker();
                ui.refresh();
            }
            UiKind::Manager => {
                ui.enter_manager(opts.page);
            }
        }
        if let Some(tx) = &store_tx {
            let _ = tx.send(StoreCmd::RefreshStats);
        }
        let thumbs = ThumbCache::new(&cc.egui_ctx);

        Self {
            kind: opts.kind,
            settings,
            settings_shared,
            store,
            store_tx,
            events,
            ui,
            thumbs,
            hwnd: None,
            positioned: false,
            focus_due: None,
            shown_at: None,
            unfocused_since: None,
            target_hwnd: opts.target_hwnd,
            paste_pending: None,
            closing_at: None,
            closed: false,
            init_error,
        }
    }

    fn is_picker(&self) -> bool {
        self.kind == UiKind::Picker
    }

    // -----------------------------------------------------------------------
    // 原生窗口
    // -----------------------------------------------------------------------

    fn init_native_window(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(hwnd) = crate::window::hwnd_of_frame(frame) {
            self.hwnd = Some(hwnd);
            if std::env::var_os("CLIPNEST_SKIP_MICA").is_none() {
                match crate::window::apply_backdrop(hwnd) {
                    Ok(kind) => crate::logging::log(&format!("窗口背景材质生效: {kind}")),
                    Err(err) => crate::logging::warn(&format!("{err}；回退半透明深色")),
                }
            }
            if let Some(pos) = crate::window::picker_position(
                if self.is_picker() {
                    self.settings.picker_size.into()
                } else {
                    self.settings.manager_size.into()
                },
                ctx.pixels_per_point(),
            ) {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            crate::window::force_foreground(hwnd);
        } else {
            crate::logging::warn("无法取得原生窗口句柄，跳过背景材质与定位");
        }
        self.positioned = true;
        self.shown_at = Some(Instant::now());
        self.focus_due = Some(Instant::now() + FOCUS_RETRY);
    }

    fn request_close(&mut self, ctx: &egui::Context) {
        if self.closed {
            return;
        }
        self.closed = true;
        // 窗口可能已被隐藏（粘贴前 Visible(false)）：直接给原生窗口发 WM_CLOSE，
        // egui 的 ViewportCommand::Close 在隐藏窗口上不保证被事件循环处理。
        if let Some(hwnd) = self.hwnd {
            crate::window::request_close_window(hwnd);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }

    fn close_after(&mut self, ctx: &egui::Context, delay: Duration) {
        if self.closing_at.is_none() {
            self.closing_at = Some(Instant::now() + delay);
            ctx.request_repaint_after(delay);
        }
    }

    fn spawn_manager(&self) -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let page = match self.ui.mode() {
            WindowMode::Manager => "history",
            WindowMode::Picker => "history",
        };
        crate::window::allow_set_foreground_any();
        std::process::Command::new(exe)
            .arg("--ui-manager")
            .arg("--page")
            .arg(page)
            .spawn()
            .is_ok()
    }

    // -----------------------------------------------------------------------
    // 事件
    // -----------------------------------------------------------------------

    fn pump_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            self.handle_event(ctx, event);
        }
    }

    fn handle_event(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::SearchPage(page) => self.ui.apply_search_page(page),
            Event::Stats(stats) => self.ui.apply_stats(stats),
            Event::ClipsChanged { .. } => {
                self.ui.refresh();
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::RefreshStats);
                }
            }
            Event::Toast(message) => self.ui.toast(message),
            Event::WinVStatus(status) => self.ui.apply_winv(status),
            Event::HotkeyStatus { ok, reason } => self.ui.apply_hotkey_status(ok, reason),
            Event::PastePayload(payload) => {
                let Some((pending_id, plain)) = self.paste_pending else {
                    return;
                };
                if payload.clip_id != pending_id {
                    crate::logging::warn(&format!(
                        "忽略过期粘贴载荷：payload={} pending={}",
                        payload.clip_id, pending_id
                    ));
                    return;
                }
                self.paste_pending = None;
                self.perform_paste(ctx, payload, plain);
            }
            _ => {}
        }
    }

    /// 写剪贴板 + 恢复目标窗口 + 模拟粘贴（在窗口先隐藏之后执行）。
    fn perform_paste(
        &mut self,
        ctx: &egui::Context,
        payload: crate::model::PastePayload,
        plain: bool,
    ) {
        let data_dir = crate::config::data_dir();
        if let Err(err) = crate::clipboard::write_clipboard(&payload, &data_dir) {
            crate::logging::error(&format!("写剪贴板失败：{err}"));
            self.ui.toast(format!("写剪贴板失败：{err}"));
            return;
        }
        if plain {
            self.ui.toast("已复制到剪贴板");
            self.close_after(ctx, PLAIN_COPY_LINGER);
            return;
        }
        if self.target_hwnd == 0 || crate::sys::util::window_pid(self.target_hwnd) == 0 {
            self.ui.toast("已复制，请手动粘贴（未找到原窗口）");
            return;
        }
        // 把窗口移到屏幕外（保持 visible：隐藏窗口上执行 eframe/winit 退出流程会异常），
        // 避免 SendInput 的 Ctrl+V 打到面板自身，然后恢复到目标窗口并粘贴。
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            -30000.0, -30000.0,
        )));
        std::thread::sleep(PRE_PASTE_HIDE);
        match crate::sys::paste::restore_and_paste(self.target_hwnd, self.settings.paste_key) {
            Ok(()) => {
                crate::logging::log("自动粘贴完成，关闭 UI 会话");
                self.request_close(ctx);
            }
            Err(err) => {
                crate::logging::warn(&format!("自动粘贴失败：{err}"));
                if let Some(pos) = crate::window::picker_position(
                    if self.is_picker() {
                        self.settings.picker_size.into()
                    } else {
                        self.settings.manager_size.into()
                    },
                    ctx.pixels_per_point(),
                ) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                if let Some(hwnd) = self.hwnd {
                    crate::window::force_foreground(hwnd);
                }
                self.ui
                    .toast(format!("已复制，请手动粘贴（{}）", err.reason()));
            }
        }
    }

    // -----------------------------------------------------------------------
    // UI 动作
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
            UiAction::RequestPastePayload(_) => {
                // Paste 动作已携带查询，避免重复 bump use_count。
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
            UiAction::OpenManager => {
                self.spawn_manager();
                self.request_close(ctx);
            }
            UiAction::OpenPicker => {
                // 当前会话已是候选窗；管理台里返回 = 关闭自己。
                self.request_close(ctx);
            }
            UiAction::HideWindow => self.request_close(ctx),
            UiAction::Quit => {
                crate::sys::thread::request_daemon_quit();
                self.request_close(ctx);
            }
            UiAction::SetSettings(next) => self.apply_settings(ctx, *next),
            UiAction::SetAutostart(enabled) => {
                self.settings.autostart = enabled;
                match crate::sys::autostart::set(enabled) {
                    Ok(()) => self.ui.toast(if enabled {
                        "已开启开机自启"
                    } else {
                        "已关闭开机自启"
                    }),
                    Err(err) => self.ui.toast(format!("设置开机自启失败：{err}")),
                }
                self.persist_settings();
            }
            UiAction::SetPause(paused) => {
                self.settings.pause_capture = paused;
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
                let _ = std::process::Command::new("explorer.exe")
                    .arg(crate::config::data_dir().as_os_str())
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
        self.ui.set_settings(&next);
        self.persist_settings();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            if self.ui.mode() == WindowMode::Picker {
                next.picker_size.into()
            } else {
                next.manager_size.into()
            },
        ));
        self.ui.toast("设置已保存（快捷键/暂停将在 1 秒内生效）");
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

    // -----------------------------------------------------------------------
    // 焦点 / 失焦关闭
    // -----------------------------------------------------------------------

    fn handle_focus(&mut self, ctx: &egui::Context) {
        if let Some(due) = self.focus_due {
            if Instant::now() >= due {
                self.focus_due = None;
                if let Some(hwnd) = self.hwnd {
                    crate::window::force_foreground(hwnd);
                }
            } else {
                ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
            }
        }
        if !self.is_picker() || !self.settings.hide_on_blur || self.closed {
            return;
        }
        if let Some(shown) = self.shown_at {
            if shown.elapsed() < FOCUS_GRACE {
                ctx.request_repaint_after(Duration::from_millis(120));
                return;
            }
        }
        if ctx.input(|i| i.focused) || self.ui.is_capturing_input() {
            self.unfocused_since = None;
            return;
        }
        match self.unfocused_since {
            None => {
                self.unfocused_since = Some(Instant::now());
                ctx.request_repaint_after(HIDE_ON_BLUR_DELAY);
            }
            Some(since) if since.elapsed() >= HIDE_ON_BLUR_DELAY => {
                crate::logging::log("候选窗失焦，关闭 UI 会话");
                self.request_close(ctx);
            }
            Some(_) => ctx.request_repaint_after(HIDE_ON_BLUR_DELAY / 2),
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
        if let Some(due) = self.closing_at {
            if Instant::now() >= due {
                self.request_close(ctx);
                return;
            }
            ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
        }
        self.handle_focus(ctx);
        self.pump_events(ctx);
        if self.closed {
            return;
        }
        if let Some(err) = self.init_error.clone() {
            if !self.positioned {
                return;
            }
            let _ = err;
            self.render_init_error(ctx);
            return;
        }
        let actions = self.ui.show(ctx, &mut self.thumbs);
        for action in actions {
            self.handle_ui_action(ctx, action);
        }
    }
}

impl Drop for ClipNestApp {
    fn drop(&mut self) {
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreCmd::Shutdown);
        }
        self.store = None;
        self.store_tx = None;
    }
}

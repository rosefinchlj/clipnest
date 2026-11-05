//! 管理台独立窗口进程（`--ui-manager`）。
//!
//! 主进程常驻候选窗；打开管理台时由主进程 spawn 本进程。这样用快捷键呼出粘贴面板时，
//! 管理台窗口不会被替换/隐藏（两个独立窗口可同时存在）。
//!
//! 本进程只启动 Store（读写同一个 SQLite），不启动 Sys：
//! - 托盘/快捷键/剪贴板监听仍由主进程负责；
//! - 粘贴直接写剪贴板并 `restore_and_paste` 到主进程传入的 target；
//! - 设置保存后通过 WM_COPYDATA 通知主进程立即应用（同时落盘 settings.json）。

use crate::model::{Event, SearchQuery, Settings, StoreCmd, ThemeMode};
use crate::store::StoreHandle;
use crate::ui::{ManagerPage, ThumbCache, UiAction, UiState};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

pub fn run(page: ManagerPage, target_hwnd: isize) -> eframe::Result<()> {
    let settings = crate::config::load_settings();
    let _ = crate::config::ensure_dirs();
    crate::logging::init(&crate::config::logs_dir(), &settings.log_level);
    crate::ui::theme::set_light(settings.theme == ThemeMode::Light);
    crate::ui::theme::set_selection_preset(settings.selection_preset);
    crate::ui::theme::set_glass_enabled(false);
    crate::logging::log(&format!(
        "管理台窗口进程启动（page={:?}，target={target_hwnd}）",
        page
    ));

    let size = settings.manager_size;
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size([880.0, 560.0])
        .with_title(format!(
            "{} · 管理 · {}",
            crate::model::APP_TITLE,
            page.key()
        ))
        .with_transparent(true)
        .with_decorations(false)
        .with_resizable(true)
        // 先创建在屏幕外，等首帧拿到真实 DPI 后再用 picker_position 定位。
        // 如果直接以默认位置显示，透明窗口的投影会先在屏幕左侧闪一下。
        .with_position([-30000.0, -30000.0])
        .with_active(false)
        .with_visible(true)
        .with_taskbar(true);
    if let Some(icon) = crate::window::app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "ClipNestManager",
        options,
        Box::new(move |cc| Ok(Box::new(ManagerApp::new(cc, page, target_hwnd)))),
    )
}

struct ManagerApp {
    settings: Settings,
    settings_shared: Arc<Mutex<Settings>>,
    _store: Option<StoreHandle>,
    store_tx: Option<Sender<StoreCmd>>,
    events: Receiver<Event>,
    ui: UiState,
    thumbs: ThumbCache,
    hwnd: Option<isize>,
    positioned: bool,
    target_hwnd: isize,
    paste_pending: Option<(i64, bool)>,
    quitting: bool,
    init_error: Option<String>,
}

impl ManagerApp {
    fn new(cc: &eframe::CreationContext<'_>, page: ManagerPage, target_hwnd: isize) -> Self {
        let settings = crate::config::load_settings();
        crate::ui::theme::set_light(settings.theme == ThemeMode::Light);
        crate::ui::theme::set_selection_preset(settings.selection_preset);
        crate::ui::theme::set_glass_enabled(false);
        if std::env::var_os("CLIPNEST_SKIP_FONTS").is_none() {
            crate::ui::install_fonts(&cc.egui_ctx);
        }
        crate::ui::install_theme(&cc.egui_ctx);

        let settings_shared = Arc::new(Mutex::new(settings.clone()));
        let (events_tx, events) = mpsc::channel::<Event>();
        let mut store = None;
        let mut store_tx = None;
        let mut init_error = None;
        match StoreHandle::spawn(events_tx, settings_shared.clone(), cc.egui_ctx.clone()) {
            Ok(handle) => {
                store_tx = Some(handle.cmd_tx());
                store = Some(handle);
            }
            Err(err) => {
                crate::logging::error(&format!("管理台 Store 启动失败：{err}"));
                init_error = Some(format!("数据库启动失败：{err}"));
            }
        }

        let mut ui = UiState::new(&cc.egui_ctx, &settings);
        ui.preview_mode(false);
        ui.enter_manager(page);
        if let Some(tx) = &store_tx {
            // 管理台独立进程没有主窗口的启动统计事件，主动拉一次，
            // 否则侧栏「共 N 条 / 收藏 N 条」会一直是 0。
            let _ = tx.send(StoreCmd::RefreshStats);
        }
        let thumbs = ThumbCache::new(&cc.egui_ctx);

        Self {
            settings,
            settings_shared,
            _store: store,
            store_tx,
            events,
            ui,
            thumbs,
            hwnd: None,
            positioned: false,
            target_hwnd,
            paste_pending: None,
            quitting: false,
            init_error,
        }
    }

    fn init_window(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let Some(hwnd) = crate::window::hwnd_of_frame(frame) else {
            self.positioned = true;
            return;
        };
        self.hwnd = Some(hwnd);
        crate::logging::log("管理台窗口：使用不透明背景（不做 Acrylic 磨砂）");
        crate::window::set_round_corners(hwnd);
        if let Some(pos) = crate::window::picker_position(
            self.settings.manager_size.into(),
            ctx.pixels_per_point(),
        ) {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        crate::window::force_foreground(hwnd);
        self.positioned = true;
    }

    fn request_close(&mut self, ctx: &egui::Context) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        if let Some(hwnd) = self.hwnd {
            crate::window::request_close_window(hwnd);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn apply_settings(&mut self, ctx: &egui::Context, next: Settings) {
        self.settings = next.clone();
        if let Ok(mut shared) = self.settings_shared.lock() {
            *shared = next.clone();
        }
        crate::ui::theme::set_glass_enabled(false);
        crate::ui::theme::set_light(next.theme == ThemeMode::Light);
        crate::ui::theme::set_selection_preset(next.selection_preset);
        crate::ui::install_theme(ctx);
        self.ui.set_settings(&next);
        self.persist_settings();
        self.notify_main_settings();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(next.manager_size.into()));
        self.ui.toast("设置已保存，主程序立即同步");
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreCmd::RefreshStats);
        }
    }

    fn persist_settings(&mut self) {
        if let Err(err) = crate::config::save_settings(&self.settings) {
            crate::logging::error(&format!("管理台保存设置失败：{err}"));
            self.ui.toast(format!("保存设置失败：{err}"));
        }
    }

    fn notify_main_settings(&self) {
        if !crate::sys::thread::send_settings_update(&self.settings) {
            crate::logging::warn("通知主进程设置更新失败（主进程可能已退出）");
        }
    }

    fn perform_paste(&mut self, payload: crate::model::PastePayload, plain: bool) {
        if let Err(err) = crate::clipboard::write_clipboard(&payload, &crate::config::data_dir()) {
            self.ui.toast(format!("写剪贴板失败：{err}"));
            return;
        }
        if plain {
            self.ui.toast("已复制到剪贴板");
            return;
        }
        if self.target_hwnd == 0 || crate::sys::util::window_pid(self.target_hwnd) == 0 {
            self.ui.toast("已复制，请手动粘贴（未找到原窗口）");
            return;
        }
        match crate::sys::paste::restore_and_paste(self.target_hwnd, self.settings.paste_key) {
            Ok(()) => {
                crate::logging::log("管理台自动粘贴完成");
                self.ui.toast("已粘贴");
                if self.settings.move_to_top_after_paste {
                    self.ui.refresh();
                }
            }
            Err(err) => {
                crate::logging::warn(&format!("管理台自动粘贴失败：{err}"));
                self.ui
                    .toast(format!("已复制，请手动粘贴（{}）", err.reason()));
            }
        }
    }

    fn pump_events(&mut self, _ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::SearchPage(page) => self.ui.apply_search_page(page),
                Event::Stats(stats) => self.ui.apply_stats(stats),
                Event::ClipsChanged => {
                    self.ui.refresh();
                    if let Some(tx) = &self.store_tx {
                        let _ = tx.send(StoreCmd::RefreshStats);
                    }
                }
                Event::Toast(message) => self.ui.toast(message),
                Event::PastePayload(payload) => {
                    let Some((pending_id, plain)) = self.paste_pending else {
                        continue;
                    };
                    if payload.clip_id != pending_id {
                        continue;
                    }
                    self.paste_pending = None;
                    self.perform_paste(payload, plain);
                }
                _ => {}
            }
        }
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: UiAction) {
        match action {
            UiAction::Search {
                text,
                filter,
                time_range,
                custom_day,
                custom_end_day,
                offset,
                trash,
                gen,
            } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::Search(SearchQuery {
                        text,
                        filter,
                        time_range,
                        custom_day,
                        custom_end_day,
                        offset,
                        limit: 500,
                        trash,
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
            UiAction::RequestPastePayload(_) => {}
            UiAction::Favorite { id, favorited } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::SetFavorited { id, favorited });
                }
            }
            UiAction::Pin { id, pinned } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::SetPinned { id, pinned });
                }
            }
            UiAction::Delete { ids } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::Delete(ids));
                }
                self.ui.refresh();
            }
            UiAction::RestoreTrash { ids } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::Restore(ids));
                }
                self.ui.refresh();
            }
            UiAction::PurgeTrash { ids } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::Purge(ids));
                }
                self.ui.refresh();
            }
            UiAction::EmptyTrash => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::EmptyTrash);
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
            UiAction::OpenManager => {}
            UiAction::OpenPicker => {
                // 主界面不隐藏：主进程把候选窗带到前台，管理台保持打开。
                if crate::sys::thread::activate_existing_instance_with_target(self.target_hwnd) {
                    self.ui.toast("已打开候选窗（管理台保持打开）");
                } else {
                    self.ui.toast("主程序未运行");
                }
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
                self.notify_main_settings();
            }
            UiAction::SetPause(paused) => {
                self.settings.pause_capture = paused;
                self.persist_settings();
                self.notify_main_settings();
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
            UiAction::PreviewGlassOpacity(_) => {
                // 管理台不透明，透明度只影响主候选窗；保存后主进程会应用。
                ctx.request_repaint();
            }
        }
    }
}

impl eframe::App for ManagerApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.positioned {
            self.init_window(ctx, frame);
        }
        if self.init_error.is_some() {
            if let Some(err) = &self.init_error {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(crate::ui::theme::window_bg()))
                    .show(ctx, |ui| {
                        ui.add_space(40.0);
                        ui.label(crate::ui::theme::title("ClipNest 管理台启动失败"));
                        ui.label(crate::ui::theme::weak(err.clone()));
                    });
            }
            return;
        }
        if self.quitting {
            return;
        }
        self.pump_events(ctx);
        let actions = self.ui.show(ctx, &mut self.thumbs);
        for action in actions {
            self.handle_action(ctx, action);
        }
    }
}

impl Drop for ManagerApp {
    fn drop(&mut self) {
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreCmd::Shutdown);
        }
    }
}

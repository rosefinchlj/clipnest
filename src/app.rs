//! 应用外壳：窗口生命周期、模式切换（Picker / Manager）、后台事件泵、动作调度。
//!
//! 线程模型（spec §9.1）：
//! - 本文件运行在 UI 线程：唯一创建窗口/调用 egui 的地方；
//! - `StoreHandle` 在独立线程独占 SQLite，`SysHandle` 在独立线程跑 Win32 消息循环；
//! - UI 与两个后台线程只通过 `mpsc` 通道（Event 收、StoreCmd/SysCmd 发）通信，
//!   `Settings` 用 `Arc<Mutex<Settings>>` 共享给后台读取。
//!
//! `--ui-preview <mode>` 模式走独立的假数据预览入口，不启动 Store/Sys。

use crate::model::{
    fmt_size, BackupEvent, ClipId, Event, SearchQuery, Settings, StoreCmd, SysCmd, ThemeMode,
};
use crate::store::StoreHandle;
use crate::sys::SysHandle;
use crate::ui::{ManagerPage, ThumbCache, UiAction, UiState, WindowMode};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// 失焦后延迟隐藏，避免点击托盘菜单/系统弹窗时误隐藏。
const HIDE_ON_BLUR_DELAY: Duration = Duration::from_millis(220);
/// 显示后多久内不因失焦隐藏（防止抢前台的瞬态误判）。
const FOCUS_GRACE: Duration = Duration::from_millis(500);
/// 显示后第二次抢前台的延迟。
const FOCUS_RETRY: Duration = Duration::from_millis(60);
/// 隐藏窗口到真正执行 SendInput 之间的等待（让窗口先移出屏幕；Sys 侧还有重试兜底）。
const PASTE_HIDE_DELAY: Duration = Duration::from_millis(20);
/// 启动后请求一次统计的时间。
const FIRST_STATS_DELAY: Duration = Duration::from_millis(300);
/// 窗口"隐藏"时移到的屏幕外坐标（保持窗口 visible，事件循环才不会停）。
const OFFSCREEN_POS: egui::Pos2 = egui::pos2(-30000.0, -30000.0);

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
    /// 候选窗被用户手动缩放后置位，隐藏时统一落盘（避免拖动过程中反复写 settings.json）。
    picker_size_dirty: bool,

    // ---- 流程状态 ----
    init_error: Option<String>,
    /// 呼出面板前的前台窗口（粘贴目标）。
    target_hwnd: isize,
    /// 等待 Store 返回载荷的粘贴请求：(clip_id, plain)
    paste_pending: Option<(ClipId, bool)>,
    /// 发起粘贴请求时的目标窗口快照（避免等待期间被新的 PickerRequested 覆盖）。
    paste_target: isize,
    /// 已隐藏窗口、等待延时后执行粘贴。
    paste_due: Option<(Instant, crate::model::PastePayload, bool)>,
    unfocused_since: Option<Instant>,
    first_stats_at: Option<Instant>,
    /// 首次显示时刻（失焦隐藏宽限期）。
    shown_at: Option<Instant>,
    /// 显示后第二次抢前台的时刻。
    focus_due: Option<Instant>,
    /// 设置文件最后修改时间（跨进程/外部修改检测）。
    settings_stamp: Option<SystemTime>,
    /// App 创建时刻（调试用：隐藏启动后延时显示）。
    #[allow(dead_code)]
    started_at: Instant,
    /// A5：首次启动的欢迎提示，延迟到面板第一次真正显示时再弹（启动时窗口在屏幕外）。
    welcome_pending: bool,
    quitting: bool,
}

impl ClipNestApp {
    pub fn new(cc: &eframe::CreationContext<'_>, preview: Option<String>) -> Self {
        let settings = crate::config::load_settings();
        crate::ui::theme::set_light(settings.theme == ThemeMode::Light);
        // 截图自检用：预览时可在安装视觉前强制深色，避免用户浅色设置影响素材。
        if preview.is_some() && std::env::var_os("CLIPNEST_PREVIEW_DARK").is_some() {
            crate::ui::theme::set_light(false);
        }
        crate::ui::theme::set_selection_preset(settings.selection_preset);
        // 预览候选窗保留磨砂；预览管理台时关闭，和真实管理台窗口一致（不透明背景）。
        let glass = preview.as_deref().map_or(true, |mode| mode == "picker")
            && std::env::var_os("CLIPNEST_PREVIEW_NO_GLASS").is_none();
        crate::ui::theme::set_glass_enabled(glass);
        // 调试/截图用：临时覆盖磨砂透明度，不写回 settings.json。
        let dev_glass = std::env::var("CLIPNEST_DEV_GLASS")
            .ok()
            .and_then(|value| value.parse::<u8>().ok());
        crate::ui::theme::set_glass_opacity(dev_glass.unwrap_or(settings.glass_opacity));
        crate::logging::init(&crate::config::logs_dir(), &settings.log_level);
        // 排查内存占用时可跳过字体/背景材质（仅调试用）。
        if std::env::var_os("CLIPNEST_SKIP_FONTS").is_none() {
            crate::ui::install_fonts(&cc.egui_ctx);
        }
        crate::ui::install_theme(&cc.egui_ctx);
        // 全局界面缩放（0.8–1.5）：候选窗与管理台都按设置放大/缩小。
        cc.egui_ctx
            .set_zoom_factor(crate::model::clamp_ui_scale(settings.ui_scale));
        // 正常模式：窗口在启动时就创建（因此打开面板是"秒开"），但先移到屏幕外。
        // 预览模式：直接显示。
        let window_visible = preview.is_some();

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

        // A5：首次启动引导 —— 只弹一条欢迎提示并落「已引导」标记；
        // 不强制开机自启（设置里仍可自行开启），也不打断正常使用。
        // 提示延迟到面板首次显示时再弹，避免启动时窗口在屏幕外看不到。
        let welcome_pending = !settings.first_run_done && preview.is_none();
        let mut settings = settings;
        if welcome_pending {
            settings.first_run_done = true;
            if let Ok(mut shared) = settings_shared.lock() {
                *shared = settings.clone();
            }
            if let Err(err) = crate::config::save_settings(&settings) {
                crate::logging::warn(&format!("写入首次启动标记失败：{err}"));
            }
            crate::logging::log("首次启动：已记录引导标记，面板首次显示时提示");
        }

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
            window_visible,
            picker_size_dirty: false,
            init_error,
            target_hwnd: 0,
            paste_pending: None,
            paste_target: 0,
            paste_due: None,
            unfocused_since: None,
            first_stats_at: Some(Instant::now() + FIRST_STATS_DELAY),
            shown_at: None,
            focus_due: None,
            settings_stamp: settings_stamp(),
            started_at: Instant::now(),
            welcome_pending,
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
        if std::env::var_os("CLIPNEST_SKIP_MICA").is_none() {
            match crate::window::apply_backdrop(
                hwnd,
                crate::ui::theme::is_light(),
                crate::ui::theme::glass_opacity(),
            ) {
                Ok(kind) => {
                    crate::logging::log(&format!("窗口背景材质生效: {kind}"));
                    self.backdrop = Some(kind);
                }
                Err(err) => {
                    crate::logging::warn(&format!("{err}；回退自绘半透明深色"));
                    self.backdrop = None;
                }
            }
        } else {
            crate::logging::warn("CLIPNEST_SKIP_MICA 已设置：跳过背景材质（仅调试用）");
        }
        if self.preview.is_none() {
            if self.window_visible {
                self.move_picker_near_cursor(ctx);
            } else {
                // 隐藏启动：窗口保持 visible 但移出屏幕外（隐藏窗口会导致事件循环不再处理唤醒）。
                self.move_offscreen(ctx);
            }
        }
        self.positioned = true;
        self.shown_at = Some(Instant::now());
        self.focus_due = None;
    }

    /// 把窗口移到屏幕外（"隐藏"但保持 visible，保证事件循环继续运行）。
    fn move_offscreen(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(OFFSCREEN_POS));
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
        if self.window_visible {
            // 已显示时再次触发（托盘连点/重复热键）：只抢焦点，不重置界面状态。
            if let Some(hwnd) = self.hwnd {
                crate::window::force_foreground(hwnd);
            }
            self.focus_due = Some(Instant::now() + FOCUS_RETRY);
            self.shown_at = Some(Instant::now());
            ctx.request_repaint();
            return;
        }
        let started = Instant::now();
        self.ui.enter_picker();
        self.ui.refresh();
        self.window_visible = true;
        self.unfocused_since = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        // 候选窗允许拖动边缘缩放，尺寸变化在隐藏时写回 settings.picker_size。
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
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
        if let Some(hwnd) = self.hwnd {
            crate::window::force_foreground(hwnd);
        }
        self.shown_at = Some(Instant::now());
        // A5：面板第一次真正显示时补上欢迎提示（此时窗口已可见）。
        if self.welcome_pending {
            self.welcome_pending = false;
            self.ui.toast(format!(
                "欢迎使用 ClipNest！按 {} 打开面板，右键托盘图标可临时暂停或退出；默认不随系统启动，需要可在设置里开启。",
                self.settings.hotkey_picker.label()
            ));
            crate::logging::log("首次启动：已显示欢迎提示");
        }
        self.focus_due = Some(Instant::now() + FOCUS_RETRY);
        crate::logging::log(&format!(
            "候选窗显示耗时 {}ms（已就绪，直接移入屏幕）",
            started.elapsed().as_millis()
        ));
        ctx.request_repaint();
    }

    /// 隐藏到屏幕外（保留进程与托盘），并把前台还给呼出前的窗口。
    fn hide_window(&mut self, ctx: &egui::Context) {
        self.window_visible = false;
        self.unfocused_since = None;
        self.shown_at = None;
        self.focus_due = None;
        // 拖动边缘缩放过候选窗：隐藏时统一落盘，避免拖动过程反复写 settings.json。
        if self.picker_size_dirty {
            self.picker_size_dirty = false;
            self.persist_settings();
        }
        self.move_offscreen(ctx);
        if self.target_hwnd != 0
            && self.target_hwnd != self.hwnd.unwrap_or(0)
            && crate::sys::util::window_pid(self.target_hwnd) != 0
        {
            crate::window::force_foreground(self.target_hwnd);
        }
        ctx.request_repaint();
    }

    /// 记录用户拖动边缘缩放后的候选窗尺寸（逻辑像素），隐藏时落盘。
    fn note_picker_size(&mut self, ctx: &egui::Context) {
        let Some(rect) = ctx.input(|i| i.viewport().inner_rect) else {
            return;
        };
        let size = [rect.width().round(), rect.height().round()];
        if (size[0] - self.settings.picker_size[0]).abs() < 1.0
            && (size[1] - self.settings.picker_size[1]).abs() < 1.0
        {
            return;
        }
        // 过滤最小化/初始化过程中的异常尺寸，避免把窗口写成 0×0。
        if size[0] < 360.0 || size[1] < 320.0 || size[0] > 2000.0 || size[1] > 1600.0 {
            return;
        }
        self.settings.picker_size = size;
        if let Ok(mut shared) = self.settings_shared.lock() {
            shared.picker_size = size;
        }
        let snapshot = self.settings.clone();
        self.ui.set_settings(&snapshot);
        self.picker_size_dirty = true;
    }

    /// 打开管理台：spawn 独立窗口进程；主窗口（候选窗）保留，不切模式。
    fn open_manager_process(&mut self, ctx: &egui::Context, page: ManagerPage, target_hwnd: isize) {
        let base = format!("{} · 管理", crate::model::APP_TITLE);
        let wanted = format!("{base} · {}", page.key());
        if let Some(hwnd) = crate::window::find_window_by_title(&wanted) {
            crate::window::force_foreground(hwnd);
            self.hide_window(ctx);
            return;
        }
        // 已有其它页面的管理台：先关掉再按请求页面重开（避免"点设置却停在历史页"）。
        if let Some(hwnd) = crate::window::find_window_by_title_prefix(&base) {
            crate::window::request_close_window(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        let Ok(exe) = std::env::current_exe() else {
            self.ui.toast("无法定位 ClipNest 可执行文件");
            return;
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--ui-manager").arg("--page").arg(page.key());
        if target_hwnd != 0 {
            cmd.arg("--target").arg(target_hwnd.to_string());
        }
        match cmd.spawn() {
            Ok(_) => {
                crate::logging::log(&format!("已启动管理台窗口（page={page:?}）"));
                self.ui.toast("已打开管理台");
                self.hide_window(ctx);
            }
            Err(err) => {
                crate::logging::error(&format!("启动管理台失败：{err}"));
                self.ui.toast(format!("启动管理台失败：{err}"));
            }
        }
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
            Event::ManagerRequested { target_hwnd } => {
                self.open_manager_process(ctx, ManagerPage::History, target_hwnd)
            }
            Event::SettingsRequested { target_hwnd } => {
                self.open_manager_process(ctx, ManagerPage::Settings, target_hwnd)
            }
            Event::QuitRequested => self.request_quit(ctx),
            Event::ClipsChanged { .. } => {
                self.ui.refresh();
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::RefreshStats);
                }
            }
            Event::SearchPage(page) => self.ui.apply_search_page(page),
            Event::SourceApps(apps) => self.ui.apply_source_apps(apps),
            Event::Stats(stats) => self.ui.apply_stats(stats),
            Event::PastePayload(payload) => {
                // 先校验再 take：过期载荷（快速换条目/连按 Enter）不能吞掉新请求。
                let Some((pending_id, _)) = self.paste_pending else {
                    return;
                };
                if payload.clip_id != pending_id {
                    crate::logging::warn(&format!(
                        "忽略过期粘贴载荷：payload={} pending={}",
                        payload.clip_id, pending_id
                    ));
                    return;
                }
                let Some((_, plain)) = self.paste_pending.take() else {
                    return;
                };
                crate::logging::log("粘贴：载荷到达，移出面板并发送");
                self.hide_window(ctx);
                // 延迟到窗口移出屏幕后再让 Sys 恢复前台并 SendInput（20ms 足够，Sys 侧还有前台重试）。
                let due = Instant::now() + PASTE_HIDE_DELAY;
                self.paste_due = Some((due, payload, plain));
                ctx.request_repaint_after(PASTE_HIDE_DELAY);
            }
            Event::PasteResult { ok, reason } => {
                if ok {
                    self.ui.toast("已粘贴");
                    if self.settings.move_to_top_after_paste {
                        self.ui.refresh();
                    }
                } else {
                    let message = format!(
                        "已复制，请手动粘贴（{}）",
                        reason.unwrap_or_else(|| "目标窗口不可用".into())
                    );
                    // 窗口已隐藏时用户看不到 toast：重新显示面板承载提示。
                    if !self.window_visible {
                        self.show_picker_window(ctx);
                    }
                    self.ui.toast(message);
                }
            }
            Event::Toast(message) => self.ui.toast(message),
            Event::Backup(event) => match event {
                BackupEvent::Created(report) => self.ui.toast(format!(
                    "已创建完整备份：{} 条 / {} 张图片 / {} 个文件副本，共 {}",
                    report.clips,
                    report.images,
                    report.favorite_files,
                    fmt_size(report.bytes)
                )),
                BackupEvent::Inspected(preview) => self.ui.set_backup_preview(*preview),
                BackupEvent::Restored(report) => self.ui.toast(format!(
                    "恢复完成：新增 {} 条，跳过 {} 条（重复或缺失），备份大小 {}",
                    report.inserted,
                    report.skipped,
                    fmt_size(report.bytes)
                )),
                BackupEvent::Failed(reason) => self.ui.toast(reason),
            },
            Event::SettingsUpdated(next) => {
                let next = *next;
                let hotkey_changed = next.hotkey_picker != self.settings.hotkey_picker;
                let pause_changed = next.pause_capture != self.settings.pause_capture;
                let theme_changed = next.theme != self.settings.theme;
                let glass_changed = next.glass_opacity != self.settings.glass_opacity;
                let preset_changed = next.selection_preset != self.settings.selection_preset;
                self.settings = next.clone();
                if let Ok(mut shared) = self.settings_shared.lock() {
                    *shared = next.clone();
                }
                self.ui.set_settings(&next);
                crate::ui::theme::set_glass_opacity(next.glass_opacity);
                if theme_changed || glass_changed || preset_changed {
                    crate::ui::theme::set_light(next.theme == ThemeMode::Light);
                    crate::ui::theme::set_selection_preset(next.selection_preset);
                    crate::ui::install_theme(ctx);
                    if let Some(hwnd) = self.hwnd {
                        let _ = crate::window::apply_backdrop(
                            hwnd,
                            crate::ui::theme::is_light(),
                            crate::ui::theme::glass_opacity(),
                        );
                    }
                }
                if let Some(tx) = &self.sys_tx {
                    if hotkey_changed {
                        let _ = tx.send(SysCmd::SetHotkeys {
                            picker: next.hotkey_picker,
                        });
                    }
                    if pause_changed {
                        let _ = tx.send(SysCmd::SetPause(next.pause_capture));
                    }
                }
                ctx.request_repaint();
            }
            Event::HotkeyStatus { ok, reason } => {
                self.ui.apply_hotkey_status(ok, reason.clone());
                if ok {
                    match reason {
                        Some(note) => self.ui.toast(format!("快捷键已生效：{note}")),
                        None => self.ui.toast(format!(
                            "快捷键已生效：{}",
                            self.settings.hotkey_picker.label()
                        )),
                    }
                } else {
                    self.ui.toast(format!(
                        "快捷键注册失败：{}（请换一个组合）",
                        reason.unwrap_or_else(|| "可能被其他程序占用".into())
                    ));
                }
            }
            Event::TempPauseChanged(pause) => {
                self.ui.set_temp_pause(pause);
                ctx.request_repaint();
            }
            Event::EncryptionChanged { enabled, error } => {
                match error {
                    None => {
                        self.settings.encrypt_db = enabled;
                        self.ui.set_encryption_enabled(enabled);
                        self.persist_settings();
                        self.ui.toast(if enabled {
                            "已加密本地数据库（当前 Windows 用户密钥）"
                        } else {
                            "已关闭数据库加密"
                        });
                    }
                    Some(reason) => {
                        self.settings.encrypt_db = !enabled;
                        self.ui.set_encryption_enabled(!enabled);
                        self.persist_settings();
                        self.ui.toast(format!("数据库加密切换失败：{reason}"));
                    }
                }
                ctx.request_repaint();
            }
            Event::FileCopyState { clip_id, state } => {
                self.ui.apply_file_copy_state(clip_id, state);
                ctx.request_repaint();
            }
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
                target_hwnd: self.paste_target,
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
                time_range,
                time_basis,
                custom_day,
                custom_end_day,
                match_mode,
                source_app,
                sort,
                offset,
                trash,
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
                        time_range,
                        time_basis,
                        custom_day,
                        custom_end_day,
                        match_mode,
                        source_app,
                        sort,
                        offset,
                        limit,
                        trash,
                        gen,
                    }));
                }
            }
            UiAction::ListSourceApps => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::ListSourceApps);
                }
            }
            UiAction::Paste { id, plain } => {
                self.paste_pending = Some((id, plain));
                self.paste_target = self.target_hwnd;
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::GetPastePayload(id));
                }
            }
            UiAction::PreviewGlassOpacity(opacity) => {
                crate::ui::theme::set_glass_opacity(opacity);
                if let Some(hwnd) = self.hwnd {
                    let _ =
                        crate::window::apply_backdrop(hwnd, crate::ui::theme::is_light(), opacity);
                }
                ctx.request_repaint();
            }
            UiAction::RequestPastePayload(_) => {
                // UI 的 Paste 动作已携带载荷请求（GetPastePayload），这里不再重复请求，
                // 否则 Store 每次取载荷都会 bump_used，导致一次粘贴 use_count +2。
            }
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
            UiAction::Clear {
                delete_blobs,
                include_favorites,
            } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::Clear {
                        delete_blobs,
                        include_favorites,
                    });
                }
                self.ui.refresh();
            }
            UiAction::SaveFavoriteFiles(id) | UiAction::RetryFileCopy(id) => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::SaveFavoriteFiles(id));
                }
            }
            UiAction::SetTempPause(pause) => {
                // A3：临时暂停只走运行期 Sys 命令，不写 settings.json。
                if let Some(tx) = &self.sys_tx {
                    let _ = tx.send(SysCmd::SetTempPause(pause));
                }
                self.ui.set_temp_pause(pause);
            }
            UiAction::IgnoreSourceApp(app) => {
                // A3：一键忽略来源应用（密码管理器等），立即落盘供 Sys 线程读取。
                if self.ui.add_ignore_app(&app) {
                    self.settings.ignore_apps = self.ui.ignore_apps().to_vec();
                    if let Ok(mut shared) = self.settings_shared.lock() {
                        *shared = self.settings.clone();
                    }
                    self.persist_settings();
                    self.ui
                        .toast(format!("已忽略来源应用：{}（设置已保存）", app.trim()));
                } else {
                    self.ui.toast(format!("{} 已在忽略名单中", app.trim()));
                }
            }
            UiAction::OpenManager => {
                self.ui.enter_picker();
                self.open_manager_process(ctx, ManagerPage::Settings, self.target_hwnd);
            }
            UiAction::OpenPicker => self.show_picker_window(ctx),
            UiAction::HideWindow => self.hide_window(ctx),
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
            UiAction::SetDbEncryption(enabled) => {
                self.settings.encrypt_db = enabled;
                self.persist_settings();
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::SetEncryption { enabled });
                    self.ui.toast("正在切换数据库加密…");
                } else {
                    self.ui.toast("Store 未就绪，无法切换加密");
                }
            }
            UiAction::ExportJson => {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                let default_name = format!("clipnest-export-{stamp}.json");
                if let Some(path) = crate::ui::export_dialog::choose_export_path(&default_name) {
                    if let Some(tx) = &self.store_tx {
                        let _ = tx.send(StoreCmd::ExportJson { path: path.clone() });
                        self.ui.toast(format!("正在导出到 {}", path.display()));
                    }
                }
            }
            UiAction::CreateBackup => {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                let default_name = format!("clipnest-backup-{stamp}.cnbak");
                if let Some(path) = crate::ui::export_dialog::choose_backup_save_path(&default_name)
                {
                    if let Some(tx) = &self.store_tx {
                        let _ = tx.send(StoreCmd::CreateBackup { path });
                        self.ui.toast("正在创建完整备份…");
                    }
                }
            }
            UiAction::RestoreBackup => {
                if let Some(path) = crate::ui::export_dialog::choose_backup_open_path() {
                    if let Some(tx) = &self.store_tx {
                        let _ = tx.send(StoreCmd::InspectBackup { path });
                        self.ui.toast("正在检查备份文件…");
                    }
                }
            }
            UiAction::ConfirmRestore { path } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::RestoreBackup { path });
                    self.ui.toast("正在恢复备份（合并导入）…");
                }
            }
            UiAction::OpenDataDir => {
                let dir = crate::config::data_dir();
                let _ = std::process::Command::new("explorer.exe")
                    .arg(dir.as_os_str())
                    .spawn();
            }
            UiAction::OpenPath(path) => {
                if let Err(err) = crate::sys::shell::open_path(&path) {
                    self.ui.toast(err);
                }
            }
            UiAction::RevealPath(path) => {
                if let Err(err) = crate::sys::shell::reveal_path(&path) {
                    self.ui.toast(err);
                }
            }
            UiAction::CopyPath(text) => match crate::sys::shell::copy_text(&text) {
                Ok(()) => self.ui.toast("路径已复制到剪贴板"),
                Err(err) => self.ui.toast(format!("复制路径失败：{err}")),
            },
            UiAction::SaveImageAs { id, path } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::SaveImageAs { id, path });
                }
            }
            UiAction::SaveTextAs { id, path } => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::SaveTextAs { id, path });
                }
            }
            UiAction::SetTimeFilterBasis(basis) => {
                // 时间依据是全局设置：候选窗里切换后立即落盘，管理台下次打开即沿用。
                self.settings.time_filter_basis = basis;
                if let Ok(mut shared) = self.settings_shared.lock() {
                    shared.time_filter_basis = basis;
                }
                let snapshot = self.settings.clone();
                self.ui.set_settings(&snapshot);
                self.persist_settings();
            }
            UiAction::RefreshStats => {
                if let Some(tx) = &self.store_tx {
                    let _ = tx.send(StoreCmd::RefreshStats);
                }
            }
            UiAction::SelectManagerPage(_) => {}
        }
    }

    fn apply_settings(&mut self, ctx: &egui::Context, next: Settings) {
        let mut next = next;
        let theme_changed = next.theme != self.settings.theme;
        let glass_changed = next.glass_opacity != self.settings.glass_opacity;
        let preset_changed = next.selection_preset != self.settings.selection_preset;
        let pause_changed = next.pause_capture != self.settings.pause_capture;
        let zoom_changed = (next.ui_scale - self.settings.ui_scale).abs() > 0.001;
        // Sys 托盘可绕过 UI 直接修改 pause_capture/autostart（立即生效）；
        // 若发现 shared 与本侧不一致，说明是 Sys 改的，保存时以 Sys 的运行时值为准，避免回滚。
        if let Ok(shared) = self.settings_shared.lock() {
            if shared.pause_capture != self.settings.pause_capture {
                next.pause_capture = shared.pause_capture;
            }
            if shared.autostart != self.settings.autostart {
                next.autostart = shared.autostart;
            }
        }
        self.settings = next.clone();
        if let Ok(mut shared) = self.settings_shared.lock() {
            *shared = next.clone();
        }
        if let Some(tx) = &self.sys_tx {
            let _ = tx.send(SysCmd::SetHotkeys {
                picker: next.hotkey_picker,
            });
            if pause_changed {
                let _ = tx.send(SysCmd::SetPause(next.pause_capture));
            }
        }
        self.ui.set_settings(&next);
        crate::ui::theme::set_glass_opacity(next.glass_opacity);
        // 全局缩放：设置页保存后立即生效（0.8–1.5）。
        ctx.set_zoom_factor(crate::model::clamp_ui_scale(next.ui_scale));
        if theme_changed || glass_changed || preset_changed || zoom_changed {
            if theme_changed || glass_changed || preset_changed {
                crate::ui::theme::set_light(next.theme == ThemeMode::Light);
                crate::ui::theme::set_selection_preset(next.selection_preset);
                crate::ui::install_theme(ctx);
            }
            if let Some(hwnd) = self.hwnd {
                let _ = crate::window::apply_backdrop(
                    hwnd,
                    crate::ui::theme::is_light(),
                    crate::ui::theme::glass_opacity(),
                );
            }
            ctx.request_repaint();
        }
        self.persist_settings();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(next.picker_size.into()));
        self.ui.toast("设置已保存，快捷键立即生效");
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreCmd::RefreshStats);
        }
    }

    /// 同步 Sys 托盘直接修改的运行时设置（暂停/自启），保持 UI 显示与落盘一致。
    fn sync_external_settings(&mut self) {
        let Ok(shared) = self.settings_shared.lock() else {
            return;
        };
        if shared.pause_capture == self.settings.pause_capture
            && shared.autostart == self.settings.autostart
        {
            return;
        }
        self.settings.pause_capture = shared.pause_capture;
        self.settings.autostart = shared.autostart;
        drop(shared);
        let snapshot = self.settings.clone();
        self.ui.set_settings(&snapshot);
        crate::logging::log(&format!(
            "同步托盘运行时设置：pause_capture={} autostart={}",
            self.settings.pause_capture, self.settings.autostart
        ));
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
        crate::window::close_window_by_title_prefix(&format!("{} · 管理", crate::model::APP_TITLE));
        self.persist_settings();
        // A2：退出清理策略 —— 先把清理命令排进 Store 队列；下面的 `store = None`
        // 会触发 StoreHandle::drop（发送 Shutdown 并 join），保证 Store 线程先处理完
        // ClearForExit、真正落盘后才退出进程。
        let exit_policy = self.settings.exit_clear_policy;
        if exit_policy.clears() {
            if let Some(tx) = &self.store_tx {
                let _ = tx.send(StoreCmd::ClearForExit(exit_policy));
                crate::logging::log(&format!("退出清理：{}", exit_policy.label()));
            }
        }
        if let Some(tx) = &self.sys_tx {
            let _ = tx.send(SysCmd::Shutdown);
        }
        // Drop 会发送 Shutdown 并 join Store 线程。
        self.store = None;
        self.store_tx = None;
        self.sys = None;
        self.sys_tx = None;
        // 优先用原生 WM_CLOSE（比 viewport Close 在部分窗口状态下更可靠），再补一条 Close 命令。
        if let Some(hwnd) = self.hwnd {
            crate::window::request_close_window(hwnd);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    // -----------------------------------------------------------------------
    // 设置文件监听（跨进程/外部修改，保证快捷键改动一定生效）
    // -----------------------------------------------------------------------

    /// 设置文件变化时重载，并以「与当前设置是否不同」为准应用（不重复写文件，避免抖动）。
    fn poll_settings_file(&mut self, ctx: &egui::Context) {
        let stamp = settings_stamp();
        if stamp == self.settings_stamp {
            return;
        }
        // 严格解析：若读到正在写入的半截文件则保持旧值，下个周期重试。
        let next = match crate::config::load_settings_strict() {
            Ok(settings) => settings,
            Err(err) => {
                // 记录本次 stamp，避免同一个坏文件每 500ms 重复解析/刷日志；
                // 文件再次被修改（mtime 变化）时会重新尝试。
                self.settings_stamp = stamp;
                crate::logging::warn(&format!(
                    "设置监听：暂时无法解析 settings.json（{err}），已跳过本次外部修改"
                ));
                return;
            }
        };
        let previous = self.settings.clone();
        self.settings_stamp = stamp;
        if next == previous {
            return;
        }
        let theme_changed = next.theme != previous.theme;
        let glass_changed = next.glass_opacity != previous.glass_opacity;
        let preset_changed = next.selection_preset != previous.selection_preset;
        let pause_changed = next.pause_capture != previous.pause_capture;
        crate::logging::log("检测到 settings.json 变化，正在应用新设置");
        self.settings = next.clone();
        if let Ok(mut shared) = self.settings_shared.lock() {
            *shared = next.clone();
        }
        crate::ui::theme::set_glass_opacity(next.glass_opacity);
        if theme_changed || glass_changed || preset_changed {
            crate::ui::theme::set_light(next.theme == ThemeMode::Light);
            crate::ui::theme::set_selection_preset(next.selection_preset);
            crate::ui::install_theme(ctx);
            if let Some(hwnd) = self.hwnd {
                let _ = crate::window::apply_backdrop(
                    hwnd,
                    crate::ui::theme::is_light(),
                    crate::ui::theme::glass_opacity(),
                );
            }
        }
        if let Some(tx) = &self.sys_tx {
            let _ = tx.send(SysCmd::SetHotkeys {
                picker: next.hotkey_picker,
            });
            if pause_changed {
                let _ = tx.send(SysCmd::SetPause(next.pause_capture));
            }
        }
        self.ui.set_settings(&next);
        ctx.request_repaint();
    }

    // -----------------------------------------------------------------------
    // 失焦自动隐藏
    // -----------------------------------------------------------------------

    fn handle_focus_autohide(&mut self, ctx: &egui::Context) {
        if !self.settings.hide_on_blur || !self.window_visible {
            self.unfocused_since = None;
            return;
        }
        // 刚显示的前 500ms 不隐藏：抢前台/窗口切换存在延迟，避免刚弹出就被误判失焦隐藏。
        if let Some(shown) = self.shown_at {
            if shown.elapsed() < FOCUS_GRACE {
                self.unfocused_since = None;
                ctx.request_repaint_after(Duration::from_millis(120));
                return;
            }
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
        // F5：循环切换磨砂透明度（调试用；正式构建不包含）。
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            let mut next = self.settings.clone();
            next.glass_opacity = match next.glass_opacity {
                75..=100 => 55,
                45..=74 => 35,
                _ => 75,
            };
            self.apply_settings(ctx, next);
            crate::logging::log("dev: cycle glass opacity");
        }
        // F6：深/浅色皮肤切换（调试用，验证主题热切换与落盘；正式构建不包含）。
        if ctx.input(|i| i.key_pressed(egui::Key::F6)) {
            let mut next = self.settings.clone();
            next.theme = if self.settings.theme == ThemeMode::Light {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            };
            self.apply_settings(ctx, next);
            crate::logging::log("dev: toggle theme");
        }
        // F7：把快捷键切成 Alt+`（调试运行时 SetHotkeys 全链路用；正式构建不包含）。
        if ctx.input(|i| i.key_pressed(egui::Key::F7)) {
            let mut next = self.settings.clone();
            next.hotkey_picker = crate::model::HotkeyChord::new(false, true, false, false, 0xC0);
            self.apply_settings(ctx, next);
            crate::logging::log("dev: hotkey -> Alt+`");
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F8)) {
            self.hide_window(ctx);
            crate::logging::log("dev: hide picker");
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F9)) {
            self.show_picker_window(ctx);
            crate::logging::log("dev: show picker");
        }
    }

    /// 调试用：隐藏启动后 `CLIPNEST_DEV_SHOW_MS` 毫秒自动显示候选窗，
    /// 验证"隐藏窗口仍然运行 update、可被事件唤起"这条链路。
    #[cfg(debug_assertions)]
    fn dev_show_after(&mut self, ctx: &egui::Context) {
        if self.window_visible {
            return;
        }
        let Some(ms) = std::env::var("CLIPNEST_DEV_SHOW_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(50));
        if self.started_at.elapsed().as_millis() as u64 >= ms {
            crate::logging::log("dev: show picker from hidden startup");
            self.show_picker_window(ctx);
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

/// 设置文件最后修改时间（跨进程/外部修改检测）。
fn settings_stamp() -> Option<SystemTime> {
    std::fs::metadata(crate::config::settings_path())
        .and_then(|meta| meta.modified())
        .ok()
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
        self.dev_show_after(ctx);

        if let Some(mode) = self.preview.clone() {
            crate::ui::preview::show(ctx, &mode);
            return;
        }

        // 显示后第二次抢前台（首次 Focus 可能被前台锁拒绝）。
        if let Some(due) = self.focus_due {
            if Instant::now() >= due {
                self.focus_due = None;
                if self.window_visible {
                    if let Some(hwnd) = self.hwnd {
                        crate::window::force_foreground(hwnd);
                    }
                }
            } else {
                ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
            }
        }

        self.pump_events(ctx);
        if self.quitting {
            return;
        }
        // 每 500ms 检查一次 settings.json（UI 自己保存/托盘修改/外部编辑都会生效）。
        ctx.request_repaint_after(Duration::from_millis(500));
        self.poll_settings_file(ctx);
        self.sync_external_settings();

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
            // 启动异常必须让用户看到：移到鼠标所在屏幕并抢前台。
            if !self.window_visible {
                self.window_visible = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                self.move_picker_near_cursor(ctx);
                if let Some(hwnd) = self.hwnd {
                    crate::window::force_foreground(hwnd);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            self.render_init_error(ctx);
            return;
        }

        self.handle_focus_autohide(ctx);
        self.flush_paste_due(ctx);
        if self.window_visible {
            self.note_picker_size(ctx);
        }

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

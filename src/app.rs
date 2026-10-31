//! 应用外壳：窗口生命周期、模式切换（Picker / Manager）、后台事件泵、动作调度。
//!
//! 骨架阶段：透明无边框窗口 + 背景材质 + 深色占位面板，用于验证亚克力与窗口链路；
//! 集成阶段由 Lead 补全 Store/Sys/UiState 接线。
//! `--ui-preview <mode>` 模式交给 UI 域的独立预览入口，供 UI 并行开发与截图。

use crate::model::Settings;
use eframe::egui;

pub struct ClipNestApp {
    pub settings: Settings,
    pub preview: Option<String>,
    /// 原生窗口句柄（首帧取得，仅 UI 线程使用）。
    hwnd: Option<isize>,
    backdrop: Option<&'static str>,
    positioned: bool,
    /// 调试用：当前是否处于 Manager 模式（F10 切换，release 不存在）。
    #[allow(dead_code)]
    manager_mode: bool,
}

impl ClipNestApp {
    pub fn new(cc: &eframe::CreationContext<'_>, preview: Option<String>) -> Self {
        let settings = crate::config::load_settings();
        crate::logging::init(&crate::config::logs_dir(), &settings.log_level);
        crate::ui::install_fonts(&cc.egui_ctx);
        crate::ui::install_theme(&cc.egui_ctx);
        Self {
            settings,
            preview,
            hwnd: None,
            backdrop: None,
            positioned: false,
            manager_mode: false,
        }
    }

    /// 调试快捷键（仅 debug）：F8 隐藏、F9 显示、F10 切换 Picker/Manager 模式。
    /// 用于验证窗口命令链路；最终版本由全局快捷键/托盘事件替代。
    #[cfg(debug_assertions)]
    fn dev_hotkeys(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::F8)) {
            self.hide_picker(ctx);
            crate::logging::log("dev: hide picker");
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F9)) {
            self.show_picker(ctx);
            crate::logging::log("dev: show picker");
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F10)) {
            if self.manager_mode {
                self.enter_picker(ctx);
                self.manager_mode = false;
                crate::logging::log("dev: enter picker mode");
            } else {
                self.enter_manager(ctx);
                self.manager_mode = true;
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
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let t = ctx.input(|i| i.time);
        if (1.5..4.0).contains(&t) && !self.manager_mode {
            self.enter_manager(ctx);
            self.manager_mode = true;
            crate::logging::log("dev-switch: enter manager mode");
        } else if t >= 4.0 && self.manager_mode {
            self.enter_picker(ctx);
            self.manager_mode = false;
            crate::logging::log("dev-switch: enter picker mode");
        }
    }

    /// 显示并聚焦 Picker（集成阶段由事件泵/托盘调用）。
    pub fn show_picker(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        if let Some(pos) =
            crate::window::picker_position(self.settings.picker_size.into(), ctx.pixels_per_point())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
        ctx.request_repaint();
    }

    /// 隐藏 Picker（不退出进程）。
    pub fn hide_picker(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// 切到 Manager 模式：带装饰、常规层级、管理台尺寸。
    pub fn enter_manager(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
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
        ctx.request_repaint();
    }

    /// 切回 Picker 模式：无边框、置顶、候选窗尺寸。
    pub fn enter_picker(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::AlwaysOnTop,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            self.settings.picker_size.into(),
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(crate::model::APP_TITLE.to_string()));
        ctx.request_repaint();
    }

    /// 首帧：取 HWND、应用背景材质、把面板放到鼠标所在屏幕中央。
    fn init_native_window(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let Some(hwnd) = crate::window::hwnd_of_frame(frame) else {
            crate::logging::warn("无法取得原生窗口句柄，跳过背景材质与定位");
            self.hwnd = None;
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
            let ppp = ctx.pixels_per_point();
            if let Some(pos) = crate::window::picker_position(self.settings.picker_size.into(), ppp)
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
            }
        }
        self.positioned = true;
    }

    fn picker_placeholder(&self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(crate::model::APP_TITLE)
                    .size(17.0)
                    .color(egui::Color32::from_rgb(230, 237, 243)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "背景：{}",
                        self.backdrop.unwrap_or("半透明兜底")
                    ))
                    .size(11.5)
                    .color(egui::Color32::from_rgb(92, 104, 120)),
                );
            });
        });
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("骨架阶段：Store / Sys / UI 域并行开发中…")
                .size(13.0)
                .color(egui::Color32::from_rgb(139, 152, 169)),
        );
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!(
                "备用快捷键 {} · Win+V {} · 数据目录 {}",
                self.settings.hotkey_picker.label(),
                if self.settings.winv_mode == crate::model::WinVMode::Off {
                    "已关闭"
                } else {
                    "接管中"
                },
                crate::config::data_dir().display()
            ))
            .size(11.5)
            .color(egui::Color32::from_rgb(92, 104, 120)),
        );
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

        let panel = egui::Frame::default()
            .fill(egui::Color32::from_rgba_unmultiplied(13, 17, 23, 215))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22),
            ))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::same(16));
        egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
            self.picker_placeholder(ui);
        });
    }
}

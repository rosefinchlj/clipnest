//! 应用外壳：窗口生命周期、模式切换（Picker / Manager）、后台事件泵、动作调度。
//!
//! 骨架阶段先渲染一个深色占位页；集成阶段由 Lead 补全 Store/Sys 接线。
//! `--ui-preview <mode>` 模式交给 UI 域的独立预览入口，供 UI 并行开发与截图。

use crate::model::Settings;
use eframe::egui;

pub struct ClipNestApp {
    pub settings: Settings,
    pub preview: Option<String>,
}

impl ClipNestApp {
    pub fn new(cc: &eframe::CreationContext<'_>, preview: Option<String>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let settings = crate::config::load_settings();
        crate::logging::init(&crate::config::logs_dir(), &settings.log_level);
        Self { settings, preview }
    }
}

impl eframe::App for ClipNestApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(mode) = self.preview.clone() {
            crate::ui::preview::show(ctx, &mode);
            return;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::from_rgb(14, 17, 22)))
            .show(ctx, |ui| {
                ui.add_space(24.0);
                ui.heading(
                    egui::RichText::new(crate::model::APP_TITLE)
                        .color(egui::Color32::from_rgb(230, 237, 243)),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("骨架阶段：Store / Sys / UI 域并行开发中…")
                        .color(egui::Color32::from_rgb(139, 152, 169)),
                );
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(format!(
                        "备用快捷键：{} · 数据目录：{}",
                        self.settings.hotkey_picker.label(),
                        crate::config::data_dir().display()
                    ))
                    .color(egui::Color32::from_rgb(92, 104, 120)),
                );
            });
    }
}

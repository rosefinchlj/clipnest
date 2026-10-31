//! `--ui-preview` 独立预览模式：不依赖 Store/Sys，用假数据渲染 UI，便于并行开发与截图。
//!
//! 由 ui teammate 实现真正的预览；此处为骨架占位。

use eframe::egui;

/// 渲染指定预览页（`picker` / `manager` / `settings` / `about`）。
pub fn show(ctx: &egui::Context, mode: &str) {
    egui::CentralPanel::default()
        .frame(egui::Frame::default().fill(egui::Color32::from_rgb(14, 17, 22)))
        .show(ctx, |ui| {
            ui.add_space(24.0);
            ui.heading(
                egui::RichText::new(format!("UI 预览：{mode}"))
                    .color(egui::Color32::from_rgb(230, 237, 243)),
            );
            ui.label(
                egui::RichText::new("等待 ui teammate 实现（src/ui/preview.rs）")
                    .color(egui::Color32::from_rgb(139, 152, 169)),
            );
        });
}

//! 内存基线探针（Lead 临时工具，验收后可删）：
//! 最小 eframe/egui 应用的常驻内存，用于区分 ClipNest 自身开销与框架基线。
//!
//! 用法：`cargo run --release --example mem_probe`
//! 可选环境变量：`MEM_PROBE_TRANSPARENT=1`（透明窗口 + 无边框）。

use eframe::egui;

struct Probe {
    frames: u64,
}

impl eframe::App for Probe {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if std::env::var_os("MEM_PROBE_TRANSPARENT").is_some() {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [0.05, 0.06, 0.08, 1.0]
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("ClipNest mem probe");
            ui.label(format!("frame {}", self.frames));
            ui.label("最小 eframe/egui 基线，无字体/Store/Sys 附加");
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

fn main() -> eframe::Result<()> {
    let transparent = std::env::var_os("MEM_PROBE_TRANSPARENT").is_some();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([620.0, 560.0])
        .with_title("mem_probe");
    if transparent {
        viewport = viewport
            .with_transparent(true)
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "mem_probe",
        options,
        Box::new(|_cc| Ok(Box::new(Probe { frames: 0 }))),
    )
}

//! 管理台 · 关于页：版本、快捷键帮助、数据/日志目录、许可与隐私说明。
//!
//! 全部本地信息，不发起任何网络请求（spec §16）。

use crate::model::APP_TITLE;
use crate::ui::{paint, theme, widgets, UiAction, UiState};
use eframe::egui::{self, Pos2, Rect, UiBuilder, Vec2};

/// 快捷键帮助表（spec 附录 B）。
const SHORTCUTS: &[(&str, &str)] = &[
    ("Win+V / Ctrl+`", "呼出/隐藏候选面板（Win+V 可被接管）"),
    ("↑ / ↓", "选择上/下一条"),
    ("PgUp / PgDn", "翻页"),
    ("Home / End", "跳到首条/末条"),
    ("Enter", "粘贴选中项（写剪贴板 + 模拟 Ctrl+V）"),
    ("Ctrl+Enter", "仅复制到剪贴板，不自动粘贴"),
    ("Ctrl+1…9", "直接粘贴当前列表第 1–9 项"),
    ("Tab / Shift+Tab", "切换过滤：全部→文本→图片→文件→收藏"),
    ("Ctrl+P", "收藏/取消收藏选中项"),
    ("Delete", "删除选中项（收藏项需确认）"),
    ("Ctrl+,", "打开设置"),
    ("Esc", "隐藏面板 / 返回"),
    ("Ctrl+F", "管理台：聚焦搜索框"),
    ("Ctrl+A", "管理台：全选"),
    ("Ctrl+E", "管理台：导出 JSON"),
    ("双击行", "管理台：打开右侧详情面板"),
];

/// 关于页入口。
pub(crate) fn show(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    actions: &mut Vec<UiAction>,
) {
    let inner = rect.shrink(theme::PAGE_PAD);
    ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
        egui::ScrollArea::vertical()
            .id_salt("about_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(theme::title("关于"));
                ui.add_space(16.0);
                brand_card(ui);
                ui.add_space(16.0);
                shortcuts_card(ui);
                ui.add_space(16.0);
                paths_card(ui, actions);
                ui.add_space(16.0);
                privacy_card(ui);
                ui.add_space(16.0);
                actions_row(state, ui, actions);
                ui.add_space(12.0);
            });
    });
}

/// 品牌 / 版本卡片。
fn brand_card(ui: &mut egui::Ui) {
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (mark_rect, _) = ui.allocate_exact_size(Vec2::splat(42.0), egui::Sense::hover());
            paint::paint_brand_mark(ui.painter(), mark_rect);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.label(theme::title(APP_TITLE));
                ui.label(theme::aux(format!(
                    "版本 v{} · MIT License · Windows 10 1809+ / Windows 11",
                    env!("CARGO_PKG_VERSION")
                )));
            });
        });
        ui.add_space(10.0);
        ui.label(theme::body_secondary(
            "深色磨砂玻璃剪贴板管理器：复制即入库、Win+V 呼出、文本/图片/文件持久化保存，\
             全本地运行、不联网、不上传任何内容。",
        ));
        ui.add_space(6.0);
        ui.label(theme::weak(format!(
            "构建信息：{} · eframe/egui 0.32.3 · Rust {}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            rustc_version_hint()
        )));
    });
}

/// Rust 版本提示（编译期不可直接读取，展示最低支持版本即可）。
fn rustc_version_hint() -> &'static str {
    "1.85+"
}

/// 快捷键帮助表。
fn shortcuts_card(ui: &mut egui::Ui) {
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(theme::row_title("快捷键"));
        ui.add_space(2.0);
        ui.painter().line_segment(
            [
                Pos2::new(ui.available_rect_before_wrap().left(), ui.available_rect_before_wrap().top() + 2.0),
                Pos2::new(ui.available_rect_before_wrap().right(), ui.available_rect_before_wrap().top() + 2.0),
            ],
            theme::divider(),
        );
        ui.add_space(8.0);
        for (key, desc) in SHORTCUTS {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(150.0, 20.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.label(theme::mono(*key));
                    },
                );
                ui.label(theme::body_secondary(*desc));
            });
            ui.add_space(2.0);
        }
    });
}

/// 数据 / 日志目录 + 快捷操作。
fn paths_card(ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(theme::row_title("数据与日志"));
        ui.add_space(8.0);
        for (label, path) in [
            ("数据目录", crate::config::data_dir().display().to_string()),
            ("数据库", crate::config::db_path().display().to_string()),
            ("日志目录", crate::config::logs_dir().display().to_string()),
            ("配置文件", crate::config::settings_path().display().to_string()),
        ] {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(76.0, 20.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.label(theme::weak(label));
                    },
                );
                ui.label(theme::mono(path));
            });
            ui.add_space(4.0);
        }
        ui.add_space(10.0);
        if widgets::button(ui, "打开数据目录", widgets::ButtonKind::Secondary).clicked() {
            actions.push(UiAction::OpenDataDir);
        }
    });
}

/// 隐私与许可。
fn privacy_card(ui: &mut egui::Ui) {
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(theme::row_title("隐私与许可"));
        ui.add_space(8.0);
        ui.label(theme::body_secondary(
            "· 全本地运行：无 HTTP 客户端、无遥测、无账号体系，数据只存在本机 %APPDATA%\\ClipNest。\n\
             · 密码管理器等敏感内容可通过「Clipboard Viewer Ignore」格式自动跳过，也可配置忽略名单。\n\
             · 日志不记录剪贴板内容全文，只记录类型/大小/来源。\n\
             · 本项目使用 MIT License 开源许可。",
        ));
    });
}

/// 关于页底部操作：退出程序。
fn actions_row(state: &mut UiState, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
    ui.horizontal(|ui| {
        if widgets::button(ui, "退出 ClipNest", widgets::ButtonKind::Danger).clicked() {
            if state.preview {
                state.toast("（预览）退出程序");
            } else {
                actions.push(UiAction::Quit);
            }
        }
        ui.add_space(8.0);
        ui.label(theme::weak("退出后托盘图标消失；也可以用托盘菜单退出。"));
    });
}

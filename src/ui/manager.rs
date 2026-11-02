//! Manager 管理台外壳：自定义标题栏（拖拽/最小化到托盘/关闭）+ 左侧 184px 导航 +
//! 页面路由（历史/收藏/设置/关于）+ 边缘缩放请求。
//!
//! 窗口尺寸/装饰/层级由 Lead 控制，这里只发 `ViewportCommand::StartDrag` /
//! `BeginResize` 这类视图命令，以及 [`UiAction::HideWindow`]。

use crate::ui::keys::{self, ManagerCommand};
use crate::ui::pages;
use crate::ui::thumbs::ThumbCache;
use crate::ui::{paint, theme, widgets, ManagerPage, UiAction, UiState};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, Pos2, Rect, ResizeDirection, Sense,
    UiBuilder, Vec2, ViewportCommand,
};

/// 边缘缩放手柄厚度。
const EDGE: f32 = 6.0;

/// 渲染管理台。
pub(crate) fn show(
    state: &mut UiState,
    ui: &mut egui::Ui,
    thumbs: &mut ThumbCache,
    actions: &mut Vec<UiAction>,
) {
    let full = ui.max_rect();

    // ---- 键盘路由（录制快捷键/确认框打开时不路由）----
    if state.confirm.is_none() && !state.rec.recording {
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        for command in keys::route_manager_keys(ui.ctx(), typing) {
            handle_command(state, command, actions);
        }
    }

    // ---- 标题栏 ----
    title_bar(state, ui, full, actions);

    // ---- 主体：左导航 + 右内容 ----
    let body = Rect::from_min_max(
        Pos2::new(full.left(), full.top() + theme::TITLE_BAR_HEIGHT),
        full.max,
    );
    let nav_rect = Rect::from_min_size(body.min, Vec2::new(theme::NAV_WIDTH, body.height()));
    let content_rect = Rect::from_min_max(
        Pos2::new(body.left() + theme::NAV_WIDTH, body.top()),
        body.max,
    );
    nav(state, ui, nav_rect, actions);
    match state.manager_page {
        ManagerPage::History => pages::history::show(state, ui, content_rect, thumbs, actions),
        ManagerPage::Favorites => pages::favorites::show(state, ui, content_rect, thumbs, actions),
        ManagerPage::Settings => pages::settings::show(state, ui, content_rect, actions),
        ManagerPage::About => pages::about::show(state, ui, content_rect, actions),
    }

    // ---- 边缘缩放（交给 Lead 的窗口命令；预览模式不操作真实窗口）----
    if !state.preview {
        edge_resize(ui, full);
    }
}

/// 处理管理台快捷键。
fn handle_command(state: &mut UiState, command: ManagerCommand, actions: &mut Vec<UiAction>) {
    match command {
        ManagerCommand::FocusSearch => {
            if !matches!(
                state.manager_page,
                ManagerPage::History | ManagerPage::Favorites
            ) {
                state.go_manager_page(ManagerPage::History, actions);
            }
            state.manager_search_focus = true;
        }
        ManagerCommand::SelectAll => {
            state.multi_selected = state.items.iter().map(|i| i.id).collect();
        }
        ManagerCommand::DeleteSelected => {
            let ids: Vec<i64> = if state.multi_selected.is_empty() {
                state.selected_item().map(|i| i.id).into_iter().collect()
            } else {
                state.multi_selected.iter().copied().collect()
            };
            state.request_delete_ids(ids, actions);
        }
        ManagerCommand::Export => actions.push(UiAction::ExportJson),
        ManagerCommand::Escape => {
            if state.detail_open {
                state.detail_open = false;
            } else if state.manager_page != ManagerPage::History {
                state.go_manager_page(ManagerPage::History, actions);
            } else if state.preview {
                state.toast("（预览）Esc：返回 / 关闭管理台");
            } else {
                actions.push(UiAction::HideWindow);
            }
        }
    }
}

/// 自定义标题栏（高 40）：品牌 + 拖拽区 + 最小化到托盘 / 关闭。
fn title_bar(state: &mut UiState, ui: &mut egui::Ui, full: Rect, actions: &mut Vec<UiAction>) {
    let rect = Rect::from_min_size(full.min, Vec2::new(full.width(), theme::TITLE_BAR_HEIGHT));
    let painter = ui.painter().clone();
    painter.rect_filled(
        rect,
        CornerRadius {
            nw: theme::RADIUS_WINDOW,
            ne: theme::RADIUS_WINDOW,
            sw: 0,
            se: 0,
        },
        theme::panel_base(),
    );
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        theme::divider(),
    );

    // 标题栏空白区域可拖动窗口（右侧按钮区域除外）
    let drag_rect = Rect::from_min_size(
        rect.min,
        Vec2::new((rect.width() - 96.0).max(1.0), rect.height()),
    );
    let drag = ui.interact(drag_rect, ui.id().with("title_drag"), Sense::drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.hovered() || drag.dragged() {
        ui.ctx().set_cursor_icon(if drag.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }

    // 右侧窗口按钮
    let close_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 44.0, rect.top()),
        Vec2::new(44.0, rect.height()),
    );
    let min_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 88.0, rect.top()),
        Vec2::new(44.0, rect.height()),
    );
    let close_resp = ui.interact(close_rect, ui.id().with("win_close"), Sense::click());
    let min_resp = ui.interact(min_rect, ui.id().with("win_min"), Sense::click());
    if close_resp.hovered() {
        painter.rect_filled(
            close_rect,
            CornerRadius::same(0),
            Color32::from_rgba_unmultiplied(0xFF, 0x6B, 0x6B, 40),
        );
    }
    if min_resp.hovered() {
        painter.rect_filled(
            min_rect,
            CornerRadius::same(0),
            theme::hover_fill(),
        );
    }
    paint::paint_minus(
        &painter,
        min_rect.shrink(10.0),
        if min_resp.hovered() {
            theme::text()
        } else {
            theme::text_secondary()
        },
    );
    paint::paint_close(
        &painter,
        close_rect.shrink(16.0),
        if close_resp.hovered() {
            theme::text()
        } else {
            theme::text_secondary()
        },
    );

    // 品牌
    let mark = Rect::from_min_size(
        Pos2::new(rect.left() + 14.0, rect.top() + 11.0),
        Vec2::splat(18.0),
    );
    paint::paint_brand_mark(&painter, mark);
    painter.text(
        Pos2::new(mark.right() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        crate::model::APP_TITLE,
        theme::font_body(),
        theme::text(),
    );

    // 其余区域可拖拽窗口
    let drag_rect = Rect::from_min_max(
        Pos2::new(rect.left(), rect.top()),
        Pos2::new(min_rect.left(), rect.bottom()),
    );
    let drag_resp = ui.interact(drag_rect, ui.id().with("title_drag"), Sense::drag());
    if drag_resp.drag_started() && !state.preview {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if drag_resp.hovered() && drag_resp.drag_started() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    // 关闭 / 最小化 → 隐藏到托盘（spec §5.1）
    if close_resp.clicked() || min_resp.clicked() {
        if state.preview {
            state.toast("（预览）关闭/最小化：隐藏到托盘");
        } else {
            actions.push(UiAction::HideWindow);
        }
    }
}

/// 左侧导航（184px）：历史 / 收藏 / 设置 / 关于 + 底部统计与「暂停记录」。
fn nav(state: &mut UiState, ui: &mut egui::Ui, rect: Rect, actions: &mut Vec<UiAction>) {
    let painter = ui.painter().clone();
    painter.rect_filled(
        rect,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: theme::RADIUS_WINDOW,
            se: 0,
        },
        theme::panel_bg(),
    );
    painter.line_segment(
        [rect.right_top(), rect.right_bottom()],
        theme::divider(),
    );

    let mut y = rect.top() + 14.0;
    let item_w = rect.width() - 24.0;
    for page in [
        ManagerPage::History,
        ManagerPage::Favorites,
        ManagerPage::Settings,
        ManagerPage::About,
    ] {
        let item_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 12.0, y),
            Vec2::new(item_w, 38.0),
        );
        let resp = ui.interact(
            item_rect,
            ui.id().with(("nav", page.label())),
            Sense::click(),
        );
        let active = state.manager_page == page;
        if active {
            painter.rect_filled(
                item_rect,
                CornerRadius::same(theme::RADIUS_ROW),
                theme::accent_fill(),
            );
            let bar = Rect::from_min_max(
                Pos2::new(item_rect.left() + 1.0, item_rect.top() + 10.0),
                Pos2::new(item_rect.left() + 3.0, item_rect.bottom() - 10.0),
            );
            painter.rect_filled(bar, CornerRadius::same(1), theme::accent());
        } else if resp.hovered() {
            painter.rect_filled(
                item_rect,
                CornerRadius::same(theme::RADIUS_ROW),
                theme::hover_fill(),
            );
        }
        let icon_color = if active {
            theme::accent()
        } else if resp.hovered() {
            theme::text()
        } else {
            theme::text_secondary()
        };
        let icon_rect = Rect::from_center_size(
            Pos2::new(item_rect.left() + 24.0, item_rect.center().y),
            Vec2::splat(16.0),
        );
        paint_nav_icon(&painter, icon_rect, page, icon_color);
        painter.text(
            Pos2::new(item_rect.left() + 42.0, item_rect.center().y),
            Align2::LEFT_CENTER,
            page.label(),
            theme::font_body(),
            if active {
                theme::text()
            } else if resp.hovered() {
                theme::text()
            } else {
                theme::text_secondary()
            },
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() && !active {
            state.go_manager_page(page, actions);
        }
        y += 42.0;
    }

    // 底部：统计 + 暂停记录开关 + 版本
    let bottom = Rect::from_min_max(
        Pos2::new(rect.left() + 12.0, rect.bottom() - 118.0),
        Pos2::new(rect.right() - 12.0, rect.bottom() - 12.0),
    );
    painter.line_segment(
        [bottom.left_top(), bottom.right_top()],
        theme::divider(),
    );
    painter.text(
        Pos2::new(bottom.left() + 2.0, bottom.top() + 14.0),
        Align2::LEFT_CENTER,
        format!("共 {} 条 · 收藏 {} 条", keys::fmt_count(state.stats.total), keys::fmt_count(state.stats.pinned)),
        theme::font_aux(),
        theme::text_weak(),
    );
    // 暂停记录开关（立即生效）
    let pause_rect = Rect::from_min_size(
        Pos2::new(bottom.left() + 2.0, bottom.top() + 34.0),
        Vec2::new(bottom.width() - 4.0, 26.0),
    );
    let mut child = ui.new_child(UiBuilder::new().max_rect(pause_rect));
    child.label(theme::aux("暂停记录"));
    let switch_rect = Rect::from_center_size(
        Pos2::new(pause_rect.right() - 18.0, pause_rect.center().y),
        Vec2::new(36.0, 20.0),
    );
    let mut paused = state.settings.pause_capture;
    let switch_resp = {
        let mut child = ui.new_child(UiBuilder::new().max_rect(switch_rect));
        widgets::switch(&mut child, &mut paused)
    };
    if switch_resp.changed() {
        state.settings.pause_capture = paused;
        state.settings_draft.pause_capture = paused;
        state.paused = paused;
        actions.push(UiAction::SetPause(paused));
        state.toast(if paused { "已暂停记录" } else { "已恢复记录" });
    }
    painter.text(
        Pos2::new(bottom.left() + 2.0, bottom.bottom()),
        Align2::LEFT_BOTTOM,
        format!("ClipNest v{}", env!("CARGO_PKG_VERSION")),
        theme::font_aux(),
        theme::text_weak().gamma_multiply(0.8),
    );
}

/// 导航图标（几何自绘）：历史=剪贴板、收藏=星、设置=齿轮、关于=信息。
fn paint_nav_icon(
    painter: &egui::Painter,
    rect: Rect,
    page: ManagerPage,
    color: Color32,
) {
    match page {
        ManagerPage::History => {
            paint::paint_clipboard_big(painter, rect.center(), rect.height() * 0.92, color);
        }
        ManagerPage::Favorites => {
            paint::paint_star(painter, rect.center(), rect.height() * 0.46, color, false);
        }
        ManagerPage::Settings => paint::paint_gear(painter, rect, color),
        ManagerPage::About => {
            let c = rect.center();
            painter.circle_stroke(c, rect.height() * 0.46, egui::Stroke::new(1.4_f32, color));
            painter.circle_filled(
                Pos2::new(c.x, c.y - rect.height() * 0.20),
                rect.height() * 0.07,
                color,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x, c.y - rect.height() * 0.02),
                    Pos2::new(c.x, c.y + rect.height() * 0.24),
                ],
                egui::Stroke::new(1.6_f32, color),
            );
        }
    }
}

/// 四边/四角拖拽缩放请求（`ViewportCommand::BeginResize`）。
fn edge_resize(ui: &mut egui::Ui, full: Rect) {
    let handles: [(Rect, ResizeDirection); 8] = [
        (
            Rect::from_min_max(full.left_top(), Pos2::new(full.left() + EDGE, full.bottom())),
            ResizeDirection::West,
        ),
        (
            Rect::from_min_max(
                Pos2::new(full.right() - EDGE, full.top()),
                full.right_bottom(),
            ),
            ResizeDirection::East,
        ),
        (
            Rect::from_min_max(full.left_top(), Pos2::new(full.right(), full.top() + EDGE)),
            ResizeDirection::North,
        ),
        (
            Rect::from_min_max(
                Pos2::new(full.left(), full.bottom() - EDGE),
                full.right_bottom(),
            ),
            ResizeDirection::South,
        ),
        (
            Rect::from_min_max(full.left_top(), full.left_top() + Vec2::splat(EDGE * 2.0)),
            ResizeDirection::NorthWest,
        ),
        (
            Rect::from_min_max(
                Pos2::new(full.right() - EDGE * 2.0, full.top()),
                Pos2::new(full.right(), full.top() + EDGE * 2.0),
            ),
            ResizeDirection::NorthEast,
        ),
        (
            Rect::from_min_max(
                Pos2::new(full.left(), full.bottom() - EDGE * 2.0),
                Pos2::new(full.left() + EDGE * 2.0, full.bottom()),
            ),
            ResizeDirection::SouthWest,
        ),
        (
            Rect::from_min_max(
                full.right_bottom() - Vec2::splat(EDGE * 2.0),
                full.right_bottom(),
            ),
            ResizeDirection::SouthEast,
        ),
    ];
    for (i, (rect, dir)) in handles.iter().enumerate() {
        let resp = ui.interact(
            *rect,
            ui.id().with(("resize", i)),
            Sense::drag(),
        );
        if resp.drag_started() {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::BeginResize(*dir));
        }
    }
}

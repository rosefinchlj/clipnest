//! 管理台 · 回收站页：手动删除的记录在此保留 2 天，可恢复或彻底删除。
//!
//! 容量自动清理与「清空历史」不会进入这里；回收站条目在保留期内继续占用磁盘，
//! 到期后由 Store 的 retention 物理删除。

use crate::model::{fmt_relative_time, fmt_size, now_ms, ClipItem, ClipKind, TRASH_RETENTION_MS};
use crate::ui::thumbs::ThumbCache;
use crate::ui::{paint, theme, widgets, UiAction, UiState};
use eframe::egui::{
    self, Align2, CornerRadius, FontId, Pos2, Rect, Sense, StrokeKind, UiBuilder, Vec2,
};

const ROW_HEIGHT: f32 = 66.0;

/// 回收站页入口。
pub(crate) fn show(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    _thumbs: &mut ThumbCache,
    actions: &mut Vec<UiAction>,
) {
    let inner = rect.shrink(theme::PAGE_PAD);
    let painter = ui.painter().clone();

    // ---- 标题行 ----
    let title_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), 26.0));
    painter.text(
        title_rect.left_center(),
        Align2::LEFT_CENTER,
        "回收站",
        FontId::new(theme::SIZE_TITLE, egui::FontFamily::Proportional),
        theme::text(),
    );
    let total = state.total;
    painter.text(
        Pos2::new(title_rect.left() + 76.0, title_rect.center().y),
        Align2::LEFT_CENTER,
        if total == 0 {
            "暂无已删除记录 · 手动删除的记录保留 2 天".to_string()
        } else {
            format!("{total} 条 · 保留 2 天，到期自动彻底删除")
        },
        theme::font_aux(),
        theme::text_weak(),
    );

    // ---- 工具栏：搜索 + 清空回收站 ----
    let toolbar_rect = Rect::from_min_size(
        Pos2::new(inner.left(), title_rect.bottom() + 12.0),
        Vec2::new(inner.width(), 40.0),
    );
    let search_w = (inner.width() * 0.34).clamp(200.0, 300.0);
    let search_rect = Rect::from_min_size(toolbar_rect.min, Vec2::new(search_w, 40.0));
    let mut search_changed = false;
    ui.scope_builder(UiBuilder::new().max_rect(search_rect), |ui| {
        let out = widgets::search_box(
            ui,
            &mut state.search_text,
            "搜索回收站…",
            Some("Ctrl+F"),
            state.manager_search_focus,
        );
        state.manager_search_focus = false;
        search_changed = out.changed || out.cleared;
    });
    if search_changed {
        state.schedule_search(false);
    }

    let clear_w = 104.0;
    let clear_rect = Rect::from_min_size(
        Pos2::new(
            toolbar_rect.right() - clear_w,
            toolbar_rect.center().y - 15.0,
        ),
        Vec2::new(clear_w, 30.0),
    );
    if widgets::button_at(ui, clear_rect, "清空回收站", widgets::ButtonKind::Danger).clicked()
    {
        state.request_empty_trash();
    }

    // ---- 列表卡片 ----
    let card = Rect::from_min_max(
        Pos2::new(inner.left(), toolbar_rect.bottom() + 12.0),
        inner.max,
    );
    painter.rect_filled(card, CornerRadius::same(theme::RADIUS_CARD), theme::card());
    painter.rect_stroke(
        card,
        CornerRadius::same(theme::RADIUS_CARD),
        theme::divider(),
        StrokeKind::Inside,
    );

    let list = card.shrink2(Vec2::new(10.0, 10.0));
    if state.items.is_empty() {
        paint_empty(state, ui, list);
        return;
    }

    // 虚拟化列表：只绘制可视行；接近底部时按分页继续向 Store 取下一页。
    let total = state.items.len();
    let mut child = ui.new_child(UiBuilder::new().max_rect(list));
    let out = egui::ScrollArea::vertical()
        .id_salt("trash_list")
        .auto_shrink([false, false])
        .show_rows(&mut child, ROW_HEIGHT, total, |ui, range| {
            ui.set_width(list.width());
            for index in range {
                let Some(item) = state.items.get(index).cloned() else {
                    continue;
                };
                let (row_rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), ROW_HEIGHT),
                    Sense::hover(),
                );
                paint_row(state, ui, row_rect, &item, actions);
                if index + 1 < total {
                    ui.painter().line_segment(
                        [row_rect.left_bottom(), row_rect.right_bottom()],
                        egui::Stroke::new(1.0_f32, theme::divider().color.gamma_multiply(0.5)),
                    );
                }
            }
        });
    if out.state.offset.y + list.height() >= out.content_size.y - 240.0 {
        state.request_more(actions);
    }
}

/// 空状态：图标 + 文案。
fn paint_empty(state: &UiState, ui: &mut egui::Ui, rect: Rect) {
    let painter = ui.painter();
    let center = rect.center();
    let icon = Rect::from_center_size(Pos2::new(center.x, center.y - 28.0), Vec2::splat(42.0));
    paint::paint_trash(painter, icon, theme::text_weak());
    painter.text(
        Pos2::new(center.x, center.y + 26.0),
        Align2::CENTER_CENTER,
        if state.loading {
            "正在加载回收站…"
        } else if state.search_text.trim().is_empty() {
            "回收站是空的"
        } else {
            "没有匹配的已删除记录"
        },
        theme::font_body(),
        theme::text_secondary(),
    );
    painter.text(
        Pos2::new(center.x, center.y + 48.0),
        Align2::CENTER_CENTER,
        "手动删除的记录会在这里保留 2 天",
        theme::font_aux(),
        theme::text_weak(),
    );
}

/// 单行：类型图标 + 摘要 + 删除时间/剩余时长 + 恢复/彻底删除。
fn paint_row(
    state: &mut UiState,
    ui: &mut egui::Ui,
    row_rect: Rect,
    item: &ClipItem,
    actions: &mut Vec<UiAction>,
) {
    let painter = ui.painter().clone();
    let response = ui.interact(
        row_rect,
        ui.id().with(("trash-row", item.id)),
        Sense::hover(),
    );
    if response.hovered() {
        painter.rect_filled(
            row_rect.shrink2(Vec2::new(4.0, 2.0)),
            CornerRadius::same(theme::RADIUS_ROW),
            theme::hover_fill(),
        );
    }

    let icon_rect = Rect::from_center_size(
        Pos2::new(row_rect.left() + 26.0, row_rect.center().y - 4.0),
        Vec2::splat(18.0),
    );
    paint::paint_kind_icon(&painter, icon_rect, item.kind, theme::text_secondary());

    let text_left = row_rect.left() + 46.0;
    let buttons_w = 160.0;
    let text_right = row_rect.right() - buttons_w - 12.0;
    let title = paint::elided_highlighted(
        ui,
        item.title(),
        theme::font_body(),
        theme::text(),
        state.search_text.as_str(),
        theme::danger(),
        (text_right - text_left).max(40.0),
    );
    painter.galley(
        Pos2::new(text_left, row_rect.top() + 10.0),
        title,
        theme::text(),
    );

    let deleted_at = item.deleted_at.unwrap_or(item.last_used_at);
    let meta = format!(
        "{} · 删除于 {} · {} · {}",
        kind_label(item.kind),
        fmt_relative_time(deleted_at),
        fmt_size(item.byte_len),
        fmt_remaining(deleted_at),
    );
    painter.text(
        Pos2::new(text_left, row_rect.top() + 36.0),
        Align2::LEFT_CENTER,
        meta,
        theme::font_aux(),
        theme::text_weak(),
    );

    let btn_h = 28.0;
    let purge_w = 82.0;
    let restore_w = 64.0;
    let purge_rect = Rect::from_min_size(
        Pos2::new(
            row_rect.right() - purge_w - 12.0,
            row_rect.center().y - btn_h / 2.0,
        ),
        Vec2::new(purge_w, btn_h),
    );
    let restore_rect = Rect::from_min_size(
        Pos2::new(purge_rect.left() - restore_w - 8.0, purge_rect.top()),
        Vec2::new(restore_w, btn_h),
    );
    if widgets::button_at(ui, restore_rect, "恢复", widgets::ButtonKind::Secondary).clicked() {
        state.restore_ids(vec![item.id], actions);
    }
    if widgets::button_at(ui, purge_rect, "彻底删除", widgets::ButtonKind::Danger).clicked() {
        state.request_purge_ids(vec![item.id], actions);
    }
}

fn kind_label(kind: ClipKind) -> &'static str {
    match kind {
        ClipKind::Text => "文本",
        ClipKind::Image => "图片",
        ClipKind::Files => "文件",
    }
}

/// 剩余保留时间：不足 1 小时按分钟显示。
fn fmt_remaining(deleted_at: i64) -> String {
    let left = (deleted_at + TRASH_RETENTION_MS - now_ms()).max(0);
    let hours = left / 3_600_000;
    if left <= 0 {
        "即将清理".to_string()
    } else if hours >= 24 {
        format!("剩余 {} 天", hours / 24)
    } else if hours >= 1 {
        format!("剩余 {hours} 小时")
    } else {
        format!("剩余 {} 分钟", (left / 60_000).max(1))
    }
}

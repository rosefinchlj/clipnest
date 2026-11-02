//! 管理台 · 历史页：搜索/类型过滤 + 虚拟化表格 + 详情面板 + 多选批量操作。
//!
//! 表格列：多选框 / 类型 / 内容摘要 / 来源 / 时间 / 大小 / 收藏（spec §6.4）。

use crate::model::{fmt_relative_time, fmt_size, ClipId, ClipItem, ClipKind};
use crate::ui::thumbs::ThumbCache;
use crate::ui::{paint, theme, widgets, UiAction, UiState};
use eframe::egui::{
    self, Align2, CornerRadius, FontId, Pos2, Rect, Sense, StrokeKind, UiBuilder, Vec2,
};

/// 历史页入口。
pub(crate) fn show(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    thumbs: &mut ThumbCache,
    actions: &mut Vec<UiAction>,
) {
    page(state, ui, rect, thumbs, actions, false);
}

/// 历史/收藏共用页面实现（`favorites=true` 时强制只看收藏）。
pub(crate) fn page(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    thumbs: &mut ThumbCache,
    actions: &mut Vec<UiAction>,
    favorites: bool,
) {
    let inner = rect.shrink(theme::PAGE_PAD);
    let painter = ui.painter().clone();

    // ---- 标题行 ----
    let title_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), 26.0));
    painter.text(
        title_rect.left_center(),
        Align2::LEFT_CENTER,
        if favorites { "收藏" } else { "历史记录" },
        FontId::new(theme::SIZE_TITLE, egui::FontFamily::Proportional),
        theme::text(),
    );
    let count_text = if favorites {
        format!("{} 条收藏 · 不会被自动清理", state.total)
    } else {
        format!("共 {} 条", state.total)
    };
    painter.text(
        Pos2::new(title_rect.left() + 92.0, title_rect.center().y),
        Align2::LEFT_CENTER,
        count_text,
        theme::font_aux(),
        theme::text_weak(),
    );

    // ---- 工具栏：搜索 + 过滤胶囊 + 右侧按钮 ----
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
            if favorites {
                "搜索收藏…"
            } else {
                "搜索历史记录…"
            },
            Some("Ctrl+F"),
            state.manager_search_focus,
        );
        state.manager_search_focus = false;
        search_changed = out.changed || out.cleared;
    });

    if !favorites {
        let chips_rect = Rect::from_min_size(
            Pos2::new(search_rect.right() + theme::GAP_M, toolbar_rect.top() + 7.0),
            Vec2::new(inner.width() * 0.30, theme::CHIP_HEIGHT),
        );
        ui.scope_builder(UiBuilder::new().max_rect(chips_rect), |ui| {
            if widgets::filter_chips(ui, &mut state.manager_filter) {
                state.schedule_search(true);
            }
        });
    }

    // 右侧：导出 + 详情开关
    let btn_h = 30.0;
    let btn_y = toolbar_rect.center().y - btn_h / 2.0;
    let export_w = 74.0;
    let export_rect =
        Rect::from_min_size(Pos2::new(toolbar_rect.right() - export_w, btn_y), Vec2::new(export_w, btn_h));
    if widgets::button_at(ui, export_rect, "导出", widgets::ButtonKind::Secondary).clicked() {
        actions.push(UiAction::ExportJson);
    }
    let detail_w = 88.0;
    let detail_rect = Rect::from_min_size(
        Pos2::new(export_rect.left() - detail_w - 8.0, btn_y),
        Vec2::new(detail_w, btn_h),
    );
    let detail_label = if state.detail_open {
        "隐藏详情"
    } else {
        "显示详情"
    };
    if widgets::button_at(ui, detail_rect, detail_label, widgets::ButtonKind::Ghost).clicked() {
        state.detail_open = !state.detail_open;
    }

    if search_changed {
        state.schedule_search(false);
    }

    // ---- 批量操作条（多选时出现）----
    let mut body_top = toolbar_rect.bottom() + 12.0;
    if !state.multi_selected.is_empty() && !favorites {
        let batch_rect = Rect::from_min_size(
            Pos2::new(inner.left(), body_top),
            Vec2::new(inner.width(), 34.0),
        );
        painter.rect_filled(
            batch_rect,
            CornerRadius::same(theme::RADIUS_ROW),
            theme::accent_fill(),
        );
        painter.text(
            Pos2::new(batch_rect.left() + 14.0, batch_rect.center().y),
            Align2::LEFT_CENTER,
            format!("已选 {} 项", state.multi_selected.len()),
            theme::font_body(),
            theme::text(),
        );
        let del_w = 88.0;
        let del_rect = Rect::from_min_size(
            Pos2::new(batch_rect.right() - del_w - 10.0, batch_rect.center().y - 14.0),
            Vec2::new(del_w, 28.0),
        );
        if widgets::button_at(ui, del_rect, "批量删除", widgets::ButtonKind::Danger).clicked() {
            let ids: Vec<ClipId> = state.multi_selected.iter().copied().collect();
            state.request_delete_ids(ids, actions);
        }
        let pin_w = 88.0;
        let pin_rect = Rect::from_min_size(
            Pos2::new(del_rect.left() - pin_w - 8.0, del_rect.top()),
            Vec2::new(pin_w, 28.0),
        );
        if widgets::button_at(ui, pin_rect, "批量收藏", widgets::ButtonKind::Secondary).clicked()
        {
            let ids: Vec<ClipId> = state.multi_selected.iter().copied().collect();
            for id in ids {
                let pinned = state
                    .items
                    .iter()
                    .find(|i| i.id == id)
                    .map(|i| i.pinned)
                    .unwrap_or(false);
                actions.push(UiAction::Pin {
                    id,
                    pinned: !pinned,
                });
            }
            state.toast("已更新收藏状态");
        }
        body_top = batch_rect.bottom() + 12.0;
    }

    // ---- 表格 + 详情 ----
    let body_rect = Rect::from_min_max(
        Pos2::new(inner.left(), body_top),
        Pos2::new(inner.right(), inner.bottom()),
    );
    let show_detail = state.detail_open && state.selected_item().is_some();
    let (table_rect, detail_rect) = if show_detail {
        let table_w = (body_rect.width() - theme::DETAIL_WIDTH - theme::GAP_M).max(240.0);
        (
            Rect::from_min_size(body_rect.min, Vec2::new(table_w, body_rect.height())),
            Rect::from_min_max(
                Pos2::new(body_rect.left() + table_w + theme::GAP_M, body_rect.top()),
                body_rect.max,
            ),
        )
    } else {
        (
            body_rect,
            Rect::from_min_size(Pos2::ZERO, Vec2::ZERO),
        )
    };

    table(state, ui, table_rect, actions);
    if show_detail && detail_rect.width() > 10.0 {
        detail_panel(state, ui, detail_rect, thumbs, actions);
    }
}

// ---------------------------------------------------------------------------
// 表格
// ---------------------------------------------------------------------------

/// 列宽定义。
struct Columns {
    check_x: f32,
    type_x: f32,
    source_x: f32,
    time_x: f32,
    size_x: f32,
    star_x: f32,
    summary_x: f32,
    summary_w: f32,
}

impl Columns {
    fn new(rect: Rect) -> Self {
        let check_w = 34.0;
        let type_w = 58.0;
        let source_w = 104.0;
        let time_w = 100.0;
        let size_w = 74.0;
        let star_w = 34.0;
        let x = rect.left() + 10.0;
        let check_x = x;
        let type_x = check_x + check_w;
        let summary_x = type_x + type_w;
        let source_x = rect.right() - star_w - size_w - time_w - source_w;
        let time_x = source_x + source_w;
        let size_x = time_x + time_w;
        let star_x = size_x + size_w;
        Self {
            check_x,
            type_x,
            source_x,
            time_x,
            size_x,
            star_x,
            summary_x,
            summary_w: (source_x - summary_x - 8.0).max(40.0),
        }
    }
}

/// 表格：card 背景 + 表头 + 虚拟化行（44px）+ 多选/右键。
fn table(state: &mut UiState, ui: &mut egui::Ui, rect: Rect, actions: &mut Vec<UiAction>) {
    let painter = ui.painter().clone();
    painter.rect_filled(
        rect,
        CornerRadius::same(theme::RADIUS_CARD),
        theme::card(),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(theme::RADIUS_CARD),
        theme::divider(),
        StrokeKind::Inside,
    );

    let header_h = 30.0;
    let header_rect =
        Rect::from_min_size(Pos2::new(rect.left() + 1.0, rect.top() + 1.0), Vec2::new(rect.width() - 2.0, header_h));
    let cols = Columns::new(header_rect);
    // 全选复选框
    let all_selected = !state.items.is_empty()
        && state
            .items
            .iter()
            .all(|i| state.multi_selected.contains(&i.id));
    let mut header_all = all_selected;
    let check_rect = Rect::from_center_size(
        Pos2::new(cols.check_x + 7.0, header_rect.center().y),
        Vec2::splat(16.0),
    );
    if widgets::checkbox(ui, &mut header_all).rect != check_rect {
        // 上面只是借用 checkbox 的交互语义；实际位置由下面重画（保持列对齐）
    }
    // 表头文字
    let head_font = theme::font_aux();
    let head_color = theme::text_weak();
    for (x, label) in [
        (cols.type_x, "类型"),
        (cols.summary_x, "内容摘要"),
        (cols.source_x, "来源"),
        (cols.time_x, "时间"),
        (cols.size_x, "大小"),
    ] {
        painter.text(
            Pos2::new(x, header_rect.center().y),
            Align2::LEFT_CENTER,
            label,
            head_font.clone(),
            head_color,
        );
    }
    // 全选交互（用整格热区，视觉与行内复选框一致）
    let check_zone = Rect::from_min_size(
        Pos2::new(cols.check_x - 2.0, header_rect.top()),
        Vec2::new(22.0, header_h),
    );
    let check_zone_resp = ui.interact(check_zone, ui.id().with("table_all"), Sense::click());
    if check_zone_resp.clicked() {
        if all_selected {
            state.multi_selected.clear();
        } else {
            state.multi_selected = state.items.iter().map(|i| i.id).collect();
        }
    }
    // 表头分隔线
    painter.line_segment(
        [
            Pos2::new(rect.left() + 1.0, header_rect.bottom()),
            Pos2::new(rect.right() - 1.0, header_rect.bottom()),
        ],
        theme::divider(),
    );

    if state.items.is_empty() {
        let msg = if state.loading {
            "正在加载…"
        } else if !state.search_text.trim().is_empty() {
            "没有匹配的记录"
        } else if state.manager_page == crate::ui::ManagerPage::Favorites {
            "还没有收藏，按 Ctrl+P 或点星标收藏"
        } else {
            "还没有记录，复制点什么试试"
        };
        let center = Pos2::new(rect.center().x, header_rect.bottom() + (rect.bottom() - header_rect.bottom()) * 0.4);
        paint::paint_clipboard_big(&painter, center - Vec2::new(0.0, 20.0), 36.0, theme::text_weak());
        painter.text(
            center + Vec2::new(0.0, 22.0),
            Align2::CENTER_TOP,
            msg,
            theme::font_body(),
            theme::text_weak(),
        );
        return;
    }

    // 行区域
    let rows_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 1.0, header_rect.bottom() + 1.0),
        Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0),
    );
    ui.scope_builder(UiBuilder::new().max_rect(rows_rect), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let row_h = theme::TABLE_ROW_HEIGHT;
        let out = egui::ScrollArea::vertical()
            .id_salt("manager_table")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, state.items.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in range {
                    let Some(item) = state.items.get(i).cloned() else {
                        continue;
                    };
                    let selected_single = state.selected == Some(i);
                    let in_multi = state.multi_selected.contains(&item.id);
                    let (row_rect, resp) =
                        ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
                    let hovered = resp.hovered();
                    if selected_single || in_multi {
                        ui.painter().rect_filled(
                            row_rect,
                            CornerRadius::ZERO,
                            theme::accent_fill(),
                        );
                        if selected_single {
                            let bar = Rect::from_min_max(
                                Pos2::new(row_rect.left(), row_rect.top() + 8.0),
                                Pos2::new(row_rect.left() + 2.0, row_rect.bottom() - 8.0),
                            );
                            ui.painter().rect_filled(bar, CornerRadius::same(1), theme::accent());
                        }
                    } else if hovered {
                        ui.painter().rect_filled(
                            row_rect,
                            CornerRadius::ZERO,
                            theme::hover_fill(),
                        );
                    }
                    paint_table_row(
                        state,
                        ui,
                        row_rect,
                        &item,
                        &cols,
                        in_multi,
                        actions,
                    );
                    // 单击：选中 / Ctrl 多选 / Shift 范围选择
                    if resp.clicked() {
                        let ctrl = ui.ctx().input(|i| i.modifiers.ctrl);
                        let shift = ui.ctx().input(|i| i.modifiers.shift);
                        if ctrl {
                            if !state.multi_selected.insert(item.id) {
                                state.multi_selected.remove(&item.id);
                            }
                        } else if shift {
                            if let Some(anchor) = state.last_click_index {
                                let (a, b) = if anchor <= i { (anchor, i) } else { (i, anchor) };
                                for idx in a..=b {
                                    if let Some(it) = state.items.get(idx) {
                                        state.multi_selected.insert(it.id);
                                    }
                                }
                            } else {
                                state.multi_selected.insert(item.id);
                            }
                        } else {
                            state.multi_selected.clear();
                            state.selected = Some(i);
                        }
                        state.last_click_index = Some(i);
                    }
                    if resp.double_clicked() {
                        state.selected = Some(i);
                        state.detail_open = true;
                    }
                    // 右键菜单
                    resp.context_menu(|ui| {
                        ui.set_width(190.0);
                        if widgets::menu_item(ui, if item.pinned { "取消收藏" } else { "收藏" })
                            .clicked()
                        {
                            let pinned = !item.pinned;
                            actions.push(UiAction::Pin {
                                id: item.id,
                                pinned,
                            });
                            ui.close();
                        }
                        if widgets::menu_item(ui, "删除").clicked() {
                            state.selected = Some(i);
                            state.request_delete_ids(vec![item.id], actions);
                            ui.close();
                        }
                    });
                }
            });
        // 滚动到底 → 加载更多
        if out.state.offset.y + rows_rect.height() >= out.content_size.y - 240.0 {
            state.request_more(actions);
        }
    });
}

/// 绘制一行表格内容。
fn paint_table_row(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    item: &ClipItem,
    cols: &Columns,
    in_multi: bool,
    actions: &mut Vec<UiAction>,
) {
    let painter = ui.painter().clone();
    let y = rect.center().y;

    // 行内复选框
    let mut checked = in_multi;
    let check_rect = Rect::from_center_size(
        Pos2::new(cols.check_x + 7.0, y),
        Vec2::splat(16.0),
    );
    let check_resp = {
        let mut child = ui.new_child(UiBuilder::new().max_rect(check_rect));
        widgets::checkbox(&mut child, &mut checked)
    };
    if check_resp.changed() {
        if checked {
            state.multi_selected.insert(item.id);
        } else {
            state.multi_selected.remove(&item.id);
        }
    }

    // 类型
    painter.text(
        Pos2::new(cols.type_x, y),
        Align2::LEFT_CENTER,
        item.kind.label(),
        theme::font_aux(),
        theme::kind_color(item.kind),
    );

    // 摘要（标题单行省略）
    paint::paint_elided(
        ui,
        Pos2::new(cols.summary_x, y - theme::SIZE_BODY * 0.5),
        item.title(),
        theme::font_body(),
        theme::text(),
        cols.summary_w,
    );
    // 来源应用（去掉 .exe）
    let source = item
        .source_app
        .as_deref()
        .map(|s| s.trim_end_matches(".exe").to_string())
        .unwrap_or_else(|| "—".to_string());
    paint::paint_elided(
        ui,
        Pos2::new(cols.source_x, y - theme::SIZE_AUX * 0.5),
        source,
        theme::font_aux(),
        theme::text_secondary(),
        96.0,
    );
    // 时间
    painter.text(
        Pos2::new(cols.time_x, y),
        Align2::LEFT_CENTER,
        fmt_relative_time(item.last_used_at),
        theme::font_aux(),
        theme::text_secondary(),
    );
    // 大小
    painter.text(
        Pos2::new(cols.size_x, y),
        Align2::LEFT_CENTER,
        fmt_size(item.byte_len),
        theme::font_aux(),
        theme::text_secondary(),
    );
    // 收藏星标（可点击）
    let star_center = Pos2::new(cols.star_x + 8.0, y);
    let star_rect = Rect::from_center_size(star_center, Vec2::splat(20.0));
    let star_resp = ui.interact(star_rect, ui.id().with(("table_star", item.id)), Sense::click());
    if item.pinned || star_resp.hovered() {
        paint::paint_star(
            &painter,
            star_center,
            7.0,
            if item.pinned {
                theme::pinned()
            } else {
                theme::text_weak()
            },
            item.pinned,
        );
    }
    if star_resp.clicked() {
        actions.push(UiAction::Pin {
            id: item.id,
            pinned: !item.pinned,
        });
    }
}

// ---------------------------------------------------------------------------
// 详情面板
// ---------------------------------------------------------------------------

/// 右侧详情面板：文本全文（只读可复制）/ 图片大图 / 文件清单 + 操作按钮。
fn detail_panel(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    thumbs: &mut ThumbCache,
    actions: &mut Vec<UiAction>,
) {
    let Some(item) = state.selected_item().cloned() else {
        return;
    };
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(theme::RADIUS_CARD), theme::card());
    painter.rect_stroke(
        rect,
        CornerRadius::same(theme::RADIUS_CARD),
        theme::divider(),
        StrokeKind::Inside,
    );
    let inner = rect.shrink(14.0);

    // 标题 + 类型标签
    let tag_w = paint::text_width(ui, item.kind.label(), theme::font_aux()) + 16.0;
    let tag_rect = Rect::from_min_size(inner.min, Vec2::new(tag_w, 20.0));
    {
        let mut child = ui.new_child(UiBuilder::new().max_rect(tag_rect));
        widgets::tag(&mut child, item.kind.label(), theme::kind_color(item.kind));
    }
    let title_x = tag_rect.right() + 8.0;
    paint::paint_elided(
        ui,
        Pos2::new(title_x, inner.top() + 2.0),
        item.title(),
        theme::font_row_title(),
        theme::text(),
        inner.right() - title_x,
    );
    // 元信息
    let meta = format!(
        "{} · {} · {}",
        fmt_relative_time(item.last_used_at),
        fmt_size(item.byte_len),
        item.source_app.as_deref().unwrap_or("来源未知")
    );
    painter.text(
        Pos2::new(inner.left(), inner.top() + 26.0),
        Align2::LEFT_TOP,
        meta,
        theme::font_aux(),
        theme::text_weak(),
    );

    // 第二行元信息：捕获时间 + 来源窗口标题（这两个字段在列表里不展示）
    let mut meta2 = format!("捕获于 {}", fmt_relative_time(item.created_at));
    if let Some(title) = item.source_title.as_deref() {
        if !title.is_empty() {
            meta2.push_str(" · ");
            meta2.push_str(title);
        }
    }
    painter.text(
        Pos2::new(inner.left(), inner.top() + 42.0),
        Align2::LEFT_TOP,
        meta2,
        theme::font_aux(),
        theme::text_weak(),
    );

    // 操作按钮行
    let btn_y = inner.top() + 66.0;
    let btn_h = 28.0;
    let mut x = inner.left();
    for (label, kind, action) in [
        ("粘贴", widgets::ButtonKind::Primary, 0u8),
        ("仅复制", widgets::ButtonKind::Secondary, 1),
        (
            if item.pinned { "取消收藏" } else { "收藏" },
            widgets::ButtonKind::Secondary,
            2,
        ),
        ("删除", widgets::ButtonKind::Danger, 3),
    ] {
        let w = paint::text_width(ui, label, theme::font_body()) + 22.0;
        let rect = Rect::from_min_size(Pos2::new(x, btn_y), Vec2::new(w, btn_h));
        if widgets::button_at(ui, rect, label, kind).clicked() {
            match action {
                0 | 1 => {
                    actions.push(UiAction::RequestPastePayload(item.id));
                    actions.push(UiAction::Paste {
                        id: item.id,
                        plain: action == 1,
                    });
                }
                2 => {
                    let pinned = !item.pinned;
                    actions.push(UiAction::Pin {
                        id: item.id,
                        pinned,
                    });
                    if let Some(current) = state.items.iter_mut().find(|i| i.id == item.id) {
                        current.pinned = pinned;
                    }
                    if pinned && item.kind == ClipKind::Files {
                        actions.push(UiAction::SaveFavoriteFiles(item.id));
                    }
                    state.toast(if pinned { "已收藏" } else { "已取消收藏" });
                }
                _ => state.request_delete_ids(vec![item.id], actions),
            }
        }
        x += w + 8.0;
    }

    // 内容区
    let content_rect = Rect::from_min_max(
        Pos2::new(inner.left(), btn_y + btn_h + 12.0),
        inner.max,
    );
    ui.scope_builder(UiBuilder::new().max_rect(content_rect), |ui| {
        egui::ScrollArea::vertical()
            .id_salt("detail_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| match item.kind {
                ClipKind::Text => {
                    let mut text = item.text.clone().unwrap_or_default();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .font(theme::font_mono())
                            .text_color(theme::text())
                            .frame(false)
                            .desired_width(f32::INFINITY)
                            .desired_rows(16),
                    );
                    ui.add_space(6.0);
                    ui.label(theme::weak("只读预览 · 可选中复制"));
                }
                ClipKind::Image => {
                    let tex = item.blob_id.as_deref().and_then(|id| thumbs.get(id));
                    let max_w = ui.available_width().min(300.0);
                    if let Some(tex) = tex {
                        let size = tex.size_vec2();
                        let scale = (max_w / size.x).min(1.0);
                        ui.add(
                            egui::Image::new(egui::load::SizedTexture::new(tex.id(), size * scale))
                                .corner_radius(CornerRadius::same(theme::RADIUS_THUMB)),
                        );
                    } else {
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(max_w, 160.0),
                            Sense::hover(),
                        );
                        paint::paint_thumb_placeholder(ui.painter(), rect);
                    }
                    ui.add_space(8.0);
                    ui.label(theme::aux(format!(
                        "{}×{} · {}",
                        item.width.unwrap_or(0),
                        item.height.unwrap_or(0),
                        fmt_size(item.byte_len)
                    )));
                }
                ClipKind::Files => {
                    for file in &item.files {
                        let status = if file.stored {
                            "已保存副本"
                        } else if file.exists {
                            "仅路径"
                        } else {
                            "文件已不存在"
                        };
                        let color = if file.stored {
                            theme::success()
                        } else if file.exists {
                            theme::text_secondary()
                        } else {
                            theme::danger()
                        };
                        ui.horizontal(|ui| {
                            ui.label(theme::body(file.display_name()));
                            ui.label(theme::aux(format!(" {} · {}", fmt_size(file.size), status)));
                            let _ = color;
                        });
                        ui.label(theme::mono(file.path.clone()));
                        ui.add_space(6.0);
                    }
                    if item.files.is_empty() {
                        ui.label(theme::weak("（空文件列表）"));
                    }
                }
            });
    });
}

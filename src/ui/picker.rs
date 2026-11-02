//! Picker 候选窗：搜索框 + 过滤胶囊 + 虚拟化列表 + 底部提示条 + Toast。
//!
//! 交互严格对应 spec §6.1 键盘表与 §6.2 列表项规格；所有副作用通过 [`UiAction`] 交给 Lead。

use crate::model::ClipItem;
use crate::ui::keys::{self, PickerCommand};
use crate::ui::thumbs::ThumbCache;
use crate::ui::{paint, theme, widgets, ManagerPage, UiAction, UiState};
use eframe::egui::{
    self, Align2, Pos2, Rect, UiBuilder, Vec2,
};
use std::time::Instant;

/// 渲染候选窗（`ui` 为面板内容区，外层 frame 由 [`UiState::show`] 提供）。
pub(crate) fn show(
    state: &mut UiState,
    ui: &mut egui::Ui,
    thumbs: &mut ThumbCache,
    actions: &mut Vec<UiAction>,
) {
    // 出现动效：内容 160ms ease-out 淡入（磨砂底由窗口/Acrylic 提供，不跟着闪）。
    let appear = state
        .picker_appear_at
        .map(|at| (at.elapsed().as_secs_f32() / 0.16).min(1.0))
        .unwrap_or(1.0);
    let appear = 1.0 - (1.0 - appear).powi(3);
    ui.set_opacity(0.62 + 0.38 * appear);
    if appear < 0.999 {
        ui.ctx().request_repaint();
    }

    let full = ui.max_rect();
    let hint_top = full.bottom() - theme::HINT_BAR_HEIGHT;
    // 顶部 16px 拖拽条：任意位置按住即可拖动面板（搜索框仍用于输入）。
    const DRAG_STRIP: f32 = 16.0;
    let drag_rect = Rect::from_min_size(full.min, Vec2::new(full.width(), DRAG_STRIP));
    let drag_resp = ui.interact(drag_rect, ui.id().with("picker_drag"), egui::Sense::drag());
    if drag_resp.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    let drag_hot = drag_resp.hovered() || drag_resp.dragged();
    if drag_hot {
        ui.ctx().set_cursor_icon(if drag_resp.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }
    // 抓取点常驻显示（悬停时更亮），让"可拖动"一眼可见。
    let handle = Pos2::new(drag_rect.center().x, drag_rect.center().y);
    let dot_color = if drag_hot {
        theme::text_secondary()
    } else {
        theme::text_weak()
    };
    for offset in [-7.0_f32, 0.0, 7.0] {
        ui.painter().circle_filled(
            Pos2::new(handle.x + offset, handle.y),
            1.6,
            dot_color,
        );
    }
    let search_top = full.top() + DRAG_STRIP;
    let chips_top = search_top + theme::SEARCH_HEIGHT + theme::GAP_S;
    let list_top = chips_top + theme::CHIP_HEIGHT + theme::GAP_S;
    let list_bottom = (hint_top - theme::GAP_XS).max(list_top + theme::ROW_HEIGHT);
    let list_rect = Rect::from_min_max(
        Pos2::new(full.left(), list_top),
        Pos2::new(full.right(), list_bottom),
    );

    // ---- 1) 键盘路由（先于控件绘制，保证搜索框聚焦时 ↑↓/Tab/Enter 仍生效）----
    if state.confirm.is_none() {
        // 搜索框有文本时 Delete 留给文本框编辑（否则删一条记录会很反直觉）
        let allow_delete = !(state.search_focused && !state.search_text.is_empty());
        let commands = keys::route_picker_keys(ui.ctx(), allow_delete);
        for command in commands {
            handle_command(state, command, list_rect.height(), actions);
        }
    }

    // ---- 2) 搜索框（高 40）----
    let hint = state.settings.hotkey_picker.label();
    let search_rect = Rect::from_min_size(
        Pos2::new(full.left(), search_top),
        Vec2::new(full.width(), theme::SEARCH_HEIGHT),
    );
    ui.scope_builder(UiBuilder::new().max_rect(search_rect), |ui| {
        let out = widgets::search_box(
            ui,
            &mut state.search_text,
            "搜索剪贴板历史…",
            Some(&hint),
            state.search_focus,
        );
        state.search_focus = false;
        state.search_focused = out.text_response.has_focus();
        if out.changed || out.cleared {
            state.schedule_search(false);
        }
    });

    // ---- 3) 过滤胶囊 ----
    let chips_rect = Rect::from_min_size(
        Pos2::new(full.left(), chips_top),
        Vec2::new(full.width(), theme::CHIP_HEIGHT),
    );
    ui.scope_builder(UiBuilder::new().max_rect(chips_rect), |ui| {
        if widgets::filter_chips(ui, &mut state.picker_filter) {
            state.schedule_search(true);
        }
    });

    // ---- 4) 列表 / 空状态 ----
    if state.items.is_empty() {
        empty_state(state, ui, list_rect, actions);
    } else {
        render_list(state, ui, thumbs, actions, list_rect);
    }

    // ---- 5) 底部提示条 ----
    let hint_rect = Rect::from_min_size(
        Pos2::new(full.left(), hint_top),
        Vec2::new(full.width(), theme::HINT_BAR_HEIGHT),
    );
    let mut gear_clicked = false;
    ui.scope_builder(UiBuilder::new().max_rect(hint_rect), |ui| {
        let out = widgets::hint_bar(ui, state.total.max(state.items.len()), state.paused);
        gear_clicked = out.gear_clicked;
    });
    if gear_clicked {
        state.request_manager(ManagerPage::Settings, actions);
    }

    // ---- 6) Toast（窗口下部浮起，1.6s 淡出）----
    if let Some(toast) = &state.toast {
        let remaining = toast
            .until
            .saturating_duration_since(Instant::now())
            .as_secs_f32();
        let elapsed = (1.6 - remaining).max(0.0);
        let alpha = (elapsed / 0.10).clamp(0.0, 1.0) * (remaining / 0.22).clamp(0.0, 1.0);
        let text = toast.text.clone();
        widgets::paint_toast(
            ui,
            Pos2::new(full.center().x, list_rect.bottom() - 6.0),
            &text,
            alpha,
        );
    }
}

/// 处理一条语义命令。
fn handle_command(
    state: &mut UiState,
    command: PickerCommand,
    list_height: f32,
    actions: &mut Vec<UiAction>,
) {
    let page = keys::page_rows(list_height, theme::ROW_HEIGHT) as i32;
    let len = state.items.len();
    let select = |state: &mut UiState, next: Option<usize>| {
        state.selected = next;
        state.pending_scroll_row = next;
    };
    match command {
        PickerCommand::Up => select(state, keys::move_selection(state.selected, -1, len)),
        PickerCommand::Down => select(state, keys::move_selection(state.selected, 1, len)),
        PickerCommand::PageUp => select(state, keys::move_selection(state.selected, -page, len)),
        PickerCommand::PageDown => select(state, keys::move_selection(state.selected, page, len)),
        PickerCommand::Home => select(state, keys::jump_selection(false, len)),
        PickerCommand::End => select(state, keys::jump_selection(true, len)),
        PickerCommand::Paste => {
            if let Some(index) = state.selected {
                state.paste_item(index, false, actions);
                if state.preview {
                    state.toast("（预览）Enter：粘贴选中项");
                }
            }
        }
        PickerCommand::PlainCopy => {
            if let Some(index) = state.selected {
                state.paste_item(index, true, actions);
                if state.preview {
                    state.toast("（预览）Ctrl+Enter：仅复制");
                }
            }
        }
        PickerCommand::QuickPaste(index) => {
            if index < len {
                select(state, Some(index));
                state.paste_item(index, false, actions);
            }
        }
        PickerCommand::FilterNext => {
            state.picker_filter = state.picker_filter.next();
            state.schedule_search(true);
        }
        PickerCommand::FilterPrev => {
            state.picker_filter = state.picker_filter.prev();
            state.schedule_search(true);
        }
        PickerCommand::TogglePin => {
            if let Some(index) = state.selected {
                state.toggle_pin(index, actions);
            }
        }
        PickerCommand::Delete => state.request_delete_selected(actions),
        PickerCommand::OpenSettings => state.request_manager(ManagerPage::Settings, actions),
        PickerCommand::Close => {
            if state.preview {
                state.toast("（预览）Esc：隐藏面板");
            } else {
                actions.push(UiAction::HideWindow);
            }
        }
    }
}

/// 键盘选中行时计算最小滚动偏移。
///
/// 只在选中行超出当前可视区时才滚动；行仍在视口内时保持原偏移，
/// 避免每次上下移动都把列表重新居中（到列表底部时尤其容易产生跳动/闪烁）。
pub(crate) fn scroll_offset_for_row(
    current_offset: f32,
    row: usize,
    viewport_height: f32,
    content_height: f32,
) -> f32 {
    let max_offset = (content_height - viewport_height).max(0.0);
    let row_top = row as f32 * theme::ROW_HEIGHT;
    let row_bottom = row_top + theme::ROW_HEIGHT;
    if row_top < current_offset {
        row_top.clamp(0.0, max_offset)
    } else if row_bottom > current_offset + viewport_height {
        (row_bottom - viewport_height).clamp(0.0, max_offset)
    } else {
        current_offset.clamp(0.0, max_offset)
    }
}

/// 虚拟化列表：`ScrollArea::show_rows`，行高 64；到底部附近自动加载下一页。
fn render_list(
    state: &mut UiState,
    ui: &mut egui::Ui,
    thumbs: &mut ThumbCache,
    actions: &mut Vec<UiAction>,
    list_rect: Rect,
) {
    ui.scope_builder(UiBuilder::new().max_rect(list_rect), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("picker_list")
            .auto_shrink([false, false]);
        // 键盘导航后只在必要时滚动（只应用一次，避免覆盖用户滚动）。
        // 之前每次按键都居中选中行，在列表底部会把视口向上拽、造成闪烁。
        if let Some(row) = state.pending_scroll_row.take() {
            let offset = scroll_offset_for_row(
                state.picker_scroll_offset,
                row,
                list_rect.height(),
                state.items.len() as f32 * theme::ROW_HEIGHT,
            );
            scroll = scroll.vertical_scroll_offset(offset);
        }
        let out = scroll.show_rows(
            ui,
            theme::ROW_HEIGHT,
            state.items.len(),
            |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in range {
                    let Some(item) = state.items.get(i).cloned() else {
                        continue;
                    };
                    let thumb = item.blob_id.as_deref().and_then(|id| thumbs.get(id));
                    let selected = state.selected == Some(i);
                    let row = widgets::clip_row(
                        ui,
                        widgets::ClipRow {
                            item: &item,
                            thumb: thumb.as_ref(),
                            selected,
                        },
                    );
                    if row.response.clicked() {
                        state.selected = Some(i);
                        state.multi_selected.clear();
                    }
                    if row.response.double_clicked() {
                        state.selected = Some(i);
                        state.paste_item(i, false, actions);
                    }
                    if row.star_clicked {
                        state.toggle_pin(i, actions);
                    }
                    row.response.context_menu(|ui| {
                        row_context_menu(state, &item, i, actions, ui);
                    });
                }
            },
        );
        state.picker_scroll_offset = out.state.offset.y;
        // 滚动接近底部 → 加载更多（数据库分页）
        if !state.items.is_empty()
            && out.state.offset.y + list_rect.height() >= out.content_size.y - 240.0
        {
            state.request_more(actions);
        }
    });
}

/// 列表行右键菜单（spec §6.1）。
fn row_context_menu(
    state: &mut UiState,
    item: &ClipItem,
    index: usize,
    actions: &mut Vec<UiAction>,
    ui: &mut egui::Ui,
) {
    ui.set_width(200.0);
    if widgets::menu_item(ui, "粘贴").clicked() {
        state.paste_item(index, false, actions);
        ui.close();
    }
    if widgets::menu_item(ui, "仅复制到剪贴板").clicked() {
        state.paste_item(index, true, actions);
        ui.close();
    }
    if widgets::menu_item(ui, "收藏 / 取消收藏").clicked() {
        state.toggle_pin(index, actions);
        ui.close();
    }
    ui.separator();
    let delete_label = if item.pinned {
        "删除（收藏项需确认）"
    } else {
        "删除"
    };
    if widgets::menu_item(ui, delete_label).clicked() {
        state.selected = Some(index);
        state.request_delete_selected(actions);
        ui.close();
    }
}

/// 空状态：无记录 / 搜索无结果（带「清除筛选」按钮）。
fn empty_state(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    actions: &mut Vec<UiAction>,
) {
    let searching = !state.search_text.trim().is_empty()
        || state.picker_filter != crate::model::ClipFilter::All;
    let center_x = rect.center().x;
    let icon_center = Pos2::new(center_x, rect.top() + rect.height() * 0.30);
    let painter = ui.painter();
    paint::paint_clipboard_big(
        painter,
        icon_center,
        44.0,
        if searching {
            theme::text_secondary()
        } else {
            theme::text_secondary().gamma_multiply(0.9)
        },
    );
    painter.text(
        Pos2::new(center_x, icon_center.y + 42.0),
        Align2::CENTER_TOP,
        if searching {
            "没有匹配的记录"
        } else {
            "还没有记录，复制点什么试试"
        },
        theme::font_body(),
        theme::text_secondary(),
    );
    if searching {
        let btn_rect = Rect::from_center_size(
            Pos2::new(center_x, icon_center.y + 84.0),
            Vec2::new(100.0, 30.0),
        );
        if widgets::button_at(ui, btn_rect, "清除筛选", widgets::ButtonKind::Secondary).clicked()
        {
            state.search_text.clear();
            state.picker_filter = crate::model::ClipFilter::All;
            state.schedule_search(true);
        }
    } else if state.loading {
        painter.text(
            Pos2::new(center_x, icon_center.y + 62.0),
            Align2::CENTER_TOP,
            "正在加载…",
            theme::font_aux(),
            theme::text_secondary(),
        );
    }
    let _ = actions;
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipFilter, ClipItem, ClipKind, Settings};
    use crate::ui::UiState;

    fn make_state() -> UiState {
        UiState::new(&egui::Context::default(), &Settings::default())
    }

    fn fill(state: &mut UiState, n: usize) {
        state.active_gen = 1;
        state.apply_search_page(crate::model::SearchPage {
            gen: 1,
            total: n,
            items: (0..n)
                .map(|i| ClipItem {
                    id: i as i64,
                    kind: ClipKind::Text,
                    text: Some(format!("内容 {i}")),
                    ..Default::default()
                })
                .collect(),
            elapsed_ms: 0,
        });
    }

    fn handle(state: &mut UiState, command: PickerCommand) -> Vec<UiAction> {
        let mut actions = Vec::new();
        handle_command(state, command, theme::ROW_HEIGHT * 6.0, &mut actions);
        actions
    }

    #[test]
    fn arrow_selection_moves_and_clamps() {
        let mut s = make_state();
        fill(&mut s, 3);
        handle(&mut s, PickerCommand::Down);
        assert_eq!(s.selected, Some(1));
        handle(&mut s, PickerCommand::Up);
        assert_eq!(s.selected, Some(0));
        handle(&mut s, PickerCommand::Up);
        assert_eq!(s.selected, Some(2), "顶部 Up 回绕到末尾");
        handle(&mut s, PickerCommand::Down);
        assert_eq!(s.selected, Some(0), "末尾 Down 回绕到第一条");
        handle(&mut s, PickerCommand::End);
        assert_eq!(s.selected, Some(2));
        handle(&mut s, PickerCommand::Home);
        assert_eq!(s.selected, Some(0));
    }

    #[test]
    fn keyboard_scroll_is_minimal_and_stable_at_bottom() {
        let row = theme::ROW_HEIGHT;
        let viewport = row * 6.0;
        let content = row * 30.0;
        // 选中行在视口内：不改变滚动位置。
        assert_eq!(scroll_offset_for_row(0.0, 1, viewport, content), 0.0);
        // 超出底部才滚动，且只滚到刚好露出。
        assert_eq!(
            scroll_offset_for_row(0.0, 8, viewport, content),
            row * 3.0
        );
        // 最后一条：滚到底部上限。
        let bottom = content - viewport;
        assert_eq!(
            scroll_offset_for_row(0.0, 29, viewport, content),
            bottom
        );
        // 从最后一条 Up 到上一条：仍在视口内，禁止重新居中（修复闪烁）。
        assert_eq!(
            scroll_offset_for_row(bottom, 28, viewport, content),
            bottom
        );
        // 回绕到顶部时滚回顶部。
        assert_eq!(scroll_offset_for_row(bottom, 0, viewport, content), 0.0);
    }

    #[test]
    fn paging_uses_visible_rows() {
        let mut s = make_state();
        fill(&mut s, 30);
        s.selected = Some(0);
        handle(&mut s, PickerCommand::PageDown);
        assert_eq!(s.selected, Some(6), "6 行可视 → 下移 6");
        handle(&mut s, PickerCommand::PageUp);
        assert_eq!(s.selected, Some(0));
    }

    #[test]
    fn filter_cycle_matches_spec() {
        let mut s = make_state();
        handle(&mut s, PickerCommand::FilterNext);
        assert_eq!(s.picker_filter, ClipFilter::Text);
        handle(&mut s, PickerCommand::FilterPrev);
        assert_eq!(s.picker_filter, ClipFilter::All);
    }

    #[test]
    fn enter_emits_payload_request_then_paste() {
        let mut s = make_state();
        fill(&mut s, 2);
        let actions = handle(&mut s, PickerCommand::Paste);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], UiAction::RequestPastePayload(0)));
        assert!(matches!(
            actions[1],
            UiAction::Paste {
                id: 0,
                plain: false
            }
        ));
    }

    #[test]
    fn ctrl_enter_plain_copy() {
        let mut s = make_state();
        fill(&mut s, 2);
        let actions = handle(&mut s, PickerCommand::PlainCopy);
        assert!(matches!(
            actions[1],
            UiAction::Paste {
                id: 0,
                plain: true
            }
        ));
    }

    #[test]
    fn quick_paste_selects_and_pastes() {
        let mut s = make_state();
        fill(&mut s, 9);
        let actions = handle(&mut s, PickerCommand::QuickPaste(4));
        assert_eq!(s.selected, Some(4));
        assert!(matches!(
            actions[1],
            UiAction::Paste { id: 4, plain: false }
        ));
        // 越界数字不 panic
        let actions = handle(&mut s, PickerCommand::QuickPaste(99));
        assert!(actions.is_empty());
    }

    #[test]
    fn delete_non_pinned_directly_and_pinned_asks_confirmation() {
        let mut s = make_state();
        fill(&mut s, 2);
        let actions = handle(&mut s, PickerCommand::Delete);
        assert!(matches!(actions[0], UiAction::Delete { .. }));
        // 收藏项 → 打开确认框，不直接删
        let mut s = make_state();
        fill(&mut s, 2);
        s.items[0].pinned = true;
        let actions = handle(&mut s, PickerCommand::Delete);
        assert!(actions.is_empty());
        assert!(s.confirm.is_some());
    }

    #[test]
    fn toggle_pin_flips_and_emits() {
        let mut s = make_state();
        fill(&mut s, 2);
        let actions = handle(&mut s, PickerCommand::TogglePin);
        assert!(s.items[0].pinned);
        assert!(matches!(
            actions[0],
            UiAction::Pin { pinned: true, .. }
        ));
    }
}

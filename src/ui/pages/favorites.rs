//! 管理台 · 收藏页：只显示收藏项（复用历史页表格与详情面板）。
//!
//! 收藏项永远不会被 retention 自动清理（spec §10.4）；文件收藏显示
//! 「已保存副本 / 仅路径」状态，在详情面板中可见。

use crate::ui::thumbs::ThumbCache;
use crate::ui::{UiAction, UiState};
use eframe::egui::{self, Rect};

/// 收藏页入口。
pub(crate) fn show(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    thumbs: &mut ThumbCache,
    actions: &mut Vec<UiAction>,
) {
    super::history::page(state, ui, rect, thumbs, actions, true);
}

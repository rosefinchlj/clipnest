//! UI 域单元测试（从 `ui.rs` 拆出，保持主文件 ≤800 行）。
//!
//! 覆盖：搜索防抖/过期 gen 丢弃、翻页追加、收藏/删除乐观更新、设置保存动作、
//! Win+V/热键状态展示、Toast 生命周期等。

use super::*;
use crate::model::{ClipItem, ClipKind, SearchPage};

fn state() -> UiState {
    UiState::new(&egui::Context::default(), &Settings::default())
}

fn page(gen: u64, n: usize) -> SearchPage {
    SearchPage {
        gen,
        total: n,
        items: (0..n)
            .map(|i| ClipItem {
                id: i as i64 + 1,
                kind: ClipKind::Text,
                text: Some(format!("条目{i}")),
                ..Default::default()
            })
            .collect(),
        elapsed_ms: 0,
    }
}

/// 测试辅助：模拟已受理翻页请求。
impl UiState {
    fn requested_more_for_test(&mut self) {
        self.append_requested_len = self.items.len();
    }
}

#[test]
fn new_state_has_pending_initial_query() {
    let s = state();
    assert_eq!(s.mode(), WindowMode::Picker);
    assert!(s.pending.is_some());
    assert_eq!(s.effective_filter(), ClipFilter::All);
}

#[test]
fn apply_page_filters_stale_generation() {
    let mut s = state();
    s.active_gen = 5;
    let mut p = page(4, 3);
    p.total = 3;
    s.apply_search_page(p);
    assert!(s.items.is_empty(), "过期 gen 必须丢弃");

    s.apply_search_page(page(5, 3));
    assert_eq!(s.items.len(), 3);
    assert_eq!(s.total, 3);
    assert_eq!(s.selected, Some(0));
}

#[test]
fn append_result_appends_without_duplicates() {
    let mut s = state();
    s.active_gen = 1;
    s.apply_search_page(page(1, 2));
    s.append_gen = Some(2);
    s.requested_more_for_test();
    // 构造追加页（与第一页有重复 id）
    let more = SearchPage {
        gen: 2,
        total: 4,
        items: vec![
            ClipItem {
                id: 2,
                ..Default::default()
            },
            ClipItem {
                id: 3,
                ..Default::default()
            },
        ],
        elapsed_ms: 0,
    };
    s.apply_search_page(more);
    assert_eq!(s.items.len(), 3, "重复 id 不追加");
    assert_eq!(s.total, 4);
    assert!(s.append_gen.is_none());
}

#[test]
fn toast_lifecycle() {
    let mut s = state();
    assert!(s.toast.is_none());
    s.toast("已复制");
    assert!(s.toast.is_some());
    // 手动过期
    if let Some(t) = &mut s.toast {
        t.until = Instant::now() - Duration::from_millis(1);
    }
    let mut actions = Vec::new();
    s.begin_frame(&egui::Context::default(), &mut actions);
    assert!(s.toast.is_none());
}

#[test]
fn search_debounce_eventually_dispatches_once() {
    let mut s = state();
    let ctx = egui::Context::default();
    s.pending = None;
    s.search_text = "中文".to_string();
    s.schedule_search(false);
    let mut actions = Vec::new();
    // 未到期不发
    s.begin_frame(&ctx, &mut actions);
    assert!(actions
        .iter()
        .all(|a| !matches!(a, UiAction::Search { .. })));
    // 手动把到期时间提前
    if let Some(p) = &mut s.pending {
        p.due = Instant::now() - Duration::from_millis(1);
    }
    s.begin_frame(&ctx, &mut actions);
    let searches: Vec<_> = actions
        .iter()
        .filter(|a| matches!(a, UiAction::Search { .. }))
        .collect();
    assert_eq!(searches.len(), 1);
    // 发出后 pending 清空，不会重复发
    actions.clear();
    s.begin_frame(&ctx, &mut actions);
    assert!(actions.is_empty());
}

#[test]
fn enter_picker_resets_search_and_filter() {
    let mut s = state();
    s.mode = WindowMode::Manager;
    s.search_text = "旧搜索".into();
    s.picker_filter = ClipFilter::Favorites;
    s.enter_picker();
    assert_eq!(s.mode(), WindowMode::Picker);
    assert!(s.search_text.is_empty());
    assert_eq!(s.picker_filter, ClipFilter::All);
    assert!(s.search_focus);
    assert!(s.pending.is_some());
}

#[test]
fn toggle_favorite_and_delete_are_optimistic() {
    let mut s = state();
    s.active_gen = 1;
    s.apply_search_page(page(1, 2));
    let mut actions = Vec::new();
    s.toggle_favorite(0, &mut actions);
    assert!(s.items[0].favorited);
    assert!(!s.items[0].pinned, "收藏不应自动置顶");
    assert!(actions.iter().any(|a| matches!(
        a,
        UiAction::Favorite {
            favorited: true,
            ..
        }
    )));
    s.toggle_pinned(0, &mut actions);
    assert!(s.items[0].pinned, "收藏后可以显式置顶");
    assert!(actions
        .iter()
        .any(|a| matches!(a, UiAction::Pin { pinned: true, .. })));
    s.delete_ids(vec![1], &mut actions);
    assert_eq!(s.items.len(), 1);
    assert_eq!(s.total, 1);
}

#[test]
fn favorites_page_forces_favorites_filter() {
    let mut s = state();
    s.mode = WindowMode::Manager;
    s.manager_page = ManagerPage::Favorites;
    s.manager_filter = ClipFilter::Text;
    assert_eq!(s.effective_filter(), ClipFilter::Favorites);
    s.manager_page = ManagerPage::History;
    assert_eq!(s.effective_filter(), ClipFilter::Text);
}

#[test]
fn request_manager_emits_actions_and_switches_mode() {
    let mut s = state();
    let mut actions = Vec::new();
    s.request_manager(ManagerPage::Settings, &mut actions);
    assert_eq!(s.mode(), WindowMode::Manager);
    assert_eq!(s.manager_page, ManagerPage::Settings);
    assert!(matches!(actions[0], UiAction::OpenManager));
    assert!(matches!(
        actions[1],
        UiAction::SelectManagerPage(ManagerPage::Settings)
    ));
}

#[test]
fn request_more_respects_total_and_loading() {
    let mut s = state();
    s.active_gen = 1;
    s.pending = None; // 初始查询已发出（真实环境/预览都会走到这个状态）
    s.apply_search_page(page(1, 5));
    s.total = 500;
    let mut actions = Vec::new();
    s.request_more(&mut actions);
    assert_eq!(actions.len(), 1);
    assert!(matches!(&actions[0], UiAction::Search { offset: 5, .. }));
    // 已在翻页中 → 不重复请求
    actions.clear();
    s.request_more(&mut actions);
    assert!(actions.is_empty());
}

#[test]
fn save_settings_draft_emits_set_settings() {
    let mut s = state();
    s.settings_draft.max_items = 42;
    s.settings_draft.hide_on_blur = false;
    let mut actions = Vec::new();
    s.save_settings_draft(&mut actions);
    assert_eq!(s.settings.max_items, 42);
    assert!(!s.paused);
    match actions.as_slice() {
        [UiAction::SetSettings(settings)] => {
            assert_eq!(settings.max_items, 42);
            assert!(!settings.hide_on_blur);
        }
        other => panic!("应只发出一个 SetSettings 动作，实际 {other:?}"),
    }
}

#[test]
fn hotkey_status_failure_toasts() {
    let mut s = state();
    let mut actions = Vec::new();
    s.begin_frame(&egui::Context::default(), &mut actions);
    s.apply_hotkey_status(false, Some("已占用".to_string()));
    assert!(s.toast.is_some());
    assert_eq!(s.hotkey_status, Some((false, Some("已占用".to_string()))));
}

#[test]
fn clear_request_opens_confirm_and_defaults_to_deleting_blobs() {
    let mut s = state();
    s.request_clear();
    assert!(s.confirm.is_some());
    assert!(s.clear_delete_blobs);
}

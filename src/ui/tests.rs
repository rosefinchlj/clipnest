//! UI 域单元测试（从 `ui.rs` 拆出，保持主文件 ≤800 行）。
//!
//! 覆盖：搜索防抖/过期 gen 丢弃、翻页追加、收藏/删除乐观更新、设置保存动作、
//! Win+V/热键状态展示、Toast 生命周期等。

use super::*;
use crate::model::{
    ClipItem, ClipKind, CopyState, FileEntry, MatchMode, SearchPage, SortOrder, TempPause, TimeRange,
};

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
fn enter_manager_selects_first_row_for_list_pages() {
    let mut s = state();
    s.selected = None;
    s.enter_manager(ManagerPage::Favorites);
    assert_eq!(s.selected, Some(0), "收藏页默认选中首行，置顶按钮才能出现");
    s.selected = None;
    s.enter_manager(ManagerPage::Settings);
    assert_eq!(s.selected, None, "设置页不需要行选中");
}

#[test]
fn entering_picker_or_manager_resets_time_range() {
    let mut s = state();
    s.picker_time_range = TimeRange::Last7Days;
    s.picker_custom_day = Some(123);
    s.picker_custom_end = Some(456);
    s.picker_time_open = true;
    s.enter_picker();
    assert_eq!(s.picker_time_range, TimeRange::All);
    assert_eq!(s.picker_custom_day, None);
    assert_eq!(s.picker_custom_end, None);
    assert!(!s.picker_time_open);
    s.manager_time_range = TimeRange::Last30Days;
    s.manager_custom_day = Some(456);
    s.manager_custom_end = Some(789);
    s.manager_time_open = true;
    s.enter_manager(ManagerPage::History);
    assert_eq!(s.manager_time_range, TimeRange::All);
    assert_eq!(s.manager_custom_day, None);
    assert_eq!(s.manager_custom_end, None);
    assert!(!s.manager_time_open);
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
fn appended_page_can_readd_id_after_optimistic_delete() {
    let mut s = state();
    s.active_gen = 1;
    s.apply_search_page(page(1, 3));
    let mut actions = Vec::new();
    s.delete_ids(vec![2], &mut actions);
    assert!(!s.loaded_ids.contains(&2));
    assert_eq!(s.items.len(), 2);

    s.append_gen = Some(2);
    s.apply_search_page(SearchPage {
        gen: 2,
        total: 3,
        items: vec![ClipItem {
            id: 2,
            ..Default::default()
        }],
        elapsed_ms: 0,
    });
    assert_eq!(s.items.len(), 3, "删除后同 id 可重新追加");
    assert!(s.loaded_ids.contains(&2));
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

// ---------------------------------------------------------------------------
// A4：文件副本状态
// ---------------------------------------------------------------------------

#[test]
fn file_copy_state_overlays_pending_and_marks_failures_retryable() {
    let mut s = state();
    let mut item = ClipItem {
        id: 7,
        kind: ClipKind::Files,
        ..Default::default()
    };
    item.files = vec![FileEntry {
        name: "a.txt".to_string(),
        path: "C:\\tmp\\a.txt".to_string(),
        size: 5,
        exists: true,
        stored: false,
        copy_state: None,
    }];
    s.items = vec![item];

    s.apply_file_copy_state(7, CopyState::Pending);
    assert_eq!(s.items[0].files[0].copy_state, Some(CopyState::Pending));
    assert_eq!(s.copy_state_for(&s.items[0]), Some(CopyState::Pending));

    s.apply_file_copy_state(7, CopyState::Failed);
    assert_eq!(s.items[0].files[0].copy_state, Some(CopyState::Failed));
    assert!(
        s.copy_state_for(&s.items[0])
            .expect("应有副本状态")
            .retryable(),
        "保存失败应允许重试"
    );
}

// ---------------------------------------------------------------------------
// A3：临时暂停
// ---------------------------------------------------------------------------

#[test]
fn temp_pause_is_runtime_only_and_restorable() {
    let mut s = state();
    assert!(!s.temp_pause.is_active());
    s.set_temp_pause(TempPause::Timed {
        until_ms: crate::model::now_ms() + 60_000,
    });
    assert!(s.temp_pause.is_active());
    s.set_temp_pause(TempPause::Off);
    assert!(!s.temp_pause.is_active());
}

// ---------------------------------------------------------------------------
// A3：忽略来源应用
// ---------------------------------------------------------------------------

#[test]
fn ignore_source_app_normalizes_and_deduplicates() {
    let mut s = state();
    assert!(s.add_ignore_app(" WeChat.EXE "));
    assert!(
        s.ignore_apps().iter().any(|x| x == "wechat.exe"),
        "应转为小写并去掉空白"
    );
    assert!(
        s.settings_draft.ignore_apps.iter().any(|x| x == "wechat.exe"),
        "同时写入设置草稿，保存栏不会把新增名单覆盖掉"
    );
    assert!(!s.add_ignore_app("wechat.exe"), "重复添加应返回 false");
    assert!(!s.add_ignore_app("   "), "空名称应返回 false");
}

// ---------------------------------------------------------------------------
// A2：清空确认的默认范围
// ---------------------------------------------------------------------------

#[test]
fn clear_confirm_resets_to_unfavorited_scope() {
    let mut s = state();
    s.clear_include_favorites = true;
    s.clear_confirm_text = "清空收藏".to_string();
    s.request_clear();
    assert!(!s.clear_include_favorites, "默认只清空未收藏");
    assert!(s.clear_confirm_text.is_empty(), "确认词应重置");
    assert!(matches!(
        s.confirm.as_ref().map(|c| &c.kind),
        Some(ConfirmKind::Clear)
    ));
}

// ---------------------------------------------------------------------------
// B2/B3：保留搜索 / 来源 / 排序 / 回收站分页
// ---------------------------------------------------------------------------

#[test]
fn keep_search_on_open_preserves_picker_state() {
    let mut s = state();
    s.settings.keep_search_on_open = true;
    s.search_text = "保留".into();
    s.picker_filter = ClipFilter::Image;
    s.picker_time_range = TimeRange::Last7Days;
    s.source_app = Some("chrome.exe".into());
    s.source_apps = vec!["chrome.exe".into()];
    s.enter_picker();
    assert_eq!(s.search_text, "保留");
    assert_eq!(s.picker_filter, ClipFilter::Image);
    assert_eq!(s.picker_time_range, TimeRange::Last7Days);
    assert_eq!(s.source_app.as_deref(), Some("chrome.exe"));

    // 关闭后恢复“打开即重置”的旧行为
    s.settings.keep_search_on_open = false;
    s.enter_picker();
    assert!(s.search_text.is_empty());
    assert_eq!(s.picker_filter, ClipFilter::All);
    assert_eq!(s.picker_time_range, TimeRange::All);
    assert_eq!(s.source_app, None);
}

#[test]
fn keep_search_on_open_preserves_manager_time_range() {
    let mut s = state();
    s.settings.keep_search_on_open = true;
    s.manager_time_range = TimeRange::Last30Days;
    s.manager_custom_day = Some(10);
    s.enter_manager(ManagerPage::Trash);
    assert_eq!(s.manager_time_range, TimeRange::Last30Days);
    assert_eq!(s.manager_custom_day, Some(10));

    s.settings.keep_search_on_open = false;
    s.enter_manager(ManagerPage::History);
    assert_eq!(s.manager_time_range, TimeRange::All);
    assert_eq!(s.manager_custom_day, None);
}

#[test]
fn apply_source_apps_keeps_current_selection() {
    let mut s = state();
    s.source_app = Some("zoom.exe".into());
    s.apply_source_apps(vec!["Code.exe".into(), "chrome.exe".into()]);
    assert_eq!(
        s.source_apps,
        vec![
            "chrome.exe".to_string(),
            "Code.exe".to_string(),
            "zoom.exe".to_string()
        ],
        "当前筛选值不在列表时也要保留，并按应用名排序"
    );
    s.apply_source_apps(vec!["chrome.exe".into(), "zoom.exe".into()]);
    assert_eq!(
        s.source_apps,
        vec!["chrome.exe".to_string(), "zoom.exe".to_string()]
    );
}

#[test]
fn search_action_carries_match_mode_source_and_sort() {
    let mut s = state();
    let ctx = egui::Context::default();
    s.pending = None;
    s.active_gen = s.next_gen;
    s.search_text = "报告 会议".into();
    s.match_mode = MatchMode::All;
    s.source_app = Some("chrome.exe".into());
    s.sort = SortOrder::Largest;
    s.schedule_search(true);
    let mut actions = Vec::new();
    s.begin_frame(&ctx, &mut actions);
    let search = actions
        .iter()
        .find_map(|action| match action {
            UiAction::Search {
                match_mode,
                source_app,
                sort,
                ..
            } => Some((*match_mode, source_app.clone(), *sort)),
            _ => None,
        })
        .expect("应当发出 Search 动作");
    assert_eq!(search.0, MatchMode::All);
    assert_eq!(search.1.as_deref(), Some("chrome.exe"));
    assert_eq!(search.2, SortOrder::Largest);

    // 切换筛选控件会立即重发查询
    assert!(s.set_sort(SortOrder::MostUsed));
    assert_eq!(s.sort, SortOrder::MostUsed);
    assert!(!s.set_sort(SortOrder::MostUsed), "相同值不重复查询");
}

#[test]
fn trash_page_request_more_uses_trash_mode_and_pages() {
    let mut s = state();
    s.mode = WindowMode::Manager;
    s.manager_page = ManagerPage::Trash;
    s.pending = None;
    s.active_gen = 1;
    s.apply_search_page(page(1, 2));
    s.total = 10;
    let mut actions = Vec::new();
    s.request_more(&mut actions);
    match &actions[0] {
        UiAction::Search {
            offset,
            trash,
            sort,
            ..
        } => {
            assert_eq!(*offset, 2, "回收站也要从已加载条数继续翻页");
            assert!(*trash, "回收站搜索必须带 trash=true");
            assert_eq!(*sort, SortOrder::Recent);
        }
        other => panic!("应发出 Search 动作，实际 {other:?}"),
    }
    // 下一轮请求的 offset 跟随追加后的条数（重复 id 已去重，实际 3 条）
    s.append_gen = Some(2);
    s.append_requested_len = 2;
    s.apply_search_page(page(2, 3));
    s.total = 10;
    assert_eq!(s.items.len(), 3);
    actions.clear();
    s.request_more(&mut actions);
    assert!(matches!(&actions[0], UiAction::Search { offset: 3, .. }));
}

//! `--ui-preview` 独立预览模式：不依赖 Store/Sys，用假数据渲染完整 UI，便于并行开发与截图。
//!
//! 用法（Lead 的 `main.rs` 已固定入口）：
//! ```text
//! cargo run -- --ui-preview picker     # 620×560 候选窗（外层 660×620 透明窗口）
//! cargo run -- --ui-preview manager    # 管理台 · 历史
//! cargo run -- --ui-preview favorites  # 管理台 · 收藏
//! cargo run -- --ui-preview settings   # 管理台 · 设置
//! cargo run -- --ui-preview about      # 管理台 · 关于
//! ```
//!
//! 预览会真实运行 [`UiState`] 的渲染与交互（搜索过滤/选中/右键/设置页控件全部可用），
//! 只是动作在本地消化（弹 toast 提示），不写数据库、不写设置、不动真实窗口。

use crate::model::{
    ClipFilter, ClipItem, ClipKind, FileEntry, SearchPage, Settings, Stats, WinVMode, WinVStatus,
};
use crate::ui::thumbs::ThumbCache;
use crate::ui::{theme, ManagerPage, UiAction, UiState, WindowMode};
use eframe::egui::{self, Color32, Rect, Vec2};
use std::cell::RefCell;

thread_local! {
    /// 是否已安装字体/主题（每个 UI 线程一次）。
    static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// 预览状态（跨帧保持，切换 mode 时重建）。
    static PREVIEW: RefCell<Option<PreviewState>> = const { RefCell::new(None) };
    /// 已渲染帧数（截图前等字体/布局稳定）。
    static FRAME_NO: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// 是否已保存本次预览截图。
    static SHOT_DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 安装字体 + 主题（只执行一次）。
///
/// 真实 App 由 Lead 在 `ClipNestApp::new` 里已经调用过 `install_fonts/install_theme`，
/// 这里用「是否已注入命名 TextStyle」探测主题，避免预览模式重复 mmap 字体/重复 set_fonts。
pub fn ensure_installed(ctx: &egui::Context) {
    INSTALLED.with(|flag| {
        if flag.get() {
            return;
        }
        let themed = ctx
            .style()
            .text_styles
            .contains_key(&egui::TextStyle::Name("row_title".into()));
        if !themed {
            crate::ui::install_fonts(ctx);
            crate::ui::install_theme(ctx);
        }
        flag.set(true);
    });
}

/// 渲染指定预览页（`picker` / `manager` / `favorites` / `settings` / `about`）。
pub fn show(ctx: &egui::Context, mode: &str) {
    ensure_installed(ctx);

    PREVIEW.with(|cell| {
        let mut slot = cell.borrow_mut();
        let need_new = slot.as_ref().is_none_or(|p| p.mode != mode);
        if need_new {
            *slot = Some(PreviewState::new(ctx, mode));
        }
        let Some(preview) = slot.as_mut() else {
            return;
        };
        preview.frame(ctx);
    });

    // 预览模式顺手截图（只截本窗口，避免桌面其它窗口干扰；用于视觉自查/交付）
    maybe_save_screenshot(ctx, mode);
}

/// 等布局稳定后请求一次窗口截图并保存到 `target/ui-<mode>.png`。
///
/// 用 eframe 的 `ViewportCommand::Screenshot` 只截本应用窗口，
/// 比桌面截图脚本更干净（透明/置顶窗口叠加时不会串图）。
fn maybe_save_screenshot(ctx: &egui::Context, mode: &str) {
    if SHOT_DONE.with(|done| done.get()) {
        return;
    }
    let frame = FRAME_NO.with(|counter| {
        let next = counter.get() + 1;
        counter.set(next);
        next
    });
    if frame < 6 {
        ctx.request_repaint_after(std::time::Duration::from_millis(80));
        return;
    }
    let mut image = None;
    ctx.input(|input| {
        for event in &input.events {
            if let egui::Event::Screenshot { image: shot, .. } = event {
                image = Some(shot.clone());
            }
        }
    });
    if let Some(image) = image {
        if save_preview_png(mode, &image) {
            SHOT_DONE.with(|done| done.set(true));
            ctx.request_repaint();
        }
        return;
    }
    // 尚未收到结果：请求截图并稍后重试
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    ctx.request_repaint_after(std::time::Duration::from_millis(120));
}

/// 把截图像素写成 PNG；返回是否成功（失败只记日志，不影响预览）。
fn save_preview_png(mode: &str, image: &egui::ColorImage) -> bool {
    let [width, height] = image.size;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b(), pixel.a()]);
    }
    let Some(buffer) = image::RgbaImage::from_raw(width as u32, height as u32, rgba) else {
        return false;
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("ui-{mode}.png"));
    match buffer.save(&path) {
        Ok(()) => {
            crate::logging::log(&format!("UI 预览截图已保存：{}", path.display()));
            eprintln!("UI 预览截图已保存：{}", path.display());
            true
        }
        Err(err) => {
            crate::logging::warn(&format!("UI 预览截图保存失败：{err}"));
            false
        }
    }
}

// ---------------------------------------------------------------------------
// 预览状态
// ---------------------------------------------------------------------------

struct PreviewState {
    mode: String,
    state: UiState,
    thumbs: ThumbCache,
    /// 全量假数据（用于本地模拟搜索/过滤/收藏/删除）。
    all_items: Vec<ClipItem>,
}

impl PreviewState {
    fn new(ctx: &egui::Context, mode: &str) -> Self {
        let all_items = fake_items();
        let mut state = UiState::new(ctx, &Settings::default());
        state.preview_mode(true);
        state.set_settings(&preview_settings());
        state.apply_stats(fake_stats());
        state.apply_winv(WinVStatus::HookActive { compensated: true });
        state.apply_hotkey_status(true, None);

        match mode {
            "manager" => state.enter_manager(ManagerPage::History),
            "favorites" => state.enter_manager(ManagerPage::Favorites),
            "settings" => state.enter_manager(ManagerPage::Settings),
            "about" => state.enter_manager(ManagerPage::About),
            _ => state.enter_picker(),
        }
        // 预览不走真实查询：取消待发查询，直接喂假数据
        state.pending = None;
        state.active_gen = state.next_gen;
        let filter = state.effective_filter();
        let items = filter_items(&all_items, "", filter);
        state.apply_search_page(SearchPage {
            gen: state.active_gen,
            total: fake_stats().total,
            items,
            elapsed_ms: 1,
        });
        state.selected = Some(0);
        state.detail_open = true;

        let mut thumbs = ThumbCache::new(ctx);
        // 为图片条目生成演示缩略图（真实环境由后台线程解码 blob 文件）
        for (index, item) in all_items.iter().enumerate() {
            if item.kind == ClipKind::Image {
                if let Some(blob_id) = item.blob_id.as_deref() {
                    thumbs.insert_image(blob_id, demo_image(index));
                }
            }
        }

        Self {
            mode: mode.to_string(),
            state,
            thumbs,
            all_items,
        }
    }

    fn frame(&mut self, ctx: &egui::Context) {
        let mut actions = Vec::new();
        let Self {
            mode,
            state,
            thumbs,
            ..
        } = self;

        if mode == "picker" {
            // 复刻真实候选窗：桌面底色 + 620×560 圆角面板（外层原生窗口 660×620）
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(Color32::from_rgb(0x0B, 0x0D, 0x11)))
                .show(ctx, |_ui| {});
            let screen = ctx.screen_rect();
            let panel_rect = Rect::from_center_size(screen.center(), Vec2::new(620.0, 560.0));
            let layer = egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("clipnest-preview-picker"),
            );
            let painter = ctx.layer_painter(layer);
            let radius = egui::CornerRadius::same(theme::RADIUS_WINDOW);
            // 近似投影（多层半透明圆角矩形）
            painter.rect_filled(
                panel_rect.translate(Vec2::new(0.0, 6.0)).expand(8.0),
                radius,
                Color32::from_black_alpha(50),
            );
            painter.rect_filled(
                panel_rect.translate(Vec2::new(0.0, 3.0)).expand(3.0),
                radius,
                Color32::from_black_alpha(60),
            );
            painter.rect_filled(panel_rect, radius, theme::window_bg());
            painter.rect_stroke(
                panel_rect,
                radius,
                theme::divider(),
                egui::StrokeKind::Inside,
            );
            let content_rect = panel_rect.shrink(theme::PICKER_PAD);
            let mut child = egui::Ui::new(
                ctx.clone(),
                egui::Id::new("clipnest-preview-picker-ui"),
                egui::UiBuilder::new()
                    .max_rect(content_rect)
                    .layer_id(layer),
            );
            child.set_clip_rect(panel_rect);
            child.set_min_size(content_rect.size());
            actions = state.render_into(&mut child, thumbs);
        } else {
            // 管理台：整窗填充（窗口 1100×720，由 Lead 控制）
            egui::CentralPanel::default()
                .frame(theme::manager_frame())
                .show(ctx, |ui| {
                    actions = state.render_into(ui, thumbs);
                });
        }

        self.handle_actions(actions);
    }

    /// 预览模式下把动作本地消化（弹提示 / 改假数据），不产生真实副作用。
    fn handle_actions(&mut self, actions: Vec<UiAction>) {
        for action in actions {
            match action {
                UiAction::Search {
                    text,
                    filter,
                    offset,
                    gen,
                } => {
                    let items = filter_items(&self.all_items, &text, filter);
                    let total = items.len();
                    let page_items = items.into_iter().skip(offset).take(500).collect();
                    self.state.apply_search_page(SearchPage {
                        gen,
                        total,
                        items: page_items,
                        elapsed_ms: 1,
                    });
                }
                UiAction::RequestPastePayload(_) => {}
                UiAction::Paste { plain, .. } => {
                    self.state
                        .toast(if plain { "（预览）仅复制到剪贴板" } else { "（预览）粘贴选中项" });
                }
                UiAction::Pin { id, pinned } => {
                    for item in &mut self.all_items {
                        if item.id == id {
                            item.pinned = pinned;
                        }
                    }
                    for item in &mut self.state.items {
                        if item.id == id {
                            item.pinned = pinned;
                        }
                    }
                    self.state
                        .toast(if pinned { "（预览）已收藏" } else { "（预览）已取消收藏" });
                }
                UiAction::Delete { ids } => {
                    self.all_items.retain(|item| !ids.contains(&item.id));
                    self.state
                        .toast(format!("（预览）删除 {} 条记录", ids.len()));
                }
                UiAction::Clear { .. } => self.state.toast("（预览）清空历史（未执行）"),
                UiAction::SaveFavoriteFiles(_) => {}
                UiAction::OpenManager => {
                    if self.mode == "picker" {
                        // 预览窗口尺寸固定为候选窗：回到 Picker，仅提示真实行为
                        self.state.mode = WindowMode::Picker;
                        self.state.toast("（预览）打开管理台 → 实际会切换到管理窗口");
                    }
                }
                UiAction::OpenPicker => {
                    self.state.mode = WindowMode::Picker;
                }
                UiAction::HideWindow => self.state.toast("（预览）隐藏窗口 → 实际会隐藏到托盘"),
                UiAction::Quit => self.state.toast("（预览）退出程序（未执行）"),
                UiAction::SetSettings(_) => self.state.toast("（预览）设置已保存（未写磁盘）"),
                UiAction::SetAutostart(on) => self.state.toast(if on {
                    "（预览）已开启开机自启"
                } else {
                    "（预览）已关闭开机自启"
                }),
                UiAction::SetPause(on) => self.state.toast(if on {
                    "（预览）已暂停记录"
                } else {
                    "（预览）已恢复记录"
                }),
                UiAction::ExportJson => self.state.toast("（预览）导出 JSON（未执行）"),
                UiAction::OpenDataDir => self.state.toast("（预览）打开数据目录（未执行）"),
                UiAction::RefreshStats => self.state.toast("（预览）刷新统计"),
                UiAction::SelectManagerPage(_) => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 假数据
// ---------------------------------------------------------------------------

/// 过滤假数据（预览本地模拟 Store 搜索）。
fn filter_items(items: &[ClipItem], text: &str, filter: ClipFilter) -> Vec<ClipItem> {
    let query = text.trim().to_lowercase();
    items
        .iter()
        .filter(|item| match filter {
            ClipFilter::All => true,
            ClipFilter::Text => item.kind == ClipKind::Text,
            ClipFilter::Image => item.kind == ClipKind::Image,
            ClipFilter::Files => item.kind == ClipKind::Files,
            ClipFilter::Pinned => item.pinned,
        })
        .filter(|item| {
            if query.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {}",
                item.title(),
                item.subtitle(),
                item.text.clone().unwrap_or_default()
            )
            .to_lowercase();
            haystack.contains(&query)
        })
        .cloned()
        .collect()
}

fn preview_settings() -> Settings {
    let mut settings = Settings::default();
    settings.ignore_apps = vec!["1password.exe".to_string(), "keepass.exe".to_string()];
    settings.winv_mode = WinVMode::Auto;
    settings
}

fn fake_stats() -> Stats {
    Stats {
        total: 1234,
        text: 901,
        image: 178,
        files: 155,
        pinned: 12,
        image_bytes: 180 * 1024 * 1024,
        favorite_file_bytes: 24 * 1024 * 1024,
        db_bytes: 32 * 1024 * 1024,
    }
}

/// 相对当前时间的毫秒时间戳。
fn ago(minutes: i64) -> i64 {
    crate::model::now_ms() - minutes * 60_000
}

fn text_item(
    id: i64,
    text: &str,
    app: &str,
    minutes_ago: i64,
    pinned: bool,
) -> ClipItem {
    ClipItem {
        id,
        kind: ClipKind::Text,
        preview: text.chars().take(80).collect(),
        text: Some(text.to_string()),
        files: Vec::new(),
        blob_id: None,
        byte_len: text.len() as u64,
        width: None,
        height: None,
        created_at: ago(minutes_ago),
        last_used_at: ago(minutes_ago),
        use_count: 3,
        pinned,
        source_app: Some(app.to_string()),
        source_title: None,
    }
}

fn image_item(
    id: i64,
    blob: &str,
    width: u32,
    height: u32,
    bytes: u64,
    app: &str,
    minutes_ago: i64,
    pinned: bool,
) -> ClipItem {
    ClipItem {
        id,
        kind: ClipKind::Image,
        preview: format!("图片 {width}×{height}"),
        text: None,
        files: Vec::new(),
        blob_id: Some(blob.to_string()),
        byte_len: bytes,
        width: Some(width),
        height: Some(height),
        created_at: ago(minutes_ago),
        last_used_at: ago(minutes_ago),
        use_count: 1,
        pinned,
        source_app: Some(app.to_string()),
        source_title: None,
    }
}

fn files_item(
    id: i64,
    files: &[(&str, &str, u64, bool)],
    app: &str,
    minutes_ago: i64,
) -> ClipItem {
    let entries: Vec<FileEntry> = files
        .iter()
        .map(|(name, path, size, exists)| FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            size: *size,
            exists: *exists,
            stored: false,
        })
        .collect();
    let byte_len = entries.iter().map(|f| f.size).sum();
    ClipItem {
        id,
        kind: ClipKind::Files,
        preview: entries
            .iter()
            .map(|f| f.name.clone())
            .collect::<Vec<_>>()
            .join("、"),
        text: None,
        files: entries,
        blob_id: Some(format!("files-{id}")),
        byte_len,
        width: None,
        height: None,
        created_at: ago(minutes_ago),
        last_used_at: ago(minutes_ago),
        use_count: 1,
        pinned: false,
        source_app: Some(app.to_string()),
        source_title: None,
    }
}

/// 一份覆盖三种类型、不同时间/来源/收藏状态的演示数据。
fn fake_items() -> Vec<ClipItem> {
    vec![
        text_item(
            1,
            "fn main() {\n    println!(\"Hello ClipNest\");\n}",
            "Code.exe",
            1,
            true,
        ),
        text_item(
            2,
            "ClipNest 的设计目标是常驻托盘、低资源占用，并且支持文本、图片、文件列表三类剪贴板内容。\n\
             所有数据保存在本机 %APPDATA%\\ClipNest 下，不联网、不上传。",
            "chrome.exe",
            4,
            false,
        ),
        text_item(
            3,
            "https://doc.rust-lang.org/book/ch01-00-getting-started.html",
            "chrome.exe",
            9,
            false,
        ),
        text_item(
            4,
            r"E:\use\ai\deepseek\Rust\clipnest\src\ui\theme.rs",
            "explorer.exe",
            23,
            false,
        ),
        image_item(
            5,
            "demo-image-clipnest-01",
            1920,
            1080,
            2_310_144,
            "SnippingTool.exe",
            120,
            true,
        ),
        text_item(
            6,
            "【产品群】周会纪要：\n1. Win+V 接管已在本机实测通过；\n2. 下周三出 release 构建；\n3. 截图四张补齐到 README。",
            "WeChat.exe",
            180,
            false,
        ),
        image_item(
            7,
            "demo-image-clipnest-02",
            2560,
            1440,
            4_610_816,
            "explorer.exe",
            300,
            false,
        ),
        files_item(
            8,
            &[(
                "2026-Q3-季度报告.docx",
                r"D:\work\reports\2026-Q3-季度报告.docx",
                1_258_291,
                true,
            )],
            "explorer.exe",
            60 * 20,
        ),
        files_item(
            9,
            &[
                ("设计稿.fig", r"D:\work\design\设计稿.fig", 8_388_608, true),
                ("需求文档.pdf", r"D:\work\设计\需求文档.pdf", 524_288, false),
                ("截图.png", r"D:\work\设计\截图.png", 1_048_576, true),
            ],
            "explorer.exe",
            60 * 26,
        ),
        text_item(
            10,
            "The quick brown fox jumps over the lazy dog. 0123456789.",
            "notepad.exe",
            60 * 40,
            false,
        ),
        image_item(
            11,
            "demo-image-clipnest-03",
            800,
            600,
            318_464,
            "SnippingTool.exe",
            60 * 72,
            true,
        ),
        text_item(
            12,
            "SELECT id, kind, preview, last_used_at\nFROM clips\nWHERE pinned = 0\nORDER BY last_used_at DESC\nLIMIT 200;",
            "DataGrip.exe",
            60 * 96,
            false,
        ),
        files_item(
            13,
            &[
                ("clipnest.exe", r"E:\build\clipnest.exe", 14_680_064, true),
                ("config.toml", r"E:\build\config.toml", 2_048, true),
            ],
            "explorer.exe",
            60 * 120,
        ),
        text_item(
            14,
            "会议时间改到明天下午三点，地点在第一会议室。",
            "WeChat.exe",
            60 * 140,
            false,
        ),
        text_item(
            15,
            "密码管理器的复制内容不会被我记录（Clipboard Viewer Ignore 自动跳过）。",
            "1Password.exe",
            60 * 160,
            false,
        ),
        image_item(
            16,
            "demo-image-clipnest-04",
            3440,
            1440,
            6_291_456,
            "SnippingTool.exe",
            60 * 200,
            false,
        ),
    ]
}

/// 生成一张 96×96 的演示缩略图（模拟截图内容）。
fn demo_image(seed: usize) -> egui::ColorImage {
    let size = 96usize;
    let mut pixels = Vec::with_capacity(size * size);
    let (r0, g0, b0) = match seed % 4 {
        0 => (34u8, 40u8, 76u8),
        1 => (20, 62, 54),
        2 => (76, 38, 52),
        _ => (30, 46, 66),
    };
    for y in 0..size {
        for x in 0..size {
            let t = (x + y) as f32 / (2.0 * size as f32);
            let mut r = r0.saturating_add((t * 70.0) as u8);
            let mut g = g0.saturating_add((t * 60.0) as u8);
            let mut b = b0.saturating_add((t * 80.0) as u8);
            // 模拟窗口标题栏
            if y < size / 6 {
                r = 224;
                g = 232;
                b = 240;
            } else if y > size / 3 && y < size * 3 / 4 && x > size / 6 && x < size * 5 / 6 {
                // 模拟内容卡片
                r = r.saturating_add(28);
                g = g.saturating_add(26);
                b = b.saturating_add(30);
            }
            // 一个强调色圆点
            let cx = size / 2;
            let cy = size / 2;
            let dx = x as i32 - cx as i32;
            let dy = y as i32 - cy as i32;
            if dx * dx + dy * dy < 220 {
                r = 110;
                g = 139;
                b = 255;
            }
            pixels.push(Color32::from_rgb(r, g, b));
        }
    }
    egui::ColorImage::new([size, size], pixels)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_items_cover_all_kinds_and_states() {
        let items = fake_items();
        assert!(items.len() >= 12);
        for kind in [ClipKind::Text, ClipKind::Image, ClipKind::Files] {
            assert!(
                items.iter().any(|i| i.kind == kind),
                "缺少类型 {kind:?} 的假数据"
            );
        }
        assert!(items.iter().any(|i| i.pinned), "缺少收藏假数据");
        assert!(items.iter().any(|i| i.source_app.as_deref() == Some("chrome.exe")));
    }

    #[test]
    fn filter_items_supports_text_and_kind() {
        let items = fake_items();
        assert_eq!(
            filter_items(&items, "", ClipFilter::All).len(),
            items.len()
        );
        let images = filter_items(&items, "", ClipFilter::Image);
        assert!(!images.is_empty());
        assert!(images.iter().all(|i| i.kind == ClipKind::Image));
        let pinned = filter_items(&items, "", ClipFilter::Pinned);
        assert!(pinned.iter().all(|i| i.pinned));
        // 中文子串搜索
        let hits = filter_items(&items, "周会", ClipFilter::All);
        assert!(!hits.is_empty());
        // 无结果
        assert!(filter_items(&items, "不存在的关键词xyz", ClipFilter::All).is_empty());
    }

    #[test]
    fn demo_image_has_expected_size() {
        let image = demo_image(0);
        assert_eq!(image.width(), 96);
        assert_eq!(image.height(), 96);
        assert_eq!(image.pixels.len(), 96 * 96);
    }

    #[test]
    fn stats_and_settings_are_consistent() {
        let stats = fake_stats();
        assert_eq!(stats.text + stats.image + stats.files, stats.total);
        let settings = preview_settings();
        assert_eq!(settings.ignore_apps.len(), 2);
        assert!(crate::model::fmt_size(stats.image_bytes).contains("MB"));
    }
}

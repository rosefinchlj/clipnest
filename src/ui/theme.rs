//! 深色磨砂玻璃主题：色板、字号、圆角、间距与 egui `Style`/`Visuals`。
//!
//! **严格对应设计文档 §7 视觉规范**（色板不可自创）：
//! - 窗口底 `#0E1116` @82%（亚克力上叠层）；面板/侧栏 `#131820` @92%；
//!   卡片/输入框 `#1A212B`；分隔线 `#2A3340`（1px、8% 白叠加）。
//! - 主/次/弱文本 `#E6EDF3` / `#8B98A9` / `#5C6878`；强调 `#6E8BFF`（悬停 `#8AA2FF`）。
//! - 字号：标题 15、列表标题 13.5、正文 13、辅助 11.5、代码 Monospace 12。
//! - 行高 64；搜索框 40；圆角：窗口 12 / 卡片 10 / 输入框 9 / 缩略图 8 / 胶囊全圆。
//!
//! 动效（§7.4）：行悬停/选中 80ms、窗口 120ms、模式切换 150ms；不做花哨动效。

use crate::model::ClipKind;
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
};

// ---------------------------------------------------------------------------
// 色板（spec §7.1，十六进制即设计稿值）
// ---------------------------------------------------------------------------

/// 主文本 `#E6EDF3`。
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xED, 0xF3);
/// 次文本 `#8B98A9`。
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x8B, 0x98, 0xA9);
/// 弱文本/占位 `#5C6878`。
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x5C, 0x68, 0x78);

/// 强调色（选中/链接/按钮）`#6E8BFF`。
pub const ACCENT: Color32 = Color32::from_rgb(0x6E, 0x8B, 0xFF);
/// 强调色悬停 `#8AA2FF`。
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x8A, 0xA2, 0xFF);
/// 危险 `#FF6B6B`。
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);
/// 成功 `#4ADE80`。
pub const SUCCESS: Color32 = Color32::from_rgb(0x4A, 0xDE, 0x80);
/// 已收藏（星标）`#F5C451`。
pub const PINNED: Color32 = Color32::from_rgb(0xF5, 0xC4, 0x51);

/// 类型色：文本 `#6E8BFF`。
pub const KIND_TEXT: Color32 = ACCENT;
/// 类型色：图片 `#4ADE80`。
pub const KIND_IMAGE: Color32 = SUCCESS;
/// 类型色：文件 `#F5A524`。
pub const KIND_FILES: Color32 = Color32::from_rgb(0xF5, 0xA5, 0x24);

/// 卡片/输入框底色 `#1A212B`。
pub const CARD: Color32 = Color32::from_rgb(0x1A, 0x21, 0x2B);
/// 卡片悬停底色（在 CARD 上加 6% 白）。
pub const CARD_HOVER: Color32 = Color32::from_rgb(0x23, 0x2B, 0x37);
/// 卡片按下/激活底色。
pub const CARD_ACTIVE: Color32 = Color32::from_rgb(0x27, 0x30, 0x3E);
/// 输入框聚焦底色。
pub const INPUT_FOCUSED: Color32 = Color32::from_rgb(0x1F, 0x27, 0x33);
/// 分隔线/边框基色 `#2A3340`（实际绘制时用 8% 白叠加，见 [`divider`]）。
pub const BORDER: Color32 = Color32::from_rgb(0x2A, 0x33, 0x40);
/// 面板/侧栏不透明基色 `#131820`。
pub const PANEL_BASE: Color32 = Color32::from_rgb(0x13, 0x18, 0x20);
/// 窗口底不透明基色 `#0E1116`（亚克力不可用时的兜底）。
pub const WINDOW_BASE: Color32 = Color32::from_rgb(0x0E, 0x11, 0x16);
/// 表格行交替色（极轻微）。
pub const TABLE_STRIPE: Color32 = Color32::from_rgb(0x16, 0x1C, 0x25);

/// 窗口底：`#0E1116` @82%（叠在亚克力/磨砂材质上，spec §7.1）。
pub fn window_bg() -> Color32 {
    Color32::from_rgba_unmultiplied(0x0E, 0x11, 0x16, 190) // 190/255 ≈ 75%（Acrylic 上叠深色，保证文字对比度）
}

/// 面板/侧栏：`#131820` @92%。
pub fn panel_bg() -> Color32 {
    Color32::from_rgba_unmultiplied(0x13, 0x18, 0x20, 215) // 215/255 ≈ 84%
}

/// 分隔线/边框：1px、8% 白叠加（spec §7.1）。
pub fn divider() -> Stroke {
    Stroke::new(
        1.0_f32,
        Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 20),
    )
}

/// 选中态背景：强调色 12%（spec §6.2）。
pub fn accent_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(0x6E, 0x8B, 0xFF, 31) // 31/255 ≈ 12%
}

/// 悬停态背景：白色 6%（spec §6.2）。
pub fn hover_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 15) // 15/255 ≈ 6%
}

/// 文本选区背景：强调色 ~24%。
pub fn selection_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(0x6E, 0x8B, 0xFF, 61)
}

/// 强调色半透明底（图标底座等，18%）。
pub fn accent_soft() -> Color32 {
    Color32::from_rgba_unmultiplied(0x6E, 0x8B, 0xFF, 46)
}

/// 类型色半透明底（类型徽标底座，15%）。
pub fn kind_soft(kind: ClipKind) -> Color32 {
    let c = kind_color(kind);
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 38)
}

/// 类型徽标/图标的语义色。
pub fn kind_color(kind: ClipKind) -> Color32 {
    match kind {
        ClipKind::Text => KIND_TEXT,
        ClipKind::Image => KIND_IMAGE,
        ClipKind::Files => KIND_FILES,
    }
}

// ---------------------------------------------------------------------------
// 圆角（spec §7.2）
// ---------------------------------------------------------------------------

/// 窗口圆角 12。
pub const RADIUS_WINDOW: u8 = 12;
/// 卡片圆角 10。
pub const RADIUS_CARD: u8 = 10;
/// 输入框圆角 9。
pub const RADIUS_INPUT: u8 = 9;
/// 缩略图圆角 8。
pub const RADIUS_THUMB: u8 = 8;
/// 列表行高亮圆角（行态，spec 未规定，取 8 与缩略图一致）。
pub const RADIUS_ROW: u8 = 8;
/// 胶囊圆角 999（u8 饱和为全圆角）。
pub const RADIUS_CHIP: u8 = 255;

// ---------------------------------------------------------------------------
// 字号与行高（spec §7.2）
// ---------------------------------------------------------------------------

/// 标题 15。
pub const SIZE_TITLE: f32 = 15.0;
/// 列表标题 13.5。
pub const SIZE_ROW_TITLE: f32 = 13.5;
/// 正文 13。
pub const SIZE_BODY: f32 = 13.0;
/// 辅助 11.5。
pub const SIZE_AUX: f32 = 11.5;
/// 代码/等宽 12。
pub const SIZE_MONO: f32 = 12.0;
/// 列表行高 64（spec §6.2/§7.2）。
pub const ROW_HEIGHT: f32 = 64.0;
/// 搜索框高 40。
pub const SEARCH_HEIGHT: f32 = 40.0;
/// 底部提示条高 36。
pub const HINT_BAR_HEIGHT: f32 = 36.0;
/// 过滤胶囊高 26。
pub const CHIP_HEIGHT: f32 = 26.0;
/// 缩略图边长 40。
pub const THUMB_SIZE: f32 = 40.0;
/// Manager 左侧导航宽 184（spec §6.4）。
pub const NAV_WIDTH: f32 = 184.0;
/// Manager 自定义标题栏高 40。
pub const TITLE_BAR_HEIGHT: f32 = 40.0;
/// Manager 详情面板宽 340。
pub const DETAIL_WIDTH: f32 = 340.0;
/// Manager 表格行高 44。
pub const TABLE_ROW_HEIGHT: f32 = 44.0;

// 间距梯度（8 的倍数体系，命名 GAP_* 避免与 egui 冲突）。
/// 间距 4。
pub const GAP_XS: f32 = 4.0;
/// 间距 8。
pub const GAP_S: f32 = 8.0;
/// 间距 12。
pub const GAP_M: f32 = 12.0;
/// 间距 16。
pub const GAP_L: f32 = 16.0;
/// 间距 24。
pub const GAP_XL: f32 = 24.0;
/// Picker 面板内边距 12。
pub const PICKER_PAD: f32 = 12.0;
/// Manager 内容页内边距 20。
pub const PAGE_PAD: f32 = 20.0;

// ---------------------------------------------------------------------------
// 字体 ID 与富文本辅助
// ---------------------------------------------------------------------------

/// 任意字号的字体 ID。
pub fn font(size: f32, family: FontFamily) -> FontId {
    FontId::new(size, family)
}

/// 列表标题字体（13.5 比例字体）。
pub fn font_row_title() -> FontId {
    FontId::new(SIZE_ROW_TITLE, FontFamily::Proportional)
}

/// 正文字体（13）。
pub fn font_body() -> FontId {
    FontId::new(SIZE_BODY, FontFamily::Proportional)
}

/// 辅助字体（11.5）。
pub fn font_aux() -> FontId {
    FontId::new(SIZE_AUX, FontFamily::Proportional)
}

/// 等宽字体（12）。
pub fn font_mono() -> FontId {
    FontId::new(SIZE_MONO, FontFamily::Monospace)
}

/// 标题富文本（15、主文本色）。
pub fn title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_TITLE).color(TEXT)
}

/// 列表标题富文本（13.5、主文本色）。
pub fn row_title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_ROW_TITLE).color(TEXT)
}

/// 正文富文本（13、主文本色）。
pub fn body(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_BODY).color(TEXT)
}

/// 次文本富文本（13、`#8B98A9`）。
pub fn body_secondary(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_BODY).color(TEXT_SECONDARY)
}

/// 辅助文本富文本（11.5、`#8B98A9`）。
pub fn aux(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_AUX).color(TEXT_SECONDARY)
}

/// 弱文本富文本（11.5、`#5C6878`）。
pub fn weak(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_AUX).color(TEXT_WEAK)
}

/// 等宽富文本（12、次文本色）。
pub fn mono(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .size(SIZE_MONO)
        .family(FontFamily::Monospace)
        .color(TEXT_SECONDARY)
}

// ---------------------------------------------------------------------------
// 常用 Frame / Shadow
// ---------------------------------------------------------------------------

/// 窗口级阴影（预览窗口、弹层）。
pub fn window_shadow() -> Shadow {
    Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(150),
    }
}

/// 弹层/菜单阴影。
pub fn popup_shadow() -> Shadow {
    Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(130),
    }
}

/// Picker 面板外框：窗口底 82%、圆角 12、1px 8% 白描边、内边距 12。
pub fn picker_frame() -> egui::Frame {
    // 全窗背景由 `UiState::show` 统一绘制（包含 SidePanel 区域），这里不再重复填充。
    egui::Frame::NONE
        .fill(Color32::TRANSPARENT)
        .corner_radius(CornerRadius::same(RADIUS_WINDOW))
        .stroke(divider())
        .inner_margin(Margin::same(PICKER_PAD as i8))
}

/// Manager 外框：不透明深色面板、圆角 12、1px 描边。
pub fn manager_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::TRANSPARENT)
        .corner_radius(CornerRadius::same(RADIUS_WINDOW))
        .stroke(divider())
}

/// 卡片外框（设置分区、详情面板等）：`#1A212B`、圆角 10、1px 描边、内边距 16。
pub fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(CARD)
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .stroke(divider())
        .inner_margin(Margin::same(16))
}

/// 轻量卡片（不填充、只有描边），用于嵌套分组。
pub fn outline_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::TRANSPARENT)
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .stroke(Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 16),
        ))
        .inner_margin(Margin::same(12))
}

/// 输入框/搜索框外框。
pub fn input_frame(focused: bool) -> egui::Frame {
    egui::Frame::NONE
        .fill(if focused { INPUT_FOCUSED } else { CARD })
        .corner_radius(CornerRadius::same(RADIUS_INPUT))
        .stroke(if focused {
            Stroke::new(1.0_f32, ACCENT)
        } else {
            divider()
        })
        .inner_margin(Margin::symmetric(10, 0))
}

/// 弹层/右键菜单外框。
pub fn popup_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::from_rgb(0x1C, 0x23, 0x2E))
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .stroke(divider())
        .inner_margin(Margin::same(6))
        .shadow(popup_shadow())
}

/// 按钮底色（普通/悬停/按下），供自绘按钮使用。
pub fn button_fill(hovered: bool, pressed: bool) -> Color32 {
    if pressed {
        CARD_ACTIVE
    } else if hovered {
        CARD_HOVER
    } else {
        CARD
    }
}

// ---------------------------------------------------------------------------
// 安装主题
// ---------------------------------------------------------------------------

/// 安装深色主题（色板 + 圆角 + 字号 + 动效），App 启动时调用一次。
pub fn install_theme(ctx: &egui::Context) {
    // 用户明确只要深色主题（spec §2.2）：强制主题偏好为 Dark，
    // 否则 egui 在系统亮色模式下会切到 Light 的默认 Style，导致 ComboBox/DragValue/
    // TextEdit 等默认控件变亮（自绘控件不受影响，但会显得不协调）。
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut style = (*ctx.style()).clone();

    // ---- Visuals ----
    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.faint_bg_color = TABLE_STRIPE;
    v.extreme_bg_color = Color32::from_rgb(0x10, 0x14, 0x1B);
    v.code_bg_color = CARD;
    v.warn_fg_color = KIND_FILES;
    v.error_fg_color = DANGER;
    v.window_fill = PANEL_BASE;
    v.window_stroke = divider();
    v.window_corner_radius = CornerRadius::same(RADIUS_CARD);
    v.window_shadow = popup_shadow();
    v.menu_corner_radius = CornerRadius::same(RADIUS_CARD);
    v.panel_fill = window_bg();
    v.selection = egui::style::Selection {
        bg_fill: selection_fill(),
        stroke: Stroke::new(1.0_f32, ACCENT),
    };
    v.interact_cursor = None;

    // 控件四态（深色卡片 + 细描边 + 主文本）。
    let base = egui::style::WidgetVisuals {
        bg_fill: CARD,
        weak_bg_fill: CARD,
        bg_stroke: divider(),
        corner_radius: CornerRadius::same(RADIUS_INPUT),
        fg_stroke: Stroke::new(1.0_f32, TEXT),
        expansion: 0.0,
    };
    v.widgets.noninteractive = base;
    v.widgets.inactive = base;
    v.widgets.hovered = egui::style::WidgetVisuals {
        bg_fill: CARD_HOVER,
        weak_bg_fill: CARD_HOVER,
        bg_stroke: Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 28),
        ),
        corner_radius: CornerRadius::same(RADIUS_INPUT),
        fg_stroke: Stroke::new(1.0_f32, TEXT),
        expansion: 0.0,
    };
    v.widgets.active = egui::style::WidgetVisuals {
        bg_fill: CARD_ACTIVE,
        weak_bg_fill: CARD_ACTIVE,
        bg_stroke: Stroke::new(1.0_f32, ACCENT),
        corner_radius: CornerRadius::same(RADIUS_INPUT),
        fg_stroke: Stroke::new(1.0_f32, TEXT),
        expansion: 0.0,
    };
    v.widgets.open = v.widgets.active;

    // 滚动条：宽 6、悬浮、悬停加亮（spec §7.3）。
    style.spacing.scroll.floating = true;
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating_width = 4.0;
    style.spacing.scroll.handle_min_length = 24.0;
    style.spacing.scroll.bar_inner_margin = 2.0;
    style.spacing.scroll.bar_outer_margin = 2.0;
    style.spacing.scroll.foreground_color = true;
    style.spacing.scroll.dormant_background_opacity = 0.0;
    style.spacing.scroll.dormant_handle_opacity = 0.0;
    style.spacing.scroll.active_background_opacity = 0.6;
    style.spacing.scroll.active_handle_opacity = 0.85;
    style.spacing.scroll.interact_background_opacity = 0.4;
    style.spacing.scroll.interact_handle_opacity = 0.7;

    style.visuals = v;

    // ---- Spacing ----
    style.spacing.item_spacing = egui::vec2(GAP_S, GAP_S - 2.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size = egui::vec2(40.0, 26.0);
    style.spacing.menu_margin = Margin::same(6);
    style.spacing.window_margin = Margin::same(8);
    style.spacing.indent = 16.0;
    style.spacing.icon_width = 16.0;
    style.spacing.icon_width_inner = 10.0;
    style.spacing.icon_spacing = 6.0;
    style.spacing.tooltip_width = 420.0;
    style.spacing.combo_width = 168.0;
    style.spacing.slider_width = 180.0;
    style.spacing.text_edit_width = 220.0;

    // ---- 动效（spec §7.4）：120ms 全局，行态 80ms 在控件内单独控制 ----
    style.animation_time = 0.12;

    // ---- 字号 ----
    let mut ts = style.text_styles.clone();
    ts.insert(TextStyle::Heading, font(SIZE_TITLE, FontFamily::Proportional));
    ts.insert(TextStyle::Body, font(SIZE_BODY, FontFamily::Proportional));
    ts.insert(TextStyle::Button, font(SIZE_BODY, FontFamily::Proportional));
    ts.insert(TextStyle::Small, font(SIZE_AUX, FontFamily::Proportional));
    ts.insert(TextStyle::Monospace, font(SIZE_MONO, FontFamily::Monospace));
    ts.insert(
        TextStyle::Name("row_title".into()),
        font(SIZE_ROW_TITLE, FontFamily::Proportional),
    );
    ts.insert(
        TextStyle::Name("aux".into()),
        font(SIZE_AUX, FontFamily::Proportional),
    );
    style.text_styles = ts;

    ctx.set_style(style);
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_matches_spec() {
        assert_eq!(TEXT, Color32::from_rgb(0xE6, 0xED, 0xF3));
        assert_eq!(TEXT_SECONDARY, Color32::from_rgb(0x8B, 0x98, 0xA9));
        assert_eq!(TEXT_WEAK, Color32::from_rgb(0x5C, 0x68, 0x78));
        assert_eq!(ACCENT, Color32::from_rgb(0x6E, 0x8B, 0xFF));
        assert_eq!(ACCENT_HOVER, Color32::from_rgb(0x8A, 0xA2, 0xFF));
        assert_eq!(DANGER, Color32::from_rgb(0xFF, 0x6B, 0x6B));
        assert_eq!(SUCCESS, Color32::from_rgb(0x4A, 0xDE, 0x80));
        assert_eq!(PINNED, Color32::from_rgb(0xF5, 0xC4, 0x51));
        assert_eq!(KIND_TEXT, ACCENT);
        assert_eq!(KIND_IMAGE, SUCCESS);
        assert_eq!(KIND_FILES, Color32::from_rgb(0xF5, 0xA5, 0x24));
        assert_eq!(CARD, Color32::from_rgb(0x1A, 0x21, 0x2B));
        assert_eq!(BORDER, Color32::from_rgb(0x2A, 0x33, 0x40));
        assert_eq!(PANEL_BASE, Color32::from_rgb(0x13, 0x18, 0x20));
        assert_eq!(WINDOW_BASE, Color32::from_rgb(0x0E, 0x11, 0x16));
    }

    #[test]
    fn alpha_layers_match_spec() {
        // 窗口底 82%、面板 92%
        assert_eq!(window_bg().a(), 190);
        assert_eq!(panel_bg().a(), 215);
        // 选中 12%、悬停 6%、分隔线 8% 白
        assert_eq!(accent_fill().a(), 31);
        assert_eq!(hover_fill().a(), 15);
        assert_eq!(divider().width, 1.0);
        assert_eq!(divider().color.a(), 20);
        // 半透明色必须已预乘，否则混合会发亮
        let af = accent_fill();
        assert!(af.r() <= ACCENT.r());
        assert!(af.g() <= ACCENT.g());
        assert!(af.b() <= ACCENT.b());
    }

    #[test]
    fn radii_and_sizes_match_spec() {
        assert_eq!(RADIUS_WINDOW, 12);
        assert_eq!(RADIUS_CARD, 10);
        assert_eq!(RADIUS_INPUT, 9);
        assert_eq!(RADIUS_THUMB, 8);
        assert_eq!(ROW_HEIGHT, 64.0);
        assert_eq!(SEARCH_HEIGHT, 40.0);
        assert_eq!(SIZE_TITLE, 15.0);
        assert_eq!(SIZE_ROW_TITLE, 13.5);
        assert_eq!(SIZE_BODY, 13.0);
        assert_eq!(SIZE_AUX, 11.5);
        assert_eq!(SIZE_MONO, 12.0);
        assert_eq!(NAV_WIDTH, 184.0);
    }

    #[test]
    fn install_theme_sets_styles() {
        let ctx = egui::Context::default();
        install_theme(&ctx);
        let style = ctx.style();
        assert!(style.visuals.dark_mode);
        assert_eq!(style.visuals.override_text_color, Some(TEXT));
        assert_eq!(
            style.text_styles.get(&TextStyle::Body).map(|f| f.size),
            Some(SIZE_BODY)
        );
        assert_eq!(
            style.text_styles.get(&TextStyle::Heading).map(|f| f.size),
            Some(SIZE_TITLE)
        );
        assert_eq!(
            style
                .text_styles
                .get(&TextStyle::Name("row_title".into()))
                .map(|f| f.size),
            Some(SIZE_ROW_TITLE)
        );
        assert_eq!(
            style.text_styles.get(&TextStyle::Monospace).map(|f| f.size),
            Some(SIZE_MONO)
        );
        assert_eq!(style.spacing.scroll.bar_width, 6.0);
        assert_eq!(style.animation_time, 0.12);
        assert_eq!(style.spacing.item_spacing.y, GAP_S - 2.0);
        // 强制深色：即使系统是亮色模式，egui 也必须用我们的深色 Style
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        assert_eq!(
            ctx.style().visuals.override_text_color,
            Some(TEXT),
            "强制 Dark 后当前 Style 仍应是主题色"
        );
    }

    #[test]
    fn kind_colors_map_to_spec() {
        assert_eq!(kind_color(ClipKind::Text), KIND_TEXT);
        assert_eq!(kind_color(ClipKind::Image), KIND_IMAGE);
        assert_eq!(kind_color(ClipKind::Files), KIND_FILES);
        // 徽标底色带 alpha 且已预乘
        for kind in [ClipKind::Text, ClipKind::Image, ClipKind::Files] {
            let c = kind_soft(kind);
            assert_eq!(c.a(), 38);
            assert!(c.r() <= kind_color(kind).r());
        }
    }

    #[test]
    fn frames_use_theme_radii() {
        let picker = picker_frame();
        assert_eq!(picker.corner_radius.nw, RADIUS_WINDOW);
        let card = card_frame();
        assert_eq!(card.corner_radius.nw, RADIUS_CARD);
        let input = input_frame(false);
        assert_eq!(input.corner_radius.nw, RADIUS_INPUT);
        assert_eq!(input.fill, CARD);
        assert_eq!(input_frame(true).fill, INPUT_FOCUSED);
    }
}

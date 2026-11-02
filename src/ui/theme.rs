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
use std::sync::atomic::{AtomicBool, Ordering};
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
};

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// 色板（深色 / 浅色两套；运行时切换）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Palette {
    text: Color32,
    text_secondary: Color32,
    text_weak: Color32,
    accent: Color32,
    accent_hover: Color32,
    danger: Color32,
    success: Color32,
    pinned: Color32,
    kind_text: Color32,
    kind_image: Color32,
    kind_files: Color32,
    card: Color32,
    card_hover: Color32,
    card_active: Color32,
    input_focused: Color32,
    border: Color32,
    panel_base: Color32,
    window_base: Color32,
    table_stripe: Color32,
    window_bg: Color32,
    panel_bg: Color32,
    divider: Color32,
    hover_fill: Color32,
    popup_fill: Color32,
    extreme_bg: Color32,
    outline_stroke: Color32,
    hover_stroke: Color32,
    shadow_window: u8,
    shadow_popup: u8,
}

const DARK: Palette = Palette {
    text: Color32::from_rgb(0xE6, 0xED, 0xF3),
    text_secondary: Color32::from_rgb(0x8B, 0x98, 0xA9),
    text_weak: Color32::from_rgb(0x5C, 0x68, 0x78),
    accent: Color32::from_rgb(0x6E, 0x8B, 0xFF),
    accent_hover: Color32::from_rgb(0x8A, 0xA2, 0xFF),
    danger: Color32::from_rgb(0xFF, 0x6B, 0x6B),
    success: Color32::from_rgb(0x4A, 0xDE, 0x80),
    pinned: Color32::from_rgb(0xF5, 0xC4, 0x51),
    kind_text: Color32::from_rgb(0x6E, 0x8B, 0xFF),
    kind_image: Color32::from_rgb(0x4A, 0xDE, 0x80),
    kind_files: Color32::from_rgb(0xF5, 0xA5, 0x24),
    card: Color32::from_rgb(0x1A, 0x21, 0x2B),
    card_hover: Color32::from_rgb(0x23, 0x2B, 0x37),
    card_active: Color32::from_rgb(0x27, 0x30, 0x3E),
    input_focused: Color32::from_rgb(0x1F, 0x27, 0x33),
    border: Color32::from_rgb(0x2A, 0x33, 0x40),
    panel_base: Color32::from_rgb(0x13, 0x18, 0x20),
    window_base: Color32::from_rgb(0x0E, 0x11, 0x16),
    table_stripe: Color32::from_rgb(0x16, 0x1C, 0x25),
    window_bg: Color32::from_rgba_premultiplied(10, 13, 16, 190),
    panel_bg: Color32::from_rgba_premultiplied(16, 20, 27, 215),
    divider: Color32::from_rgba_premultiplied(20, 20, 20, 20),
    hover_fill: Color32::from_rgba_premultiplied(15, 15, 15, 15),
    popup_fill: Color32::from_rgb(0x1C, 0x23, 0x2E),
    extreme_bg: Color32::from_rgb(0x10, 0x14, 0x1B),
    outline_stroke: Color32::from_rgba_premultiplied(16, 16, 16, 16),
    hover_stroke: Color32::from_rgba_premultiplied(28, 28, 28, 28),
    shadow_window: 150,
    shadow_popup: 130,
};

const LIGHT: Palette = Palette {
    text: Color32::from_rgb(0x1B, 0x24, 0x30),
    text_secondary: Color32::from_rgb(0x4A, 0x55, 0x68),
    text_weak: Color32::from_rgb(0x74, 0x80, 0x8F),
    accent: Color32::from_rgb(0x3E, 0x63, 0xDD),
    accent_hover: Color32::from_rgb(0x5B, 0x7C, 0xFA),
    danger: Color32::from_rgb(0xD6, 0x45, 0x45),
    success: Color32::from_rgb(0x2F, 0x9E, 0x5F),
    pinned: Color32::from_rgb(0xB7, 0x86, 0x0B),
    kind_text: Color32::from_rgb(0x3E, 0x63, 0xDD),
    kind_image: Color32::from_rgb(0x2F, 0x9E, 0x5F),
    kind_files: Color32::from_rgb(0xC7, 0x7A, 0x00),
    card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    card_hover: Color32::from_rgb(0xF2, 0xF5, 0xFA),
    card_active: Color32::from_rgb(0xE8, 0xEE, 0xF8),
    input_focused: Color32::from_rgb(0xF4, 0xF7, 0xFC),
    border: Color32::from_rgb(0xC9, 0xD3, 0xE0),
    panel_base: Color32::from_rgb(0xF7, 0xF9, 0xFC),
    window_base: Color32::from_rgb(0xF2, 0xF5, 0xFA),
    table_stripe: Color32::from_rgb(0xF3, 0xF6, 0xFA),
    window_bg: Color32::from_rgba_premultiplied(185, 186, 188, 190),
    panel_bg: Color32::from_rgba_premultiplied(215, 215, 215, 215),
    divider: Color32::from_rgba_premultiplied(0, 0, 0, 24),
    hover_fill: Color32::from_rgba_premultiplied(0, 0, 0, 12),
    popup_fill: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    extreme_bg: Color32::from_rgb(0xEE, 0xF2, 0xF8),
    outline_stroke: Color32::from_rgba_premultiplied(0, 0, 0, 20),
    hover_stroke: Color32::from_rgba_premultiplied(0, 0, 0, 36),
    shadow_window: 50,
    shadow_popup: 40,
};

static LIGHT_MODE: AtomicBool = AtomicBool::new(false);

/// 切换深/浅色（由设置驱动）；切换后需要重新调用 [`install_theme`]。
pub fn set_light(light: bool) {
    LIGHT_MODE.store(light, Ordering::Relaxed);
}

pub fn is_light() -> bool {
    LIGHT_MODE.load(Ordering::Relaxed)
}

fn palette() -> Palette {
    if is_light() {
        LIGHT
    } else {
        DARK
    }
}

pub fn text() -> Color32 {
    palette().text
}
pub fn text_secondary() -> Color32 {
    palette().text_secondary
}
pub fn text_weak() -> Color32 {
    palette().text_weak
}
pub fn accent() -> Color32 {
    palette().accent
}
pub fn accent_hover() -> Color32 {
    palette().accent_hover
}
pub fn danger() -> Color32 {
    palette().danger
}
pub fn success() -> Color32 {
    palette().success
}
pub fn pinned() -> Color32 {
    palette().pinned
}
pub fn card() -> Color32 {
    palette().card
}
pub fn card_hover() -> Color32 {
    palette().card_hover
}
pub fn card_active() -> Color32 {
    palette().card_active
}
pub fn input_focused() -> Color32 {
    palette().input_focused
}
pub fn border() -> Color32 {
    palette().border
}
pub fn panel_base() -> Color32 {
    palette().panel_base
}
pub fn window_base() -> Color32 {
    palette().window_base
}
pub fn table_stripe() -> Color32 {
    palette().table_stripe
}

pub fn window_bg() -> Color32 {
    palette().window_bg
}

pub fn panel_bg() -> Color32 {
    palette().panel_bg
}

pub fn divider() -> Stroke {
    Stroke::new(1.0_f32, palette().divider)
}

pub fn accent_fill() -> Color32 {
    let c = palette().accent;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 31)
}

pub fn hover_fill() -> Color32 {
    palette().hover_fill
}

pub fn selection_fill() -> Color32 {
    let c = palette().accent;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 61)
}

pub fn accent_soft() -> Color32 {
    let c = palette().accent;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 46)
}

pub fn kind_soft(kind: ClipKind) -> Color32 {
    let c = kind_color(kind);
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 38)
}

pub fn kind_color(kind: ClipKind) -> Color32 {
    match kind {
        ClipKind::Text => palette().kind_text,
        ClipKind::Image => palette().kind_image,
        ClipKind::Files => palette().kind_files,
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
pub fn title(value: impl Into<String>) -> egui::RichText {
    egui::RichText::new(value).size(SIZE_TITLE).color(text())
}

/// 列表标题富文本（13.5、主文本色）。
pub fn row_title(value: impl Into<String>) -> egui::RichText {
    egui::RichText::new(value).size(SIZE_ROW_TITLE).color(text())
}

/// 正文富文本（13、主文本色）。
pub fn body(value: impl Into<String>) -> egui::RichText {
    egui::RichText::new(value).size(SIZE_BODY).color(text())
}

/// 次文本富文本（13、`#8B98A9`）。
pub fn body_secondary(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_BODY).color(text_secondary())
}

/// 辅助文本富文本（11.5、`#8B98A9`）。
pub fn aux(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_AUX).color(text_secondary())
}

/// 弱文本富文本（11.5、`#5C6878`）。
pub fn weak(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(SIZE_AUX).color(text_weak())
}

/// 等宽富文本（12、次文本色）。
pub fn mono(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .size(SIZE_MONO)
        .family(FontFamily::Monospace)
        .color(text_secondary())
}

// ---------------------------------------------------------------------------
// 常用 Frame / Shadow
// ---------------------------------------------------------------------------

pub fn window_shadow() -> Shadow {
    Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(palette().shadow_window),
    }
}

pub fn popup_shadow() -> Shadow {
    Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(palette().shadow_popup),
    }
}

/// Picker 面板外框：窗口底、圆角 12、1px 描边、内边距 12。
pub fn picker_frame() -> egui::Frame {
    // 全窗背景由 `UiState::show` 统一绘制（包含 SidePanel 区域），这里不再重复填充。
    egui::Frame::NONE
        .fill(Color32::TRANSPARENT)
        .corner_radius(CornerRadius::same(RADIUS_WINDOW))
        .stroke(divider())
        .inner_margin(Margin::same(PICKER_PAD as i8))
}

/// Manager 外框：圆角 12、1px 描边。
pub fn manager_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::TRANSPARENT)
        .corner_radius(CornerRadius::same(RADIUS_WINDOW))
        .stroke(divider())
}

/// 卡片外框（设置分区、详情面板等）。
pub fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(card())
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .stroke(divider())
        .inner_margin(Margin::same(16))
}

/// 轻量卡片（不填充、只有描边），用于嵌套分组。
pub fn outline_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::TRANSPARENT)
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .stroke(Stroke::new(1.0_f32, palette().outline_stroke))
        .inner_margin(Margin::same(12))
}

/// 输入框/搜索框外框。
pub fn input_frame(focused: bool) -> egui::Frame {
    egui::Frame::NONE
        .fill(if focused {
            input_focused()
        } else {
            card()
        })
        .corner_radius(CornerRadius::same(RADIUS_INPUT))
        .stroke(if focused {
            Stroke::new(1.0_f32, accent())
        } else {
            divider()
        })
        .inner_margin(Margin::symmetric(10, 0))
}

/// 弹层/右键菜单外框。
pub fn popup_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(palette().popup_fill)
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .stroke(divider())
        .inner_margin(Margin::same(6))
        .shadow(popup_shadow())
}

/// 按钮底色（普通/悬停/按下），供自绘按钮使用。
pub fn button_fill(hovered: bool, pressed: bool) -> Color32 {
    if pressed {
        card_active()
    } else if hovered {
        card_hover()
    } else {
        card()
    }
}

// ---------------------------------------------------------------------------
// 安装主题
// ---------------------------------------------------------------------------

/// 安装当前主题（深/浅色由 [`set_light`] 决定），App 启动/切换时调用。
pub fn install_theme(ctx: &egui::Context) {
    let light = is_light();
    ctx.set_theme(if light {
        egui::ThemePreference::Light
    } else {
        egui::ThemePreference::Dark
    });
    let mut style = (*ctx.style()).clone();

    // ---- Visuals ----
    let mut v = if light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    v.dark_mode = !light;
    v.override_text_color = Some(text());
    v.hyperlink_color = accent();
    v.faint_bg_color = table_stripe();
    v.extreme_bg_color = palette().extreme_bg;
    v.code_bg_color = card();
    v.warn_fg_color = palette().kind_files;
    v.error_fg_color = danger();
    v.window_fill = panel_base();
    v.window_stroke = divider();
    v.window_corner_radius = CornerRadius::same(RADIUS_CARD);
    v.window_shadow = popup_shadow();
    v.menu_corner_radius = CornerRadius::same(RADIUS_CARD);
    v.panel_fill = window_bg();
    v.selection = egui::style::Selection {
        bg_fill: selection_fill(),
        stroke: Stroke::new(1.0_f32, accent()),
    };
    v.interact_cursor = None;

    // 控件四态（卡片 + 细描边 + 主文本）。
    let base = egui::style::WidgetVisuals {
        bg_fill: card(),
        weak_bg_fill: card(),
        bg_stroke: divider(),
        corner_radius: CornerRadius::same(RADIUS_INPUT),
        fg_stroke: Stroke::new(1.0_f32, text()),
        expansion: 0.0,
    };
    v.widgets.noninteractive = base;
    v.widgets.inactive = base;
    v.widgets.hovered = egui::style::WidgetVisuals {
        bg_fill: card_hover(),
        weak_bg_fill: card_hover(),
        bg_stroke: Stroke::new(1.0_f32, palette().hover_stroke),
        corner_radius: CornerRadius::same(RADIUS_INPUT),
        fg_stroke: Stroke::new(1.0_f32, text()),
        expansion: 0.0,
    };
    v.widgets.active = egui::style::WidgetVisuals {
        bg_fill: card_active(),
        weak_bg_fill: card_active(),
        bg_stroke: Stroke::new(1.0_f32, accent()),
        corner_radius: CornerRadius::same(RADIUS_INPUT),
        fg_stroke: Stroke::new(1.0_f32, text()),
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
    fn dark_palette_matches_spec() {
        set_light(false);
        assert_eq!(text(), Color32::from_rgb(0xE6, 0xED, 0xF3));
        assert_eq!(text_secondary(), Color32::from_rgb(0x8B, 0x98, 0xA9));
        assert_eq!(text_weak(), Color32::from_rgb(0x5C, 0x68, 0x78));
        assert_eq!(accent(), Color32::from_rgb(0x6E, 0x8B, 0xFF));
        assert_eq!(accent_hover(), Color32::from_rgb(0x8A, 0xA2, 0xFF));
        assert_eq!(danger(), Color32::from_rgb(0xFF, 0x6B, 0x6B));
        assert_eq!(success(), Color32::from_rgb(0x4A, 0xDE, 0x80));
        assert_eq!(pinned(), Color32::from_rgb(0xF5, 0xC4, 0x51));
        assert_eq!(card(), Color32::from_rgb(0x1A, 0x21, 0x2B));
        assert_eq!(border(), Color32::from_rgb(0x2A, 0x33, 0x40));
        assert_eq!(panel_base(), Color32::from_rgb(0x13, 0x18, 0x20));
        assert_eq!(window_base(), Color32::from_rgb(0x0E, 0x11, 0x16));
    }

    #[test]
    fn light_palette_has_dark_text_on_light_cards() {
        let palette = LIGHT;
        assert!(palette.text.r() < 64, "浅色皮肤主文本必须是深色");
        assert!(palette.card.r() > 220, "浅色皮肤卡片必须是浅色");
        assert!(palette.window_bg.a() < 255, "浅色皮肤也要保留半透明玻璃层");
        set_light(false);
    }

    #[test]
    fn alpha_layers_match_spec() {
        set_light(false);
        assert_eq!(window_bg().a(), 190);
        assert_eq!(panel_bg().a(), 215);
        assert_eq!(accent_fill().a(), 31);
        assert_eq!(hover_fill().a(), 15);
        assert_eq!(divider().width, 1.0);
        assert_eq!(divider().color.a(), 20);
        let af = accent_fill();
        assert!(af.r() <= accent().r());
        assert!(af.g() <= accent().g());
        assert!(af.b() <= accent().b());
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
    fn install_theme_sets_dark_and_light_styles() {
        let ctx = egui::Context::default();
        set_light(false);
        install_theme(&ctx);
        let style = ctx.style();
        assert!(style.visuals.dark_mode);
        assert_eq!(style.visuals.override_text_color, Some(text()));
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        assert_eq!(
            style.text_styles.get(&TextStyle::Body).map(|f| f.size),
            Some(SIZE_BODY)
        );
        assert_eq!(
            style.text_styles.get(&TextStyle::Heading).map(|f| f.size),
            Some(SIZE_TITLE)
        );
        assert_eq!(style.spacing.scroll.bar_width, 6.0);
        assert_eq!(style.animation_time, 0.12);

        // 切到浅色：Visuals 与文本色都要跟着变
        set_light(true);
        install_theme(&ctx);
        assert!(!ctx.style().visuals.dark_mode);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert_eq!(ctx.style().visuals.override_text_color, Some(text()));
        assert!(text().r() < 64);
        set_light(false);
        install_theme(&ctx);
    }

    #[test]
    fn kind_colors_map_to_palette() {
        set_light(false);
        assert_eq!(kind_color(ClipKind::Text), accent());
        assert_eq!(kind_color(ClipKind::Image), success());
        assert_eq!(kind_color(ClipKind::Files), Color32::from_rgb(0xF5, 0xA5, 0x24));
        for kind in [ClipKind::Text, ClipKind::Image, ClipKind::Files] {
            let c = kind_soft(kind);
            assert_eq!(c.a(), 38);
            assert!(c.r() <= kind_color(kind).r());
        }
    }

    #[test]
    fn frames_use_theme_radii() {
        set_light(false);
        let picker = picker_frame();
        assert_eq!(picker.corner_radius.nw, RADIUS_WINDOW);
        let card_shape = card_frame();
        assert_eq!(card_shape.corner_radius.nw, RADIUS_CARD);
        let input = input_frame(false);
        assert_eq!(input.corner_radius.nw, RADIUS_INPUT);
        assert_eq!(input.fill, card());
        assert_eq!(input_frame(true).fill, input_focused());
    }
}

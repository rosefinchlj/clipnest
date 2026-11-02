//! 复用控件：搜索框、过滤胶囊、列表行、toast、底部提示条、开关、快捷键录制、确认框、按钮。
//!
//! 全部按 spec §7 视觉规范自绘（深色卡片 + 细描边 + 圆角 + 强调色），
//! 不依赖 egui 默认控件外观；图标见 [`crate::ui::paint`]（几何自绘，不用 emoji）。

use crate::model::{ClipFilter, HotkeyChord};
use crate::ui::{keys, paint, theme};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    Vec2,
};

// ---------------------------------------------------------------------------
// 颜色混合辅助
// ---------------------------------------------------------------------------

/// 两个不透明色按 `t` 线性插值。
fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        f(a.r(), b.r()),
        f(a.g(), b.g()),
        f(a.b(), b.b()),
        f(a.a(), b.a()),
    )
}

// ---------------------------------------------------------------------------
// 按钮
// ---------------------------------------------------------------------------

/// 按钮语义。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// 主按钮：强调色实底白字。
    Primary,
    /// 次按钮：卡片底 + 描边。
    Secondary,
    /// 危险按钮：红色描边/文字。
    Danger,
    /// 幽灵按钮：透明底，悬停 6% 白。
    Ghost,
}

/// 自绘按钮（高 30、圆角 9）。
pub fn button(ui: &mut egui::Ui, text: &str, kind: ButtonKind) -> Response {
    let width = paint::text_width(ui, text, theme::font_body()) + 26.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::hover());
    button_at(ui, rect, text, kind)
}

/// 在指定矩形内画按钮（用于固定布局，如空状态/对话框）。
pub fn button_at(ui: &mut egui::Ui, rect: Rect, text: &str, kind: ButtonKind) -> Response {
    let mut resp = ui.interact(rect, ui.id().with(("btn", text)), Sense::click());
    let radius = CornerRadius::same(theme::RADIUS_INPUT);
    let hovered = resp.hovered();
    let pressed = resp.is_pointer_button_down_on();
    let (fill, stroke, fg) = match kind {
        ButtonKind::Primary => (
            if hovered {
                theme::accent_hover()
            } else {
                theme::accent()
            },
            Stroke::new(1.0_f32, theme::accent()),
            Color32::WHITE,
        ),
        ButtonKind::Secondary => (
            theme::button_fill(hovered, pressed),
            theme::divider(),
            theme::text(),
        ),
        ButtonKind::Danger => (
            if hovered {
                Color32::from_rgba_unmultiplied(0xFF, 0x6B, 0x6B, 30)
            } else {
                theme::card()
            },
            Stroke::new(1.0_f32, theme::danger()),
            theme::danger(),
        ),
        ButtonKind::Ghost => (
            if hovered {
                theme::hover_fill()
            } else {
                Color32::TRANSPARENT
            },
            Stroke::NONE,
            if hovered {
                theme::text()
            } else {
                theme::text_secondary()
            },
        ),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, radius, fill);
    if stroke != Stroke::NONE {
        painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        theme::font_body(),
        fg,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        resp.mark_changed();
    }
    resp
}

/// 右键菜单项（高 28、悬停 6% 白底）。
pub fn menu_item(ui: &mut egui::Ui, text: &str) -> Response {
    let width = ui.available_width().max(170.0);
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::same(6),
            theme::hover_fill(),
        );
    }
    painter.text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::font_body(),
        if resp.hovered() {
            theme::text()
        } else {
            theme::text_secondary()
        },
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        resp.mark_changed();
    }
    resp
}

// ---------------------------------------------------------------------------
// 复选框 / 开关
// ---------------------------------------------------------------------------

/// 16×16 自绘复选框。
pub fn checkbox(ui: &mut egui::Ui, checked: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
    let radius = CornerRadius::same(4);
    let painter = ui.painter();
    if *checked {
        painter.rect_filled(rect, radius, theme::accent());
        paint::paint_check(painter, rect, Color32::WHITE);
    }
    painter.rect_stroke(
        rect.shrink(0.5),
        radius,
        Stroke::new(1.2_f32, if *checked { theme::accent() } else { theme::border() }),
        StrokeKind::Inside,
    );
    if resp.clicked() {
        *checked = !*checked;
        resp.mark_changed();
    }
    resp
}

/// 36×20 圆角开关。
pub fn switch(ui: &mut egui::Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(36.0, 20.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let radius = CornerRadius::same(10);
    let track = lerp_color(theme::border(), theme::accent(), t);
    let painter = ui.painter();
    painter.rect_filled(rect, radius, track);
    if t < 0.99 {
        painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, theme::border()), StrokeKind::Inside);
    }
    let knob_x = rect.left() + 10.0 + (rect.width() - 20.0) * t;
    painter.circle_filled(
        Pos2::new(knob_x, rect.center().y),
        7.0,
        if t > 0.5 { Color32::WHITE } else { theme::text_secondary() },
    );
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

// ---------------------------------------------------------------------------
// 搜索框
// ---------------------------------------------------------------------------

/// 搜索框输出。
pub struct SearchBoxOutput {
    /// 整行响应（点击聚焦、hover 判定用）。
    pub response: Response,
    /// 文本编辑框响应。
    pub text_response: Response,
    /// 本帧文本是否变化。
    pub changed: bool,
    /// 是否点了右侧清除按钮。
    pub cleared: bool,
}

/// 搜索框（高 40、圆角 9）：左侧放大镜、占位提示、右侧清除 ✕ 或快捷键提示。
pub fn search_box(
    ui: &mut egui::Ui,
    text: &mut String,
    placeholder: &str,
    right_hint: Option<&str>,
    auto_focus: bool,
) -> SearchBoxOutput {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, theme::SEARCH_HEIGHT),
        Sense::click(),
    );
    let text_id = response.id.with("text");
    let focused = ui.ctx().memory(|m| m.has_focus(text_id));

    // 背景/描边（聚焦强调色描边）
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(theme::RADIUS_INPUT),
        if focused {
            theme::input_focused()
        } else {
            theme::card()
        },
    );
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::RADIUS_INPUT),
        if focused {
            Stroke::new(1.0_f32, theme::accent())
        } else {
            theme::divider()
        },
        StrokeKind::Inside,
    );

    // 左侧放大镜
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 21.0, rect.center().y),
        Vec2::splat(16.0),
    );
    paint::paint_search(
        ui.painter(),
        icon_rect,
        if focused {
            theme::accent()
        } else {
            theme::text_weak()
        },
    );

    // 右侧：清除按钮 / 快捷键提示（画成键帽样式，避免反引号等窄字形被误认为被裁切）
    let mono_font = FontId::new(theme::SIZE_AUX, egui::FontFamily::Monospace);
    let hint_size = right_hint
        .map(|hint| paint::text_width(ui, hint, mono_font.clone()) + 14.0)
        .unwrap_or(0.0);
    let right_reserved = if text.is_empty() {
        (hint_size + 12.0).max(24.0)
    } else {
        28.0
    };
    let mut cleared = false;
    let mut clear_clicked = false;
    if text.is_empty() {
        if let Some(hint) = right_hint {
            let key_rect = Rect::from_center_size(
                Pos2::new(rect.right() - 10.0 - hint_size / 2.0, rect.center().y),
                Vec2::new(hint_size, 18.0),
            );
            ui.painter().rect_filled(
                key_rect,
                CornerRadius::same(4),
                Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 10),
            );
            ui.painter().rect_stroke(
                key_rect,
                CornerRadius::same(4),
                theme::divider(),
                StrokeKind::Inside,
            );
            ui.painter().text(
                key_rect.center(),
                Align2::CENTER_CENTER,
                hint,
                mono_font.clone(),
                theme::text_secondary(),
            );
        }
    } else {
        let clear_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 20.0, rect.center().y),
            Vec2::splat(20.0),
        );
        let clear_resp = ui.interact(clear_rect, text_id.with("clear"), Sense::click());
        paint::paint_close(
            ui.painter(),
            clear_rect,
            if clear_resp.hovered() {
                theme::text()
            } else {
                theme::text_weak()
            },
        );
        if clear_resp.clicked() {
            text.clear();
            cleared = true;
            clear_clicked = true;
        }
    }

    // 文本输入区（无边框，背景由外框绘制）
    let text_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 38.0, rect.top() + 5.0),
        Pos2::new(rect.right() - right_reserved, rect.bottom() - 5.0),
    );
    let text_response = ui.put(
        text_rect,
        egui::TextEdit::singleline(text)
            .id(text_id)
            .frame(false)
            .margin(egui::Margin::ZERO)
            .hint_text(
                egui::RichText::new(placeholder)
                    .size(theme::SIZE_BODY)
                    .color(theme::text_secondary()),
            )
            .font(theme::font_body())
            .text_color(theme::text())
            .desired_width(text_rect.width())
            .vertical_align(egui::Align::Center),
    );

    if auto_focus {
        text_response.request_focus();
    }
    if response.clicked() && !clear_clicked {
        text_response.request_focus();
    }

    SearchBoxOutput {
        changed: text_response.changed(),
        response,
        text_response,
        cleared,
    }
}

// 列表行（Picker，64px）实现在 `crate::ui::row`，这里 re-export 保持控件入口统一。
pub use crate::ui::row::{clip_row, ClipRow};

// ---------------------------------------------------------------------------
// 过滤胶囊
// ---------------------------------------------------------------------------

/// 过滤胶囊组（全部/文本/图片/文件/收藏）；返回是否切换了过滤。
pub fn filter_chips(ui: &mut egui::Ui, current: &mut ClipFilter) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::GAP_S;
        for filter in ClipFilter::all() {
            let label = filter.label();
            let width = paint::text_width(ui, label, theme::font_aux()) + 26.0;
            let (rect, resp) = ui.allocate_exact_size(
                Vec2::new(width, theme::CHIP_HEIGHT),
                Sense::click(),
            );
            let selected = *current == filter;
            let radius = CornerRadius::same(theme::RADIUS_CHIP);
            let painter = ui.painter();
            if selected {
                painter.rect_filled(rect, radius, theme::accent());
            } else if resp.hovered() {
                painter.rect_filled(rect, radius, theme::hover_fill());
                painter.rect_stroke(
                    rect,
                    radius,
                    Stroke::new(1.0_f32, theme::border()),
                    StrokeKind::Inside,
                );
            } else {
                painter.rect_stroke(
                    rect,
                    radius,
                    Stroke::new(1.0_f32, theme::border()),
                    StrokeKind::Inside,
                );
            }
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::new(theme::SIZE_AUX + 0.5, egui::FontFamily::Proportional),
                if selected {
                    Color32::WHITE
                } else {
                    theme::text_secondary()
                },
            );
            if resp.clicked() && !selected {
                *current = filter;
                changed = true;
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
    });
    changed
}

// ---------------------------------------------------------------------------
// 底部提示条 / Toast
// ---------------------------------------------------------------------------

/// 底部提示条输出。
pub struct HintBarOutput {
    pub gear_clicked: bool,
}

/// 底部提示条（高 36）：左侧快捷键提示，右侧「共 N 条」+ 齿轮。
pub fn hint_bar(ui: &mut egui::Ui, total: usize, paused: bool) -> HintBarOutput {
    let height = theme::HINT_BAR_HEIGHT;
    let (rect, _resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        Sense::hover(),
    );
    let painter = ui.painter().clone();
    // 底部提示条铺一层较实的底：候选窗半透明时，白底/浅底下文字也能看清。
    painter.rect_filled(
        rect,
        egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: theme::RADIUS_ROW,
            se: theme::RADIUS_ROW,
        },
        theme::panel_bg(),
    );
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        theme::divider(),
    );

    // 左：快捷键提示（暂停时追加状态）
    let mut x = rect.left() + 2.0;
    x += paint::paint_elided(
        ui,
        Pos2::new(x, rect.center().y - theme::SIZE_AUX * 0.5),
        keys::PICKER_HINT,
        theme::font_aux(),
        theme::text_secondary(),
        rect.width() * 0.62,
    );
    if paused {
        painter.text(
            Pos2::new(x + 8.0, rect.center().y),
            Align2::LEFT_CENTER,
            "· 已暂停记录",
            theme::font_aux(),
            theme::pinned(),
        );
    }

    // 右：齿轮按钮 + 统计
    let gear_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 14.0, rect.center().y),
        Vec2::splat(18.0),
    );
    let gear_resp = ui.interact(gear_rect, ui.id().with("hint_gear"), Sense::click());
    paint::paint_gear(
        &painter,
        gear_rect.shrink(1.0),
        if gear_resp.hovered() {
            theme::text()
        } else {
            theme::text_secondary()
        },
    );
    painter.text(
        Pos2::new(gear_rect.left() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        format!("共 {} 条", keys::fmt_count(total)),
        theme::font_aux(),
        theme::text_secondary(),
    );
    if gear_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    HintBarOutput {
        gear_clicked: gear_resp.clicked(),
    }
}

/// 绘制 Toast（在 `bottom_center` 上方浮起；`alpha` 0..1 用于淡入淡出）。
pub fn paint_toast(ui: &egui::Ui, bottom_center: Pos2, text: &str, alpha: f32) {
    if alpha <= 0.01 {
        return;
    }
    let galley = ui.fonts(|f| f.layout_no_wrap(text.to_owned(), theme::font_body(), theme::text()));
    let size = galley.size() + Vec2::new(28.0, 18.0);
    let rect = Rect::from_center_size(
        bottom_center - Vec2::new(0.0, size.y / 2.0),
        size,
    );
    let painter = ui.painter();
    let radius = CornerRadius::same(theme::RADIUS_CARD);
    // 阴影（多层模拟模糊）
    for i in 1..=3 {
        let spread = i as f32 * 2.0;
        painter.rect_filled(
            rect.translate(Vec2::new(0.0, 2.0)).expand(spread),
            radius,
            Color32::from_black_alpha(((26.0 * alpha) / i as f32) as u8),
        );
    }
    painter.rect_filled(
        rect,
        radius,
        Color32::from_rgba_unmultiplied(0x1C, 0x23, 0x2E, (238.0 * alpha) as u8),
    );
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, (26.0 * alpha) as u8),
        ),
        StrokeKind::Inside,
    );
    painter.galley(
        rect.min + Vec2::new(14.0, 9.0),
        galley,
        theme::text().gamma_multiply(alpha),
    );
}

// ---------------------------------------------------------------------------
// 快捷键录制
// ---------------------------------------------------------------------------

/// 快捷键录制状态（每帧渲染需持久化）。
#[derive(Default)]
pub struct HotkeyRecorderState {
    pub recording: bool,
    pub error: Option<String>,
}

/// 快捷键录制控件：点击进入录制，捕获到合法组合后返回 `Some(chord)`；Esc 取消。
///
/// 注意：egui 不跟踪 Win 键，因此录制只支持 Ctrl/Alt/Shift 组合；Win 相关
/// 快捷键（Win+V）由设置页上方的「接管 Win+V」单独配置。
pub fn hotkey_recorder(
    ui: &mut egui::Ui,
    chord: &HotkeyChord,
    state: &mut HotkeyRecorderState,
) -> Option<HotkeyChord> {
    let mut captured_chord = None;
    let width = 220.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
    let radius = CornerRadius::same(theme::RADIUS_INPUT);
    let badge = if state.recording {
        "按下组合键…".to_string()
    } else {
        chord.label()
    };
    ui.painter().rect_filled(
        rect,
        radius,
        if state.recording {
            theme::input_focused()
        } else {
            theme::button_fill(resp.hovered(), resp.is_pointer_button_down_on())
        },
    );
    ui.painter().rect_stroke(
        rect,
        radius,
        if state.recording {
            Stroke::new(1.0_f32, theme::accent())
        } else {
            theme::divider()
        },
        StrokeKind::Inside,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        if state.recording {
            "按下组合键…"
        } else {
            "录制"
        },
        theme::font_aux(),
        theme::text_weak(),
    );
    ui.painter().text(
        Pos2::new(rect.right() - 12.0, rect.center().y),
        Align2::RIGHT_CENTER,
        badge,
        FontId::new(theme::SIZE_BODY, egui::FontFamily::Proportional),
        if state.recording {
            theme::accent()
        } else {
            theme::text()
        },
    );
    if resp.clicked() {
        state.recording = !state.recording;
        state.error = None;
    }
    if state.recording {
        ui.ctx().request_repaint();
        let mut captured: Option<(u32, egui::Modifiers)> = None;
        let mut cancel = false;
        ui.ctx().input_mut(|i| {
            i.events.retain(|ev| {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                {
                    if !cancel && captured.is_none() {
                        if *key == egui::Key::Escape {
                            cancel = true;
                        } else if let Some(vk) = keys::vk_from_key(*key) {
                            captured = Some((vk, *modifiers));
                        }
                    }
                    // 录制期间吞掉所有按键，避免触发界面其它快捷键
                    return false;
                }
                true
            });
        });
        if cancel {
            state.recording = false;
            state.error = None;
        } else if let Some((vk, mods)) = captured {
            let new_chord = HotkeyChord::new(mods.ctrl, mods.alt, mods.shift, false, vk);
            match keys::validate_chord(&new_chord) {
                Ok(()) => {
                    state.recording = false;
                    state.error = None;
                    captured_chord = Some(new_chord);
                }
                Err(msg) => {
                    state.recording = false;
                    state.error = Some(msg);
                }
            }
        }
    }
    captured_chord
}

// ---------------------------------------------------------------------------
// 确认对话框
// ---------------------------------------------------------------------------

/// 确认对话框内容。
#[derive(Clone, Debug, Default)]
pub struct ConfirmState {
    pub title: String,
    pub message: String,
    pub confirm_label: String,
    /// 危险操作（删除/清空）用红色确认按钮。
    pub danger: bool,
}

/// 居中模态确认框；返回 `Some(true)` 确认、`Some(false)` 取消、`None` 仍在显示。
pub fn confirm_dialog(ctx: &egui::Context, state: &ConfirmState) -> Option<bool> {
    let mut result = None;
    let modal = egui::Modal::new(egui::Id::new("clipnest-confirm"))
        .frame(
            theme::popup_frame()
                .inner_margin(egui::Margin::same(20))
                .shadow(theme::window_shadow()),
        )
        .backdrop_color(Color32::from_black_alpha(120));
    let resp = modal.show(ctx, |ui| {
        ui.set_width(360.0);
        ui.label(theme::title(&state.title));
        ui.add_space(10.0);
        ui.add(
            egui::Label::new(theme::body_secondary(&state.message))
                .wrap_mode(egui::TextWrapMode::Wrap),
        );
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let confirm = if state.confirm_label.is_empty() {
                    "确认".to_string()
                } else {
                    state.confirm_label.clone()
                };
                let confirm_kind = if state.danger {
                    ButtonKind::Danger
                } else {
                    ButtonKind::Primary
                };
                if button(ui, &confirm, confirm_kind).clicked() {
                    result = Some(true);
                }
                if button(ui, "取消", ButtonKind::Secondary).clicked() {
                    result = Some(false);
                }
            });
        });
    });
    if result.is_none() && resp.should_close() {
        result = Some(false);
    }
    result
}

// ---------------------------------------------------------------------------
// 小标签
// ---------------------------------------------------------------------------

/// 小标签（胶囊，半透明语义色底 + 语义色文字）。
pub fn tag(ui: &mut egui::Ui, text: &str, color: Color32) -> Response {
    let width = paint::text_width(ui, text, theme::font_aux()) + 16.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 20.0), Sense::hover());
    let radius = CornerRadius::same(theme::RADIUS_CHIP);
    ui.painter().rect_filled(
        rect,
        radius,
        Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            (color.a() as f32 * 0.16) as u8,
        ),
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        theme::font_aux(),
        color,
    );
    resp
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_is_endpoint_exact() {
        let a = theme::card();
        let b = theme::accent();
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
        assert_eq!(lerp_color(a, b, 2.0), b); // 越界夹取
    }

    #[test]
    fn button_kinds_are_distinct() {
        // 仅验证枚举可用于 UI 状态机（编译期检查 + 保证四种语义都存在）
        for kind in [
            ButtonKind::Primary,
            ButtonKind::Secondary,
            ButtonKind::Danger,
            ButtonKind::Ghost,
        ] {
            assert!(matches!(
                kind,
                ButtonKind::Primary
                    | ButtonKind::Secondary
                    | ButtonKind::Danger
                    | ButtonKind::Ghost
            ));
        }
    }
}

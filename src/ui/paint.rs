//! 自绘几何图标与文本排版辅助。
//!
//! 设计约束：**不使用 emoji 当图标**，全部用几何图形绘制（线条/圆/多边形），
//! 这样不依赖字体含不含某个符号，也不受不同系统 emoji 字体影响。
//! 图标统一在给定 `Rect` 内居中绘制，语义色由调用方传入。

use crate::model::ClipKind;
use crate::ui::theme;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2,
};
use std::sync::Arc;

/// 测量单行文本宽度。
pub fn text_width(ui: &egui::Ui, text: impl Into<String>, font: FontId) -> f32 {
    ui.fonts(|f| f.layout_no_wrap(text.into(), font, Color32::WHITE).size().x)
}

/// 查找 `keyword` 在 `text` 中的出现位置（字节区间），用于搜索命中高亮。
///
/// 只做逐字符大小写不敏感匹配（ASCII 大小写折叠），中文与全角字符按原样比较；
/// 返回的区间互不重叠，按出现顺序排列。
pub fn match_ranges(text: &str, keyword: &str) -> Vec<(usize, usize)> {
    let needle = keyword.trim();
    if needle.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() || needle_chars.len() > chars.len() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut i = 0;
    while i + needle_chars.len() <= chars.len() {
        let matched = needle_chars
            .iter()
            .enumerate()
            .all(|(k, want)| chars[i + k].1 == *want || chars[i + k].1.eq_ignore_ascii_case(want));
        if matched {
            let start = chars[i].0;
            let end = chars
                .get(i + needle_chars.len())
                .map(|(byte, _)| *byte)
                .unwrap_or(text.len());
            ranges.push((start, end));
            i += needle_chars.len();
        } else {
            i += 1;
        }
    }
    ranges
}

/// 把文本按命中区间切成普通的/高亮的两类片段，追加到 `job`。
fn append_highlighted(
    job: &mut egui::text::LayoutJob,
    text: &str,
    font: &FontId,
    color: Color32,
    keyword: &str,
    highlight: Color32,
) {
    let ranges = match_ranges(text, keyword);
    if ranges.is_empty() {
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
        return;
    }
    let mut cursor = 0;
    for (start, end) in ranges {
        if start > cursor {
            job.append(
                &text[cursor..start],
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color,
                    ..Default::default()
                },
            );
        }
        job.append(
            &text[start..end],
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: highlight,
                ..Default::default()
            },
        );
        cursor = end;
    }
    if cursor < text.len() {
        job.append(
            &text[cursor..],
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
    }
}

/// 单行省略排版：超宽用 `…` 截断（`max_width <= 0` 时不截断）。
pub fn elided(
    ui: &egui::Ui,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> Arc<egui::Galley> {
    if max_width <= 0.0 {
        return ui.fonts(|f| f.layout_no_wrap(text.into(), font, color));
    }
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    job.append(
        &text.into(),
        0.0,
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    ui.fonts(|f| f.layout_job(job))
}

/// 在 `pos`（左上角）绘制单行省略文本，返回实际绘制宽度。
pub fn paint_elided(
    ui: &egui::Ui,
    pos: Pos2,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> f32 {
    let galley = elided(ui, text, font, color, max_width);
    let size = galley.size();
    ui.painter().galley(pos, galley, color);
    size.x
}

/// 构造带关键字高亮的排版任务（多行 TextEdit 自定义 layouter 用）。
pub fn layout_job_highlighted(
    text: &str,
    font: FontId,
    color: Color32,
    keyword: &str,
    highlight: Color32,
    wrap_width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    if wrap_width > 0.0 {
        job.wrap.max_width = wrap_width;
    }
    append_highlighted(&mut job, text, &font, color, keyword, highlight);
    job
}

/// 单行省略排版，并把命中的搜索关键字标成高亮色（失败时退化为普通排版）。
pub fn elided_highlighted(
    ui: &egui::Ui,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
    keyword: &str,
    highlight: Color32,
    max_width: f32,
) -> Arc<egui::Galley> {
    let text = text.into();
    let mut job = egui::text::LayoutJob::default();
    if max_width > 0.0 {
        job.wrap.max_width = max_width;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
    }
    append_highlighted(&mut job, &text, &font, color, keyword, highlight);
    ui.fonts(|f| f.layout_job(job))
}

/// 在 `pos`（左上角）绘制带关键字高亮的单行省略文本，返回实际绘制宽度。
pub fn paint_elided_highlighted(
    ui: &egui::Ui,
    pos: Pos2,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
    keyword: &str,
    highlight: Color32,
    max_width: f32,
) -> f32 {
    let galley = elided_highlighted(ui, text, font, color, keyword, highlight, max_width);
    let size = galley.size();
    ui.painter().galley(pos, galley, color);
    size.x
}

// ---------------------------------------------------------------------------
// 几何图标
// ---------------------------------------------------------------------------

/// 放大镜（搜索）。
pub fn paint_search(painter: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.30;
    let center = c - Vec2::splat(r * 0.25);
    painter.circle_stroke(center, r, Stroke::new(1.5_f32, color));
    painter.line_segment(
        [
            center + Vec2::angled(std::f32::consts::FRAC_PI_4) * r,
            center + Vec2::angled(std::f32::consts::FRAC_PI_4) * (r * 2.05),
        ],
        Stroke::new(1.5_f32, color),
    );
}

/// 关闭按钮的方形图标框，避免高 DPI 下 X 被横向压扁成短横线。
fn close_icon_rect(rect: Rect) -> Rect {
    let side = rect.width().min(rect.height());
    Rect::from_center_size(rect.center(), Vec2::splat(side)).shrink(1.5)
}

/// 清除 ✕。
pub fn paint_close(painter: &egui::Painter, rect: Rect, color: Color32) {
    let r = close_icon_rect(rect);
    painter.line_segment(
        [r.left_top(), r.right_bottom()],
        Stroke::new(1.6_f32, color),
    );
    painter.line_segment(
        [r.right_top(), r.left_bottom()],
        Stroke::new(1.6_f32, color),
    );
}

/// 最小化「—」。
pub fn paint_minus(painter: &egui::Painter, rect: Rect, color: Color32) {
    let y = rect.center().y;
    let half = rect.width() * 0.24;
    painter.line_segment(
        [
            Pos2::new(rect.center().x - half, y),
            Pos2::new(rect.center().x + half, y),
        ],
        Stroke::new(1.4_f32, color),
    );
}

/// 尖括号（`>` / `<`），用于「进入/详情」。
pub fn paint_chevron(painter: &egui::Painter, rect: Rect, points_right: bool, color: Color32) {
    let c = rect.center();
    let h = rect.width().min(rect.height()) * 0.28;
    let (a, b, d) = if points_right {
        (
            c + Vec2::new(-h * 0.5, -h),
            c + Vec2::new(h * 0.5, 0.0),
            c + Vec2::new(-h * 0.5, h),
        )
    } else {
        (
            c + Vec2::new(h * 0.5, -h),
            c + Vec2::new(-h * 0.5, 0.0),
            c + Vec2::new(h * 0.5, h),
        )
    };
    let stroke = Stroke::new(1.5_f32, color);
    painter.line_segment([a, b], stroke);
    painter.line_segment([b, d], stroke);
}

/// 五角星（收藏），`filled` 为实心/描边。
pub fn paint_star(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
    filled: bool,
) {
    let mut points = Vec::with_capacity(10);
    for i in 0..10 {
        let r = if i % 2 == 0 { radius } else { radius * 0.44 };
        let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        points.push(center + Vec2::angled(angle) * r);
    }
    if filled {
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    } else {
        let stroke = Stroke::new(1.4_f32, color);
        for i in 0..10 {
            painter.line_segment([points[i], points[(i + 1) % 10]], stroke);
        }
    }
}

/// 图钉（置顶）。
pub fn paint_pin(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32, filled: bool) {
    let stroke = Stroke::new(1.4_f32, color);
    if filled {
        painter.circle_filled(
            center + Vec2::new(0.0, -radius * 0.35),
            radius * 0.42,
            color,
        );
    } else {
        painter.circle_stroke(
            center + Vec2::new(0.0, -radius * 0.35),
            radius * 0.42,
            stroke,
        );
    }
    painter.line_segment(
        [
            center + Vec2::new(0.0, -radius * 0.05),
            center + Vec2::new(0.0, radius * 0.95),
        ],
        stroke,
    );
    painter.line_segment(
        [
            center + Vec2::new(-radius * 0.58, radius * 0.18),
            center + Vec2::new(radius * 0.58, radius * 0.18),
        ],
        stroke,
    );
}

/// 勾选标记（复选框/成功）。
pub fn paint_check(painter: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.30;
    let stroke = Stroke::new(1.7_f32, color);
    painter.line_segment(
        [c + Vec2::new(-s, 0.0), c + Vec2::new(-s * 0.25, s * 0.75)],
        stroke,
    );
    painter.line_segment(
        [
            c + Vec2::new(-s * 0.25, s * 0.75),
            c + Vec2::new(s, -s * 0.7),
        ],
        stroke,
    );
}

/// 齿轮（设置）。
pub fn paint_gear(painter: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.42;
    let stroke = Stroke::new(1.3_f32, color);
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::angled(angle);
        painter.line_segment([c + dir * (r * 0.62), c + dir * r], stroke);
    }
    painter.circle_stroke(c, r * 0.55, stroke);
    painter.circle_filled(c, r * 0.20, color);
}

/// 类型图标：文本 `T` / 图片（相框+山+太阳）/ 文件（文档+折角+文字行）。
pub fn paint_kind_icon(painter: &egui::Painter, rect: Rect, kind: ClipKind, color: Color32) {
    let stroke = Stroke::new(1.5_f32, color);
    match kind {
        ClipKind::Text => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "T",
                FontId::new(rect.height() * 0.62, egui::FontFamily::Proportional),
                color,
            );
        }
        ClipKind::Image => {
            let frame = Rect::from_center_size(rect.center(), Vec2::splat(rect.width() * 0.62));
            painter.rect_stroke(frame, CornerRadius::same(2), stroke, StrokeKind::Inside);
            // 太阳
            painter.circle_filled(
                frame.left_top() + Vec2::splat(frame.width() * 0.26),
                frame.width() * 0.09,
                color,
            );
            // 山
            let base_y = frame.bottom() - frame.height() * 0.14;
            painter.line_segment(
                [
                    Pos2::new(frame.left() + frame.width() * 0.12, base_y),
                    Pos2::new(
                        frame.left() + frame.width() * 0.40,
                        frame.top() + frame.height() * 0.38,
                    ),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(
                        frame.left() + frame.width() * 0.40,
                        frame.top() + frame.height() * 0.38,
                    ),
                    Pos2::new(frame.left() + frame.width() * 0.66, base_y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(frame.left() + frame.width() * 0.52, base_y),
                    Pos2::new(
                        frame.left() + frame.width() * 0.72,
                        frame.top() + frame.height() * 0.52,
                    ),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(
                        frame.left() + frame.width() * 0.72,
                        frame.top() + frame.height() * 0.52,
                    ),
                    Pos2::new(frame.right() - frame.width() * 0.10, base_y),
                ],
                stroke,
            );
        }
        ClipKind::Files => {
            let doc = Rect::from_center_size(
                rect.center(),
                Vec2::new(rect.width() * 0.52, rect.height() * 0.64),
            );
            let fold = doc.width() * 0.30;
            let pts = vec![
                doc.left_top(),
                Pos2::new(doc.right() - fold, doc.top()),
                Pos2::new(doc.right(), doc.top() + fold),
                doc.right_bottom(),
                doc.left_bottom(),
            ];
            painter.add(Shape::closed_line(pts, stroke));
            // 折角
            painter.line_segment(
                [
                    Pos2::new(doc.right() - fold, doc.top()),
                    Pos2::new(doc.right() - fold, doc.top() + fold),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(doc.right() - fold, doc.top() + fold),
                    Pos2::new(doc.right(), doc.top() + fold),
                ],
                stroke,
            );
            // 文字行
            for i in 0..2 {
                let y = doc.top() + doc.height() * (0.52 + i as f32 * 0.18);
                painter.line_segment(
                    [
                        Pos2::new(doc.left() + doc.width() * 0.18, y),
                        Pos2::new(doc.right() - doc.width() * 0.20, y),
                    ],
                    Stroke::new(1.2_f32, color),
                );
            }
        }
    }
}

/// 缩略图占位（浅色山形 + 相框），解码中/失败时使用。
pub fn paint_thumb_placeholder(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(
        rect,
        CornerRadius::same(theme::RADIUS_THUMB),
        theme::card_active(),
    );
    let frame = rect.shrink(rect.width() * 0.22);
    paint_kind_icon(painter, frame, ClipKind::Image, theme::text_weak());
}

/// 空状态大图标（剪贴板 + 折线），弱色。
pub fn paint_clipboard_big(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let board = Rect::from_center_size(center, Vec2::new(size * 0.72, size));
    let radius = CornerRadius::same(6);
    let stroke = Stroke::new(1.6_f32, color);
    painter.rect_stroke(board, radius, stroke, StrokeKind::Inside);
    // 顶部夹子
    let clip = Rect::from_center_size(
        Pos2::new(board.center().x, board.top()),
        Vec2::new(size * 0.34, size * 0.16),
    );
    painter.rect_filled(clip, CornerRadius::same(4), color);
    // 三条内容线
    for i in 0..3 {
        let y = board.top() + board.height() * (0.38 + i as f32 * 0.18);
        painter.line_segment(
            [
                Pos2::new(board.left() + board.width() * 0.18, y),
                Pos2::new(board.right() - board.width() * 0.18, y),
            ],
            Stroke::new(1.4_f32, color),
        );
    }
}

/// 回收站图标：桶身 + 桶盖 + 两条竖线。
pub fn paint_trash(painter: &egui::Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.4_f32, color);
    let body = Rect::from_min_max(
        Pos2::new(
            rect.left() + rect.width() * 0.24,
            rect.top() + rect.height() * 0.30,
        ),
        Pos2::new(
            rect.right() - rect.width() * 0.24,
            rect.bottom() - rect.height() * 0.08,
        ),
    );
    painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
    let lid_y = rect.top() + rect.height() * 0.24;
    painter.line_segment(
        [
            Pos2::new(rect.left() + rect.width() * 0.16, lid_y),
            Pos2::new(rect.right() - rect.width() * 0.16, lid_y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(
                rect.center().x - rect.width() * 0.12,
                lid_y - rect.height() * 0.10,
            ),
            Pos2::new(
                rect.center().x + rect.width() * 0.12,
                lid_y - rect.height() * 0.10,
            ),
        ],
        stroke,
    );
    for dx in [-0.12_f32, 0.12] {
        painter.line_segment(
            [
                Pos2::new(
                    rect.center().x + rect.width() * dx,
                    body.top() + body.height() * 0.22,
                ),
                Pos2::new(
                    rect.center().x + rect.width() * dx,
                    body.bottom() - body.height() * 0.20,
                ),
            ],
            Stroke::new(1.2_f32, color),
        );
    }
}

/// 品牌标记：强调色圆角方块 + 白色剪贴板折角。
pub fn paint_brand_mark(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::same(7), theme::accent());
    let inner = rect.shrink(rect.width() * 0.24);
    let stroke = Stroke::new(1.6_f32, Color32::WHITE);
    painter.rect_stroke(
        Rect::from_min_max(
            Pos2::new(inner.left(), inner.top() + inner.height() * 0.14),
            inner.max,
        ),
        CornerRadius::same(2),
        stroke,
        StrokeKind::Inside,
    );
    let clip = Rect::from_center_size(
        Pos2::new(inner.center().x, inner.top() + inner.height() * 0.02),
        Vec2::new(inner.width() * 0.44, inner.height() * 0.16),
    );
    painter.rect_filled(clip, CornerRadius::same(2), Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_icon_is_square_not_flat() {
        let icon = close_icon_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(20.0, 16.0)));
        assert!((icon.width() - icon.height()).abs() < 0.01);
        assert!(icon.width() >= 10.0, "close icon too small: {:?}", icon);
    }

    #[test]
    fn match_ranges_finds_all_case_insensitive_occurrences() {
        assert_eq!(
            match_ranges("Foo foo FOOD", "foo"),
            vec![(0, 3), (4, 7), (8, 11)]
        );
    }

    #[test]
    fn match_ranges_handles_chinese_and_empty_keyword() {
        assert_eq!(match_ranges("剪贴板 剪贴", "剪贴"), vec![(0, 6), (10, 16)]);
        assert!(match_ranges("剪贴板", "").is_empty());
        assert!(match_ranges("剪贴板", "   ").is_empty());
    }

    #[test]
    fn match_ranges_keeps_utf8_boundaries() {
        // 中 = 3 bytes；返回的区间必须落在字符边界上，供 LayoutJob 安全切片。
        assert_eq!(match_ranges("a中b中", "中"), vec![(1, 4), (5, 8)]);
        let text = "a中b中";
        for (start, end) in match_ranges(text, "中") {
            assert!(text.is_char_boundary(start));
            assert!(text.is_char_boundary(end));
        }
    }
}

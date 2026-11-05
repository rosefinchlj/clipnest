//! Picker 列表行（64px）：类型徽标/缩略图 + 标题/预览 + 相对时间/星标 + hover/选中态。
//!
//! 从 `widgets.rs` 拆出，保持单文件 ≤800 行（spec §6.2 列表项规格）。

use crate::model::{fmt_relative_time, ClipItem, ClipKind};
use crate::ui::{paint, theme};
use eframe::egui::{self, Align2, CornerRadius, FontId, Pos2, Rect, Response, Sense, Vec2};

// ---------------------------------------------------------------------------
// 列表行（Picker，64px）
// ---------------------------------------------------------------------------

/// `clip_row` 的入参（避免长参数列表）。
pub struct ClipRow<'a> {
    pub item: &'a ClipItem,
    /// 图片缩略图（懒加载完成才有）。
    pub thumb: Option<&'a egui::TextureHandle>,
    pub selected: bool,
    /// 当前列表中的序号（从 1 开始，不是数据库 id）。
    pub ordinal: usize,
    /// 是否处于收藏列表：只有收藏列表才用金色标记置顶。
    pub highlight_pinned: bool,
    /// 当前搜索关键字；非空时在标题/预览中把命中文字标红。
    pub keyword: &'a str,
}

/// 列表行输出。
pub struct ClipRowOutput {
    pub response: Response,
    /// 行矩形（滚动到选中行时用）。
    pub rect: Rect,
    /// 本帧是否点了收藏星标。
    pub star_clicked: bool,
    /// 本帧是否点了置顶图标。
    pub pin_clicked: bool,
}

/// 64px 列表行：类型徽标/40×40 缩略图（圆角 8）+ 标题单行 + 预览单行 +
/// 右侧相对时间与星标；选中态强调色 12% + 左侧 2px 竖条，悬停 6%。
pub fn clip_row(ui: &mut egui::Ui, row: ClipRow<'_>) -> ClipRowOutput {
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, theme::ROW_HEIGHT), Sense::click());
    let ctx = ui.ctx().clone();
    // 选中态必须瞬时切换：用 80ms 淡入淡出时，每次上下移动都会同时淡出旧行/淡入新行，
    // 看起来像整块列表闪一下。hover 仍保留轻微动画。
    let sel_t = if row.selected { 1.0 } else { 0.0 };
    let hover_t = ctx.animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.08);

    // 背景 / 左侧竖条：只有收藏列表用金色底标记置顶，其他列表保持普通选中态。
    let painter = ui.painter().clone();
    let pinned_highlight = row.item.pinned && row.highlight_pinned;
    if pinned_highlight {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_ROW),
            if row.selected {
                theme::pinned_fill_strong()
            } else {
                theme::pinned_fill()
            },
        );
    } else if sel_t > 0.001 {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_ROW),
            theme::selection_fill().gamma_multiply(sel_t),
        );
    } else if hover_t > 0.001 {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS_ROW),
            theme::hover_fill().gamma_multiply(hover_t),
        );
    }
    if row.selected {
        let bar = Rect::from_min_max(
            Pos2::new(rect.left() + 1.0, rect.top() + 12.0),
            Pos2::new(rect.left() + 3.0, rect.bottom() - 12.0),
        );
        painter.rect_filled(
            bar,
            CornerRadius::same(1),
            if row.item.pinned {
                theme::pinned()
            } else {
                theme::selection_bar()
            },
        );
    }

    // 左：列表序号 + 40×40 类型徽标 / 缩略图
    // 序号按当前列表顺序从 1 开始，只用于视觉定位，不暴露数据库 id。
    let ordinal_width = 28.0;
    let ordinal_center = Pos2::new(
        rect.left() + theme::GAP_S + ordinal_width / 2.0,
        rect.center().y,
    );
    painter.text(
        ordinal_center,
        Align2::CENTER_CENTER,
        row.ordinal.to_string(),
        theme::font_mono(),
        if row.selected && pinned_highlight {
            theme::text()
        } else if row.selected {
            theme::selection_bar()
        } else {
            theme::text_secondary()
        },
    );
    let icon_rect = Rect::from_center_size(
        Pos2::new(
            rect.left() + theme::GAP_S + ordinal_width + theme::THUMB_SIZE / 2.0,
            rect.center().y,
        ),
        Vec2::splat(theme::THUMB_SIZE),
    );
    if row.item.kind == ClipKind::Image {
        if let Some(tex) = row.thumb {
            painter.rect_filled(
                icon_rect,
                CornerRadius::same(theme::RADIUS_THUMB),
                theme::card_active(),
            );
            let size = icon_rect.size();
            ui.put(
                icon_rect,
                egui::Image::new(egui::load::SizedTexture::new(tex.id(), size))
                    .fit_to_exact_size(size)
                    .corner_radius(CornerRadius::same(theme::RADIUS_THUMB)),
            );
            // `ui.put` 会把父 Ui 的 cursor 推进到图片矩形底部（40px 图标 → 52px），
            // 比整行 64px 矮，导致后续图片行上移、行距不一致。恢复到整行底部。
            ui.advance_cursor_after_rect(rect);
        } else {
            paint::paint_thumb_placeholder(&painter, icon_rect);
        }
    } else {
        painter.rect_filled(
            icon_rect,
            CornerRadius::same(theme::RADIUS_THUMB),
            theme::kind_soft(row.item.kind),
        );
        paint::paint_kind_icon(
            &painter,
            icon_rect.shrink(11.0),
            row.item.kind,
            theme::kind_color(row.item.kind),
        );
    }

    // 中：标题 + 预览（各单行省略）
    let text_left = icon_rect.right() + 10.0;
    let text_right = rect.right() - 116.0;
    let title = row.item.title();
    paint::paint_elided_highlighted(
        ui,
        Pos2::new(text_left, rect.top() + 11.0),
        title,
        theme::font_row_title(),
        theme::text(),
        row.keyword,
        theme::danger(),
        text_right - text_left,
    );
    let subtitle = row.item.subtitle();
    paint::paint_elided_highlighted(
        ui,
        Pos2::new(text_left, rect.top() + 34.0),
        subtitle,
        FontId::new(theme::SIZE_AUX, egui::FontFamily::Proportional),
        if pinned_highlight {
            theme::text()
        } else {
            theme::text_secondary()
        },
        row.keyword,
        theme::danger(),
        text_right - text_left,
    );

    // 右：相对时间（上）+ 星标（下）
    let right_meta = if row.item.use_count > 0 {
        format!(
            "{} · {} 次",
            fmt_relative_time(row.item.last_used_at),
            row.item.use_count
        )
    } else {
        fmt_relative_time(row.item.last_used_at)
    };
    painter.text(
        Pos2::new(rect.right() - 12.0, rect.top() + 12.0),
        Align2::RIGHT_TOP,
        right_meta,
        theme::font_aux(),
        if pinned_highlight {
            theme::text()
        } else {
            theme::text_secondary()
        },
    );
    let star_center = Pos2::new(rect.right() - 20.0, rect.bottom() - 19.0);
    let star_rect = Rect::from_center_size(star_center, Vec2::splat(18.0));
    let star_resp = ui.interact(star_rect, response.id.with("star"), Sense::click());
    let show_star = row.item.favorited || row.selected || star_resp.hovered() || response.hovered();
    if show_star {
        paint::paint_star(
            &painter,
            star_center,
            7.0,
            if row.item.favorited {
                theme::pinned()
            } else if star_resp.hovered() {
                theme::pinned().gamma_multiply(0.8)
            } else {
                theme::text_weak()
            },
            row.item.favorited,
        );
    }

    // 置顶按钮：收藏项才允许置顶，星标左侧显示小图钉。
    let pin_center = Pos2::new(rect.right() - 44.0, rect.bottom() - 19.0);
    let pin_rect = Rect::from_center_size(pin_center, Vec2::splat(18.0));
    let pin_resp = ui.interact(pin_rect, response.id.with("pin"), Sense::click());
    if row.item.favorited {
        paint::paint_pin(
            &painter,
            pin_center,
            6.5,
            if row.item.pinned {
                theme::text()
            } else if pin_resp.hovered() {
                theme::accent()
            } else {
                theme::text_weak()
            },
            row.item.pinned,
        );
    }

    if response.hovered() || star_resp.hovered() || pin_resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // 悬停预览完整内容（截断时才有意义）
    let response = response.on_hover_ui(|ui| {
        tooltip_item(ui, row.item);
    });
    ClipRowOutput {
        response,
        rect,
        star_clicked: star_resp.clicked(),
        pin_clicked: row.item.favorited && pin_resp.clicked(),
    }
}

/// 列表项的 tooltip 内容（标题截断/文件多时展示详情）。
fn tooltip_item(ui: &mut egui::Ui, item: &ClipItem) {
    ui.set_max_width(460.0);
    ui.label(theme::row_title(item.title()));
    ui.add_space(2.0);
    match item.kind {
        ClipKind::Text => {
            let text = item.text.as_deref().unwrap_or(item.preview.as_str());
            let preview: String = text.chars().take(600).collect();
            ui.add(egui::Label::new(theme::mono(preview)).wrap_mode(egui::TextWrapMode::Wrap));
        }
        ClipKind::Image => {
            ui.label(theme::aux(format!(
                "{} · {}×{}",
                crate::model::fmt_size(item.byte_len),
                item.width.unwrap_or(0),
                item.height.unwrap_or(0)
            )));
        }
        ClipKind::Files => {
            for file in item.files.iter().take(6) {
                ui.label(theme::aux(format!(
                    "{} · {}",
                    file.display_name(),
                    crate::model::fmt_size(file.size)
                )));
            }
            if item.files.len() > 6 {
                ui.label(theme::weak(format!("… 共 {} 个文件", item.files.len())));
            }
        }
    }
}

//! 管理台 · 设置页：外观皮肤、快捷键录制、容量、行为、忽略名单、开机自启、数据操作。
//!
//! 所有修改先写入 [`UiState::settings_draft`]，点「保存设置」后通过
//! [`UiAction::SetSettings`] 交给 Lead 落盘；开自启/暂停/导出/清空等即时动作
//! 通过对应 [`UiAction`] 发送。**UI 不直接写设置文件**。

use crate::model::{
    clamp_ui_scale, fmt_size, PasteKey, SelectionPreset, Settings, ThemeMode,
    PICKER_SIZE_PRESETS, UI_SCALE_MAX, UI_SCALE_MIN, UI_SCALE_STEP,
};
use crate::ui::{theme, widgets, UiAction, UiState};
use eframe::egui::{self, Pos2, Rect, UiBuilder, Vec2};

const MIB: f64 = 1024.0 * 1024.0;

/// 底部固定保存栏高度（含卡片内边距）。
const SAVE_BAR_HEIGHT: f32 = 64.0;
/// 内容区与固定保存栏之间的留白。
const SAVE_BAR_GAP: f32 = 12.0;

/// 设置页入口。
pub(crate) fn show(
    state: &mut UiState,
    ui: &mut egui::Ui,
    rect: Rect,
    actions: &mut Vec<UiAction>,
) {
    let inner = rect.shrink(theme::PAGE_PAD);
    let (content, footer) = split_page(inner);

    ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
        egui::ScrollArea::vertical()
            .id_salt("settings_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(theme::title("设置"));
                ui.add_space(4.0);
                ui.label(theme::aux(
                    "焦点在输入框时按 Esc 会先结束编辑；带「立即生效」的开关会即时应用，其余点底部固定栏「保存设置」生效。",
                ));
                ui.add_space(18.0);

                appearance_section(state, ui, actions);
                ui.add_space(16.0);
                hotkey_section(state, ui, actions);
                ui.add_space(16.0);
                capacity_section(state, ui, actions);
                ui.add_space(16.0);
                behavior_section(state, ui, actions);
                ui.add_space(16.0);
                ignore_section(state, ui);
                ui.add_space(16.0);
                data_section(state, ui, actions);
                // 给滚动内容留出呼吸空间，避免最后一张卡片贴着固定栏。
                ui.add_space(8.0);
            });
    });

    save_bar(state, ui, footer, actions);
}

/// 把设置页内容区切成「可滚动内容 + 底部固定保存栏」。
fn split_page(inner: Rect) -> (Rect, Rect) {
    let footer = Rect::from_min_max(
        Pos2::new(inner.left(), inner.bottom() - SAVE_BAR_HEIGHT),
        inner.max,
    );
    let content = Rect::from_min_max(
        inner.min,
        Pos2::new(inner.right(), footer.top() - SAVE_BAR_GAP),
    );
    (content, footer)
}

/// 外观：深色 / 浅色皮肤（保存设置后即时切换）。
fn appearance_section(state: &mut UiState, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
    section(ui, "外观", |ui| {
        setting_row(
            ui,
            "皮肤",
            "深色磨砂玻璃 / 浅色。浅色适合亮色桌面或白天使用，保存后立即切换。",
            |ui| {
                let mut mode = state.settings_draft.theme;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut mode, ThemeMode::Dark, ThemeMode::Dark.label());
                    ui.selectable_value(&mut mode, ThemeMode::Light, ThemeMode::Light.label());
                });
                if mode != state.settings_draft.theme {
                    state.settings_draft.theme = mode;
                }
            },
        );
        setting_row(
            ui,
            "选中配色",
            "候选窗选中行、左侧竖条与过滤胶囊的强调色；默认玉石青绿。保存后立即生效。",
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    // 右侧是 RTL 布局，反向遍历让“玉石青绿 → 靛蓝 → 琥珀”从左到右显示。
                    for preset in SelectionPreset::ALL.into_iter().rev() {
                        ui.colored_label(theme::preset_color(preset), "●");
                        ui.selectable_value(
                            &mut state.settings_draft.selection_preset,
                            preset,
                            preset.label(),
                        );
                    }
                });
            },
        );
        setting_row(
            ui,
            "磨砂透明度",
            "终端式 Acrylic 磨砂：越低越透；深色 tint 有下限，白底/亮色窗口上文字仍可读。只作用于候选窗。",
            |ui| {
                let mut opacity = state.settings_draft.glass_opacity as u32;
                let slider = egui::Slider::new(&mut opacity, 35..=100)
                    .suffix(" %")
                    .fixed_decimals(0);
                if slider_with_rail(ui, slider).changed() {
                    state.settings_draft.glass_opacity = opacity as u8;
                    actions.push(UiAction::PreviewGlassOpacity(opacity as u8));
                }
            },
        );
        setting_row(
            ui,
            "界面缩放",
            "整体 UI 缩放（候选窗与管理台），0.8×–1.5×；保存后立即生效，高分屏可适当放大。",
            |ui| {
                let mut scale = state.settings_draft.ui_scale;
                let slider = egui::Slider::new(&mut scale, UI_SCALE_MIN..=UI_SCALE_MAX)
                    .step_by(UI_SCALE_STEP as f64)
                    .fixed_decimals(2)
                    .suffix("×");
                if slider_with_rail(ui, slider).changed() {
                    state.settings_draft.ui_scale = clamp_ui_scale(scale);
                }
            },
        );
        setting_row(
            ui,
            "候选窗尺寸",
            "候选窗默认大小；也可直接拖动候选窗边缘缩放，尺寸会自动记住。",
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (label, size) in PICKER_SIZE_PRESETS {
                        let selected = state.settings_draft.picker_size == size;
                        if ui.selectable_label(selected, label).clicked() {
                            state.settings_draft.picker_size = size;
                        }
                    }
                });
            },
        );
    });
}

// ---------------------------------------------------------------------------
// 布局辅助
// ---------------------------------------------------------------------------

/// 给滑条一点可见度：默认 `inactive.bg_fill` 与卡片同色，滑轨会“消失”。
fn slider_with_rail(ui: &mut egui::Ui, slider: egui::Slider<'_>) -> egui::Response {
    ui.scope(|ui| {
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.bg_fill = theme::border();
        visuals.widgets.inactive.weak_bg_fill = theme::border();
        visuals.widgets.hovered.bg_fill = theme::accent();
        visuals.widgets.active.bg_fill = theme::accent();
        visuals.selection.bg_fill = theme::accent();
        visuals.slider_trailing_fill = true;
        ui.add(slider)
    })
    .inner
}

/// 一个设置分区卡片（标题 + 内容）。
fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(theme::row_title(title));
        ui.add_space(2.0);
        painter_divider(ui);
        ui.add_space(10.0);
        body(ui);
    });
}

/// 卡片内分隔线。
fn painter_divider(ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    let y = rect.top() + 2.0;
    ui.painter().line_segment(
        [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
        theme::divider(),
    );
    ui.add_space(2.0);
}

/// 左标题/说明 + 右控件的设置行。
fn setting_row(ui: &mut egui::Ui, title: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 260.0).max(160.0);
        ui.allocate_ui_with_layout(
            Vec2::new(w, 20.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.label(theme::body(title));
                if !desc.is_empty() {
                    ui.label(theme::weak(desc));
                }
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            control(ui);
        });
    });
    ui.add_space(12.0);
}

/// MB 数值行（u64 字节 ↔ MB 浮点）。
fn mb_row(
    ui: &mut egui::Ui,
    state: &mut UiState,
    title: &str,
    desc: &str,
    get: impl Fn(&Settings) -> u64,
    set: impl Fn(&mut Settings, u64),
    range: std::ops::RangeInclusive<f64>,
) {
    let mut mb = get(&state.settings_draft) as f64 / MIB;
    let resp = ui
        .horizontal(|ui| {
            let w = (ui.available_width() - 260.0).max(160.0);
            ui.allocate_ui_with_layout(
                Vec2::new(w, 20.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.label(theme::body(title));
                    if !desc.is_empty() {
                        ui.label(theme::weak(desc));
                    }
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::DragValue::new(&mut mb)
                        .speed(10.0)
                        .range(range)
                        .max_decimals(2)
                        .suffix(" MB"),
                )
            })
            .inner
        })
        .inner;
    if resp.changed() {
        set(&mut state.settings_draft, (mb * MIB) as u64);
    }
    ui.add_space(12.0);
}

/// 一行小号等宽路径 + 右侧按钮。
fn path_row(ui: &mut egui::Ui, label: &str, path: &str) {
    ui.horizontal(|ui| {
        ui.label(theme::weak(label));
        ui.label(theme::mono(path));
    });
}

// ---------------------------------------------------------------------------
// 快捷键 / Win+V
// ---------------------------------------------------------------------------

fn hotkey_section(state: &mut UiState, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
    section(ui, "快捷键", |ui| {
        // 备用全局快捷键（录制）
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new((ui.available_width() - 280.0).max(180.0), 20.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.label(theme::body("备用全局快捷键"));
                    ui.label(theme::weak(
                        "点击「录制」设置组合键（Ctrl/Alt/Shift + 键或 F1–F24），保存后立即生效",
                    ));
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut chord = state.settings_draft.hotkey_picker;
                if let Some(new_chord) = widgets::hotkey_recorder(ui, &chord, &mut state.rec) {
                    chord = new_chord;
                    state.settings_draft.hotkey_picker = chord;
                    state.toast(format!("已录制 {}", chord.label()));
                }
                if let Some(err) = &state.rec.error {
                    ui.label(
                        egui::RichText::new(err.clone())
                            .size(theme::SIZE_AUX)
                            .color(theme::danger()),
                    );
                }
            });
        });
        ui.add_space(4.0);
        // 备用快捷键注册状态
        match &state.hotkey_status {
            Some((true, Some(note))) => {
                ui.label(
                    egui::RichText::new(format!("快捷键已生效：{note}"))
                        .size(theme::SIZE_AUX)
                        .color(theme::success()),
                );
            }
            Some((true, None)) => {
                ui.label(
                    egui::RichText::new(format!(
                        "快捷键当前生效：{}",
                        state.settings.hotkey_picker.label()
                    ))
                    .size(theme::SIZE_AUX)
                    .color(theme::success()),
                );
            }
            Some((false, reason)) => {
                ui.label(
                    egui::RichText::new(format!(
                        "注册失败：{}（可能被其他程序占用，请换一个组合）",
                        reason.clone().unwrap_or_else(|| "未知原因".into())
                    ))
                    .size(theme::SIZE_AUX)
                    .color(theme::danger()),
                );
            }
            None => {
                ui.label(theme::weak("（等待 Sys 线程回传注册状态）"));
            }
        }
        ui.add_space(14.0);
        let _ = actions;
    });
}

// ---------------------------------------------------------------------------
// 容量与清理
// ---------------------------------------------------------------------------

fn capacity_section(state: &mut UiState, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
    section(ui, "容量与清理", |ui| {
        // 条数上限
        let mut max_items = state.settings_draft.max_items as u64;
        let resp = ui
            .horizontal(|ui| {
                let w = (ui.available_width() - 260.0).max(160.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(w, 20.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.label(theme::body("历史条数上限"));
                        ui.label(theme::weak(
                            "超出后自动删除最旧的非收藏记录（收藏永不清理）",
                        ));
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::DragValue::new(&mut max_items)
                            .speed(100.0)
                            .range(500..=1_000_000u64)
                            .suffix(" 条"),
                    )
                })
                .inner
            })
            .inner;
        if resp.changed() {
            state.settings_draft.max_items = max_items as usize;
        }
        ui.add_space(12.0);

        mb_row(
            ui,
            state,
            "图片总量上限",
            "图片 blob 总量超限时清理最旧的非收藏图片",
            |s| s.max_image_bytes,
            |s, v| s.max_image_bytes = v,
            50.0..=10240.0,
        );
        mb_row(
            ui,
            state,
            "收藏文件副本上限",
            "达到上限后不再复制新文件实体（已有副本不会删除）",
            |s| s.max_favorite_file_bytes,
            |s, v| s.max_favorite_file_bytes = v,
            0.0..=2048.0,
        );
        mb_row(
            ui,
            state,
            "单条文本上限",
            "超长文本会被截断并标记",
            |s| s.max_text_bytes,
            |s, v| s.max_text_bytes = v,
            0.0625..=16.0,
        );
        mb_row(
            ui,
            state,
            "收藏单文件上限",
            "超过该大小的文件不再复制实体，仅记录路径",
            |s| s.max_file_bytes,
            |s, v| s.max_file_bytes = v,
            1.0..=512.0,
        );
        // 当前占用概览
        ui.add_space(4.0);
        ui.label(theme::weak(format!(
            "当前占用：图片 {} · 收藏文件 {} · 数据库 {}",
            fmt_size(state.stats.image_bytes),
            fmt_size(state.stats.favorite_file_bytes),
            fmt_size(state.stats.db_bytes)
        )));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(theme::weak(
                "立即按已保存的上限清理一次；收藏内容与收藏文件实体不会被删除。",
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, "立即清理", widgets::ButtonKind::Secondary).clicked() {
                    actions.push(UiAction::RefreshStats);
                    state.toast("正在清理并刷新占用…");
                }
            });
        });
    });
}

// ---------------------------------------------------------------------------
// 行为
// ---------------------------------------------------------------------------

fn behavior_section(state: &mut UiState, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
    section(ui, "行为", |ui| {
        // 失焦隐藏
        setting_row(
            ui,
            "失去焦点时自动隐藏面板",
            "默认开启；关闭后面板需要手动 Esc 关闭",
            |ui| {
                let mut v = state.settings_draft.hide_on_blur;
                if widgets::switch(ui, &mut v).changed() {
                    state.settings_draft.hide_on_blur = v;
                }
            },
        );
        // 粘贴后保留剪贴板
        setting_row(
            ui,
            "粘贴后保留剪贴板内容",
            "关闭后粘贴完成会恢复原剪贴板内容",
            |ui| {
                let mut v = state.settings_draft.keep_clipboard_after_paste;
                if widgets::switch(ui, &mut v).changed() {
                    state.settings_draft.keep_clipboard_after_paste = v;
                }
            },
        );
        // 粘贴后置顶
        setting_row(
            ui,
            "粘贴后把该条置顶",
            "让它出现在列表最前",
            |ui| {
                let mut v = state.settings_draft.move_to_top_after_paste;
                if widgets::switch(ui, &mut v).changed() {
                    state.settings_draft.move_to_top_after_paste = v;
                }
            },
        );
        // 粘贴按键
        setting_row(
            ui,
            "模拟粘贴按键",
            "部分老程序只认 Shift+Insert",
            |ui| {
                let mut key = state.settings_draft.paste_key;
                egui::ComboBox::from_id_salt("paste_key_combo")
                    .selected_text(key.label())
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for candidate in [PasteKey::CtrlV, PasteKey::ShiftInsert] {
                            ui.selectable_value(&mut key, candidate, candidate.label());
                        }
                    });
                if key != state.settings_draft.paste_key {
                    state.settings_draft.paste_key = key;
                }
            },
        );
        // 开机自启（立即生效）
        setting_row(
            ui,
            "开机自启",
            "写入 HKCU Run（当前用户，无需管理员）",
            |ui| {
                let mut v = state.settings_draft.autostart;
                if widgets::switch(ui, &mut v).changed() {
                    state.settings_draft.autostart = v;
                    state.settings.autostart = v;
                    actions.push(UiAction::SetAutostart(v));
                    state.toast(if v {
                        "已开启开机自启"
                    } else {
                        "已关闭开机自启"
                    });
                }
            },
        );
        // 暂停记录（立即生效）
        setting_row(
            ui,
            "暂停剪贴板记录",
            "暂停后不采集新内容（托盘菜单也可切换）",
            |ui| {
                let mut v = state.settings_draft.pause_capture;
                if widgets::switch(ui, &mut v).changed() {
                    state.settings_draft.pause_capture = v;
                    state.settings.pause_capture = v;
                    state.paused = v;
                    actions.push(UiAction::SetPause(v));
                    state.toast(if v {
                        "已暂停记录"
                    } else {
                        "已恢复记录"
                    });
                }
            },
        );
    });
}

// ---------------------------------------------------------------------------
// 忽略名单
// ---------------------------------------------------------------------------

fn ignore_section(state: &mut UiState, ui: &mut egui::Ui) {
    section(ui, "忽略应用名单", |ui| {
        ui.label(theme::weak(
            "名单内的进程复制的内容不会被记录（小写进程名，如 chrome.exe）。密码管理器通常已通过 Clipboard Viewer Ignore 自动跳过。",
        ));
        ui.add_space(10.0);
        if state.settings_draft.ignore_apps.is_empty() {
            ui.label(theme::weak("（名单为空）"));
        } else {
            let mut remove_index = None;
            let apps = state.settings_draft.ignore_apps.clone();
            for (idx, app) in apps.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new((ui.available_width() - 80.0).max(120.0), 24.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(theme::mono(app));
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, "移除", widgets::ButtonKind::Ghost).clicked() {
                            remove_index = Some(idx);
                        }
                    });
                });
            }
            if let Some(idx) = remove_index {
                state.settings_draft.ignore_apps.remove(idx);
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let input_w = (ui.available_width() - 110.0).max(160.0);
            ui.add_sized(
                Vec2::new(input_w, 30.0),
                egui::TextEdit::singleline(&mut state.ignore_input)
                    .hint_text("输入进程名，例如 wechat.exe")
                    .font(theme::font_body())
                    .text_color(theme::text())
                    .margin(egui::Margin::symmetric(10, 6)),
            );
            if widgets::button(ui, "添加", widgets::ButtonKind::Secondary).clicked() {
                let name = state.ignore_input.trim().to_ascii_lowercase();
                if name.is_empty() {
                    state.toast("请输入进程名");
                } else if state.settings_draft.ignore_apps.contains(&name) {
                    state.toast("该进程已在名单中");
                } else {
                    state.settings_draft.ignore_apps.push(name);
                    state.ignore_input.clear();
                    state.toast("已添加（保存后生效）");
                }
            }
        });
        ui.add_space(8.0);
        ui.label(theme::weak("提示：在下方输入进程名（如 notepad.exe）后点击「添加」；自动枚举运行中进程将在 v1.1 提供。"));
    });
}

// ---------------------------------------------------------------------------
// 数据操作
// ---------------------------------------------------------------------------

fn data_section(state: &mut UiState, ui: &mut egui::Ui, actions: &mut Vec<UiAction>) {
    section(ui, "数据", |ui| {
        path_row(
            ui,
            "数据目录",
            &crate::config::data_dir().display().to_string(),
        );
        ui.add_space(4.0);
        path_row(
            ui,
            "日志目录",
            &crate::config::logs_dir().display().to_string(),
        );
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            if widgets::button(ui, "打开数据目录", widgets::ButtonKind::Secondary).clicked() {
                actions.push(UiAction::OpenDataDir);
            }
            if widgets::button(ui, "导出 JSON", widgets::ButtonKind::Secondary).clicked() {
                actions.push(UiAction::ExportJson);
            }
            if widgets::button(ui, "刷新统计", widgets::ButtonKind::Ghost).clicked() {
                actions.push(UiAction::RefreshStats);
            }
        });
        ui.add_space(14.0);
        ui.label(theme::row_title("危险操作"));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut del_blobs = state.clear_delete_blobs;
            widgets::checkbox(ui, &mut del_blobs);
            if del_blobs != state.clear_delete_blobs {
                state.clear_delete_blobs = del_blobs;
            }
            ui.label(theme::aux("清空历史时同时删除图片/文件副本"));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, "清空历史…", widgets::ButtonKind::Danger).clicked() {
                state.request_clear();
            }
            if widgets::button(ui, "恢复默认设置", widgets::ButtonKind::Secondary).clicked() {
                state.settings_draft = Settings::default();
                state.toast("已恢复默认值（点保存生效）");
            }
        });
        ui.add_space(8.0);
        ui.label(theme::weak(
            "提示：清空历史不可恢复（含回收站）；收藏项也会被清空。导出为 JSON；导入与重建索引计划在 v1.1 提供。",
        ));
    });
}

// ---------------------------------------------------------------------------
// 保存栏
// ---------------------------------------------------------------------------

fn save_bar(state: &mut UiState, ui: &mut egui::Ui, rect: Rect, actions: &mut Vec<UiAction>) {
    let dirty = settings_dirty(state);
    let painter = ui.painter().clone();
    // 固定栏浮在设置页底部：不随 ScrollArea 滚走，始终能看到「保存设置」。
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(theme::RADIUS_CARD),
        theme::card(),
    );
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(theme::RADIUS_CARD),
        theme::divider(),
        egui::StrokeKind::Inside,
    );

    let inner = rect.shrink2(Vec2::new(16.0, 10.0));
    ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
        ui.horizontal(|ui| {
            if widgets::button(ui, "保存设置", widgets::ButtonKind::Primary).clicked() {
                state.save_settings_draft(actions);
            }
            if widgets::button(ui, "放弃修改", widgets::ButtonKind::Secondary).clicked() {
                state.settings_draft = state.settings.clone();
                state.rec.recording = false;
                state.rec.error = None;
                state.toast("已放弃修改");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if dirty {
                    ui.label(
                        egui::RichText::new("● 有未保存的修改")
                            .size(theme::SIZE_AUX)
                            .color(theme::pinned()),
                    );
                } else {
                    ui.label(theme::weak("设置已保存"));
                }
            });
        });
    });
}

/// 设置草稿与已生效设置是否有差异。
fn settings_dirty(state: &UiState) -> bool {
    match (
        serde_json::to_string(&state.settings),
        serde_json::to_string(&state.settings_draft),
    ) {
        (Ok(a), Ok(b)) => a != b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticky_footer_splits_content_without_overlap() {
        let inner = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(600.0, 500.0));
        let (content, footer) = split_page(inner);
        assert_eq!(footer.bottom(), inner.bottom());
        assert!((footer.height() - SAVE_BAR_HEIGHT).abs() < 0.01);
        assert_eq!(content.top(), inner.top());
        assert!(content.bottom() <= footer.top());
        assert!((footer.top() - content.bottom() - SAVE_BAR_GAP).abs() < 0.01);
    }
}

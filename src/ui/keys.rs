//! 键盘路由与格式化纯函数（Picker/Manager 快捷键，spec §6.1 与附录 B）。
//!
//! 这里只做「按键 → 语义命令」的映射，不碰 UI 状态，便于单元测试。
//! 事件通过 `Context::input_mut(|i| i.consume_key(..))` 消费，因此被处理的按键
//! 不会再传给 TextEdit 等控件（这是 Picker 里 ↑↓/Tab/Enter 能在搜索框聚焦时生效的关键）。

use crate::model::HotkeyChord;
use eframe::egui::{self, Key, Modifiers};

/// Picker 语义命令（spec §6.1）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerCommand {
    /// 上移一条。
    Up,
    /// 下移一条。
    Down,
    /// 上一页。
    PageUp,
    /// 下一页。
    PageDown,
    /// 第一条。
    Home,
    /// 最后一条。
    End,
    /// Enter：粘贴（写剪贴板 + 模拟 Ctrl+V）。
    Paste,
    /// Ctrl+Enter：仅复制到剪贴板。
    PlainCopy,
    /// Ctrl+1…9：直接粘贴第 N 条（0 基下标）。
    QuickPaste(usize),
    /// Tab：下一个过滤。
    FilterNext,
    /// Shift+Tab：上一个过滤。
    FilterPrev,
    /// Ctrl+P：收藏/取消收藏。
    TogglePin,
    /// Delete：删除选中项。
    Delete,
    /// Ctrl+,：打开设置。
    OpenSettings,
    /// Esc：隐藏面板。
    Close,
}

/// Manager 语义命令（spec 附录 B 管理台快捷键）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ManagerCommand {
    /// Ctrl+F：聚焦搜索框。
    FocusSearch,
    /// Ctrl+A：全选当前页。
    SelectAll,
    /// Delete：删除选中项。
    DeleteSelected,
    /// Ctrl+E：导出 JSON。
    Export,
    /// Esc：关闭详情/返回/隐藏。
    Escape,
}

/// 从输入状态消费 Picker 快捷键，返回本帧命令序列。
///
/// 匹配顺序遵循「更具体的组合优先」：`consume_key` 会忽略多余的 Shift/Alt，
/// 所以先匹配带修饰键的组合，再匹配裸键。
pub fn route_picker_keys(ctx: &egui::Context, allow_delete: bool) -> Vec<PickerCommand> {
    let mut out = Vec::new();
    ctx.input_mut(|i| {
        // ---- 带修饰键（更具体）----
        if i.consume_key(Modifiers::CTRL, Key::Enter) {
            out.push(PickerCommand::PlainCopy);
        } else if i.consume_key(Modifiers::NONE, Key::Enter) {
            out.push(PickerCommand::Paste);
        }

        for (key, idx) in [
            (Key::Num1, 0usize),
            (Key::Num2, 1),
            (Key::Num3, 2),
            (Key::Num4, 3),
            (Key::Num5, 4),
            (Key::Num6, 5),
            (Key::Num7, 6),
            (Key::Num8, 7),
            (Key::Num9, 8),
        ] {
            if i.consume_key(Modifiers::CTRL, key) {
                out.push(PickerCommand::QuickPaste(idx));
            }
        }
        if i.consume_key(Modifiers::CTRL, Key::P) {
            out.push(PickerCommand::TogglePin);
        }
        if i.consume_key(Modifiers::CTRL, Key::Comma) {
            out.push(PickerCommand::OpenSettings);
        }

        // ---- 裸键 ----
        // Shift+Tab 需先匹配（否则会被裸 Tab 的宽松匹配吃掉）；两分支独立，
        // 保证同一帧同时出现两种 Tab 时都能正确路由（真实输入同一帧只会有一个）。
        if i.consume_key(Modifiers::SHIFT, Key::Tab) {
            out.push(PickerCommand::FilterPrev);
        }
        if i.consume_key(Modifiers::NONE, Key::Tab) {
            out.push(PickerCommand::FilterNext);
        }
        if i.consume_key(Modifiers::NONE, Key::Escape) {
            out.push(PickerCommand::Close);
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
            out.push(PickerCommand::Up);
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
            out.push(PickerCommand::Down);
        }
        if i.consume_key(Modifiers::NONE, Key::PageUp) {
            out.push(PickerCommand::PageUp);
        }
        if i.consume_key(Modifiers::NONE, Key::PageDown) {
            out.push(PickerCommand::PageDown);
        }
        if i.consume_key(Modifiers::NONE, Key::Home) {
            out.push(PickerCommand::Home);
        }
        if i.consume_key(Modifiers::NONE, Key::End) {
            out.push(PickerCommand::End);
        }
        // 搜索框有文本时 Delete 归文本框编辑，避免「删记录」打断输入
        if allow_delete && i.consume_key(Modifiers::NONE, Key::Delete) {
            out.push(PickerCommand::Delete);
        }
    });
    out
}

/// 从输入状态消费 Manager 快捷键。
///
/// `typing` 为 true（有文本控件聚焦）时，只路由 Ctrl+F / Ctrl+E / Esc，
/// 避免 Delete/Ctrl+A 抢走文本编辑语义。
pub fn route_manager_keys(ctx: &egui::Context, typing: bool) -> Vec<ManagerCommand> {
    let mut out = Vec::new();
    ctx.input_mut(|i| {
        if i.consume_key(Modifiers::CTRL, Key::F) {
            out.push(ManagerCommand::FocusSearch);
        }
        if i.consume_key(Modifiers::CTRL, Key::E) {
            out.push(ManagerCommand::Export);
        }
        if i.consume_key(Modifiers::NONE, Key::Escape) {
            out.push(ManagerCommand::Escape);
        }
        if !typing {
            if i.consume_key(Modifiers::CTRL, Key::A) {
                out.push(ManagerCommand::SelectAll);
            }
            if i.consume_key(Modifiers::NONE, Key::Delete) {
                out.push(ManagerCommand::DeleteSelected);
            }
        }
    });
    out
}

/// 列表选中位置移动（`delta` 可为负；越界夹取；空列表返回 None）。
pub fn move_selection(current: Option<usize>, delta: i32, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let Some(cur) = current else {
        return Some(if delta >= 0 { 0 } else { len - 1 });
    };
    let next = (cur as i64 + delta as i64).clamp(0, len as i64 - 1) as usize;
    Some(next)
}

/// 跳到首/尾。
pub fn jump_selection(end: bool, len: usize) -> Option<usize> {
    if len == 0 {
        None
    } else if end {
        Some(len - 1)
    } else {
        Some(0)
    }
}

/// 一页的条数（按列表可视高度 / 行高，至少 1）。
pub fn page_rows(list_height: f32, row_height: f32) -> usize {
    if row_height <= 0.0 {
        return 1;
    }
    ((list_height / row_height).floor() as usize).max(1)
}

/// egui 逻辑键 → Windows 虚拟键码（用于快捷键录制）。
pub fn vk_from_key(key: Key) -> Option<u32> {
    let vk = match key {
        Key::A => 0x41,
        Key::B => 0x42,
        Key::C => 0x43,
        Key::D => 0x44,
        Key::E => 0x45,
        Key::F => 0x46,
        Key::G => 0x47,
        Key::H => 0x48,
        Key::I => 0x49,
        Key::J => 0x4A,
        Key::K => 0x4B,
        Key::L => 0x4C,
        Key::M => 0x4D,
        Key::N => 0x4E,
        Key::O => 0x4F,
        Key::P => 0x50,
        Key::Q => 0x51,
        Key::R => 0x52,
        Key::S => 0x53,
        Key::T => 0x54,
        Key::U => 0x55,
        Key::V => 0x56,
        Key::W => 0x57,
        Key::X => 0x58,
        Key::Y => 0x59,
        Key::Z => 0x5A,
        Key::Num0 => 0x30,
        Key::Num1 => 0x31,
        Key::Num2 => 0x32,
        Key::Num3 => 0x33,
        Key::Num4 => 0x34,
        Key::Num5 => 0x35,
        Key::Num6 => 0x36,
        Key::Num7 => 0x37,
        Key::Num8 => 0x38,
        Key::Num9 => 0x39,
        Key::F1 => 0x70,
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F4 => 0x73,
        Key::F5 => 0x74,
        Key::F6 => 0x75,
        Key::F7 => 0x76,
        Key::F8 => 0x77,
        Key::F9 => 0x78,
        Key::F10 => 0x79,
        Key::F11 => 0x7A,
        Key::F12 => 0x7B,
        Key::F13 => 0x7C,
        Key::F14 => 0x7D,
        Key::F15 => 0x7E,
        Key::F16 => 0x7F,
        Key::F17 => 0x80,
        Key::F18 => 0x81,
        Key::F19 => 0x82,
        Key::F20 => 0x83,
        Key::F21 => 0x84,
        Key::F22 => 0x85,
        Key::F23 => 0x86,
        Key::F24 => 0x87,
        Key::ArrowLeft => 0x25,
        Key::ArrowUp => 0x26,
        Key::ArrowRight => 0x27,
        Key::ArrowDown => 0x28,
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::Insert => 0x2D,
        Key::Delete => 0x2E,
        Key::Backspace => 0x08,
        Key::Tab => 0x09,
        Key::Enter => 0x0D,
        Key::Escape => 0x1B,
        Key::Space => 0x20,
        Key::Backtick => 0xC0,
        Key::Minus => 0xBD,
        Key::Equals | Key::Plus => 0xBB,
        Key::OpenBracket => 0xDB,
        Key::CloseBracket => 0xDD,
        Key::Backslash | Key::Pipe => 0xDC,
        Key::Semicolon | Key::Colon => 0xBA,
        Key::Quote => 0xDE,
        Key::Comma => 0xBC,
        Key::Period => 0xBE,
        Key::Slash | Key::Questionmark => 0xBF,
        _ => return None,
    };
    Some(vk)
}

/// 校验录制到的全局快捷键组合（返回中文错误提示）。
pub fn validate_chord(chord: &HotkeyChord) -> Result<(), String> {
    if !chord.is_set() {
        return Err("未录制到按键".to_string());
    }
    if chord.is_win_v() {
        return Err("Win+V 请在上方「接管 Win+V」中配置，备用快捷键请换一个组合".to_string());
    }
    let has_modifier = chord.ctrl || chord.alt || chord.shift || chord.win;
    let is_function_key = (0x70..=0x87).contains(&chord.vk);
    if !has_modifier && !is_function_key {
        return Err("全局快捷键需要至少一个修饰键（Ctrl/Alt/Shift），或使用 F1–F24".to_string());
    }
    if chord.vk == 0 {
        return Err("未录制到按键".to_string());
    }
    Ok(())
}

/// 数字加千分位：`1234` → `1,234`。
pub fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Picker 底部提示文案（spec §7.3）。
pub const PICKER_HINT: &str = "↑↓ 选择 · Enter 粘贴 · Ctrl+1…9 快选 · Esc 关闭";

/// Manager 历史页提示文案。
pub const MANAGER_HINT: &str =
    "Ctrl+F 搜索 · Ctrl+A 全选 · Del 删除 · Ctrl+E 导出 · Ctrl+、设置 · Esc 返回";

#[cfg(test)]
mod tests {
    use super::*;

    /// 注入一次按键事件。
    fn press(ctx: &egui::Context, key: Key, modifiers: Modifiers) {
        ctx.input_mut(|i| {
            i.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            });
        });
    }

    #[test]
    fn route_enter_and_ctrl_enter() {
        let ctx = egui::Context::default();
        press(&ctx, Key::Enter, Modifiers::NONE);
        assert_eq!(route_picker_keys(&ctx, true), vec![PickerCommand::Paste]);
        press(&ctx, Key::Enter, Modifiers::CTRL);
        assert_eq!(
            route_picker_keys(&ctx, true),
            vec![PickerCommand::PlainCopy]
        );
        // 已消费的按键不会被重复处理
        assert!(route_picker_keys(&ctx, true).is_empty());
    }

    #[test]
    fn route_quick_paste_digits() {
        let ctx = egui::Context::default();
        press(&ctx, Key::Num1, Modifiers::CTRL);
        press(&ctx, Key::Num9, Modifiers::CTRL);
        assert_eq!(
            route_picker_keys(&ctx, true),
            vec![PickerCommand::QuickPaste(0), PickerCommand::QuickPaste(8)]
        );
    }

    #[test]
    fn route_navigation_and_filters() {
        let ctx = egui::Context::default();
        press(&ctx, Key::ArrowDown, Modifiers::NONE);
        press(&ctx, Key::PageDown, Modifiers::NONE);
        press(&ctx, Key::Home, Modifiers::NONE);
        press(&ctx, Key::Tab, Modifiers::NONE);
        press(&ctx, Key::Tab, Modifiers::SHIFT);
        press(&ctx, Key::Escape, Modifiers::NONE);
        press(&ctx, Key::Delete, Modifiers::NONE);
        // 路由按「先修饰键组合、后裸键」的固定顺序消费，返回顺序与匹配顺序一致
        assert_eq!(
            route_picker_keys(&ctx, true),
            vec![
                PickerCommand::FilterPrev,
                PickerCommand::FilterNext,
                PickerCommand::Close,
                PickerCommand::Down,
                PickerCommand::PageDown,
                PickerCommand::Home,
                PickerCommand::Delete,
            ]
        );
    }

    #[test]
    fn route_ctrl_p_and_ctrl_comma() {
        let ctx = egui::Context::default();
        press(&ctx, Key::P, Modifiers::CTRL);
        press(&ctx, Key::Comma, Modifiers::CTRL);
        assert_eq!(
            route_picker_keys(&ctx, true),
            vec![PickerCommand::TogglePin, PickerCommand::OpenSettings]
        );
    }

    #[test]
    fn route_manager_keys_ignores_typing_conflicts() {
        let ctx = egui::Context::default();
        press(&ctx, Key::Delete, Modifiers::NONE);
        press(&ctx, Key::A, Modifiers::CTRL);
        press(&ctx, Key::F, Modifiers::CTRL);
        // typing=true：Delete / Ctrl+A 交给文本框
        assert_eq!(
            route_manager_keys(&ctx, true),
            vec![ManagerCommand::FocusSearch]
        );

        let ctx = egui::Context::default();
        press(&ctx, Key::Delete, Modifiers::NONE);
        press(&ctx, Key::A, Modifiers::CTRL);
        press(&ctx, Key::E, Modifiers::CTRL);
        assert_eq!(
            route_manager_keys(&ctx, false),
            vec![
                ManagerCommand::Export,
                ManagerCommand::SelectAll,
                ManagerCommand::DeleteSelected,
            ]
        );
    }

    #[test]
    fn selection_moves_and_clamps() {
        assert_eq!(move_selection(None, 1, 0), None);
        assert_eq!(move_selection(None, 1, 5), Some(0));
        assert_eq!(move_selection(None, -1, 5), Some(4));
        assert_eq!(move_selection(Some(0), -1, 5), Some(0));
        assert_eq!(move_selection(Some(4), 1, 5), Some(4));
        assert_eq!(move_selection(Some(2), 2, 5), Some(4));
        assert_eq!(jump_selection(false, 5), Some(0));
        assert_eq!(jump_selection(true, 5), Some(4));
        assert_eq!(jump_selection(true, 0), None);
        assert_eq!(page_rows(420.0, 64.0), 6);
        assert_eq!(page_rows(10.0, 64.0), 1);
        assert_eq!(page_rows(100.0, 0.0), 1);
    }

    #[test]
    fn count_formatting() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1234), "1,234");
        assert_eq!(fmt_count(1234567), "1,234,567");
    }

    #[test]
    fn vk_mapping_covers_common_keys() {
        assert_eq!(vk_from_key(Key::V), Some(0x56));
        assert_eq!(vk_from_key(Key::Num1), Some(0x31));
        assert_eq!(vk_from_key(Key::F12), Some(0x7B));
        assert_eq!(vk_from_key(Key::Backtick), Some(0xC0));
        assert_eq!(vk_from_key(Key::Comma), Some(0xBC));
        assert_eq!(vk_from_key(Key::ArrowDown), Some(0x28));
        // 无法映射为全局快捷键的键返回 None（不 panic）
        assert_eq!(vk_from_key(Key::Copy), None);
    }

    #[test]
    fn chord_validation() {
        let ctx = egui::Context::default(); // 仅用于保持测试环境一致
        drop(ctx);
        // 默认 Ctrl+` 合法
        assert!(validate_chord(&HotkeyChord::default()).is_ok());
        // Win+V 被接管配置占用
        let winv = HotkeyChord::new(false, false, false, true, 0x56);
        assert!(validate_chord(&winv).is_err());
        // 裸字母非法
        let bare = HotkeyChord::new(false, false, false, false, 0x41);
        assert!(validate_chord(&bare).is_err());
        // 裸 F 键合法
        let f9 = HotkeyChord::new(false, false, false, false, 0x78);
        assert!(validate_chord(&f9).is_ok());
        // 未设置
        let unset = HotkeyChord::new(true, false, false, false, 0);
        assert!(validate_chord(&unset).is_err());
    }
}

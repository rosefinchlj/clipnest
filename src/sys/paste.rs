//! 自动粘贴引擎：恢复前台窗口 → `SendInput` 模拟粘贴按键。
//!
//! 流程（spec §6.3）：
//! 1. 校验目标窗口（已销毁 → `TargetInvalid`；提权目标 → `UipiBlocked`）；
//! 2. `ShowWindow(SW_RESTORE)` + `AttachThreadInput` 绕过前台锁定，`SetForegroundWindow` 重试；
//! 3. 等待 40–80ms 让目标窗口真正拿到前台焦点；
//! 4. `SendInput` 发送 Ctrl+V 或 Shift+Insert。
//!
//! 失败不重试粘贴动作（剪贴板内容已写好，UI 提示「已复制，请手动粘贴」即可）。

use crate::model::PasteKey;
use crate::sys::util;
use std::time::Duration;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_INSERT, VK_SHIFT,
    VK_V,
};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindow,
    SetForegroundWindow, ShowWindow, SW_RESTORE,
};

/// 粘贴失败原因（可区分，用于 UI 文案与日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteError {
    /// 目标窗口无效/已销毁。
    TargetInvalid,
    /// 目标进程提权或前台恢复被 UIPI/前台锁阻止。
    UipiBlocked,
    /// SendInput 注入失败。
    SendInputFailed,
}

impl PasteError {
    /// 用户可见的中文原因。
    pub fn reason(self) -> &'static str {
        match self {
            PasteError::TargetInvalid => "目标窗口已失效，已复制到剪贴板，请手动粘贴",
            PasteError::UipiBlocked => "目标窗口以管理员权限运行，无法自动粘贴，请手动粘贴",
            PasteError::SendInputFailed => "模拟粘贴按键失败，已复制到剪贴板，请手动粘贴",
        }
    }
}

impl std::fmt::Display for PasteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// 恢复 `target` 为前台窗口并发送粘贴按键。
pub fn restore_and_paste(target: isize, key: PasteKey) -> Result<(), PasteError> {
    if target == 0 {
        return Err(PasteError::TargetInvalid);
    }
    let hwnd = target as HWND;
    if unsafe { IsWindow(hwnd) } == 0 {
        return Err(PasteError::TargetInvalid);
    }
    // 提权目标：本进程非提权时 UIPI 会拦截，提前给出可区分的错误
    let pid = util::window_pid(target);
    if pid != 0 && util::is_process_elevated(pid) && !util::is_elevated() {
        return Err(PasteError::UipiBlocked);
    }

    restore_foreground(hwnd)?;
    // 40–80ms：给目标窗口完成激活/输入队列切换（spec §6.3）
    std::thread::sleep(Duration::from_millis(60));

    if unsafe { GetForegroundWindow() } != hwnd {
        if unsafe { IsWindow(hwnd) } == 0 {
            return Err(PasteError::TargetInvalid);
        }
        return Err(PasteError::UipiBlocked);
    }

    send_paste_keys(key)
}

/// 把目标窗口恢复为前台（最小化则还原），失败返回 `UipiBlocked`。
fn restore_foreground(hwnd: HWND) -> Result<(), PasteError> {
    if unsafe { IsIconic(hwnd) } != 0 {
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
        }
    }

    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        // 附加到目标线程输入队列，绕过前台锁定（限制见 SetForegroundWindow 文档）
        let target_thread = unsafe { GetWindowThreadProcessId(hwnd, std::ptr::null_mut()) };
        let my_thread = unsafe { GetCurrentThreadId() };
        let attached = target_thread != 0
            && target_thread != my_thread
            && unsafe { AttachThreadInput(my_thread, target_thread, 1) } != 0;
        let ok = unsafe { SetForegroundWindow(hwnd) } != 0;
        unsafe {
            BringWindowToTop(hwnd);
        }
        if attached {
            unsafe {
                AttachThreadInput(my_thread, target_thread, 0);
            }
        }
        let _ = ok;
        if unsafe { GetForegroundWindow() } == hwnd {
            return Ok(());
        }
        // 目标窗口若已销毁，无需继续重试
        if unsafe { IsWindow(hwnd) } == 0 {
            return Err(PasteError::TargetInvalid);
        }
    }
    Err(PasteError::UipiBlocked)
}

/// 粘贴按键序列（纯函数，便于单元测试）。
///
/// 返回 `[(虚拟键码, 是否抬起)]`：修饰键按下 → 主键按下/抬起 → 修饰键抬起。
pub fn paste_key_sequence(key: PasteKey) -> [(u16, bool); 4] {
    match key {
        PasteKey::CtrlV => [
            (VK_CONTROL, false),
            (VK_V, false),
            (VK_V, true),
            (VK_CONTROL, true),
        ],
        PasteKey::ShiftInsert => [
            (VK_SHIFT, false),
            (VK_INSERT, false),
            (VK_INSERT, true),
            (VK_SHIFT, true),
        ],
    }
}

fn build_key_input(vk: u16, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if key_up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// 发送粘贴按键；`SendInput` 未全部入队视为失败。
fn send_paste_keys(key: PasteKey) -> Result<(), PasteError> {
    let seq = paste_key_sequence(key);
    let inputs: Vec<INPUT> = seq
        .iter()
        .map(|(vk, up)| build_key_input(*vk, *up))
        .collect();
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != inputs.len() as u32 {
        crate::logging::warn(&format!(
            "SendInput 粘贴按键失败：期望 {} 个事件，实际 {}（错误 {}）",
            inputs.len(),
            sent,
            std::io::Error::last_os_error()
        ));
        return Err(PasteError::SendInputFailed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_struct_size_is_40_on_x64() {
        // SendInput 要求 cbSize 与系统 INPUT 结构一致（x64 = 40）；不一致会返回错误 87。
        assert_eq!(std::mem::size_of::<INPUT>(), 40);
    }

    #[test]
    fn paste_sequence_ctrl_v() {
        let seq = paste_key_sequence(PasteKey::CtrlV);
        assert_eq!(seq[0], (VK_CONTROL, false));
        assert_eq!(seq[1], (VK_V, false));
        assert_eq!(seq[2], (VK_V, true));
        assert_eq!(seq[3], (VK_CONTROL, true));
    }

    #[test]
    fn paste_sequence_shift_insert() {
        let seq = paste_key_sequence(PasteKey::ShiftInsert);
        assert_eq!(seq[0], (VK_SHIFT, false));
        assert_eq!(seq[1], (VK_INSERT, false));
        assert_eq!(seq[2], (VK_INSERT, true));
        assert_eq!(seq[3], (VK_SHIFT, true));
    }

    #[test]
    fn invalid_targets_are_rejected() {
        assert_eq!(
            restore_and_paste(0, PasteKey::CtrlV),
            Err(PasteError::TargetInvalid)
        );
        // 任意伪造句柄必然 IsWindow=0
        assert_eq!(
            restore_and_paste(0x1234_5678, PasteKey::CtrlV),
            Err(PasteError::TargetInvalid)
        );
    }

    #[test]
    fn error_reasons_are_distinct_and_chinese() {
        let reasons = [
            PasteError::TargetInvalid.reason(),
            PasteError::UipiBlocked.reason(),
            PasteError::SendInputFailed.reason(),
        ];
        assert!(reasons.iter().all(|r| !r.is_empty()));
        assert_ne!(reasons[0], reasons[1]);
        assert_ne!(reasons[1], reasons[2]);
        assert!(reasons.iter().all(|r| r.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))));
    }
}

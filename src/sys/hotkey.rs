//! 全局快捷键：字符串解析、注册/注销、冲突诊断与降级链。
//!
//! 降级链（Lead 决策，配合 P0-FINDINGS）：
//! 用户配置的快捷键注册失败（最常见 `ERROR_HOTKEY_ALREADY_REGISTERED = 1409`）时，
//! 依次尝试 `Ctrl+Shift+V` → `` Alt+` `` → `Ctrl+Alt+Shift+V`，首个成功者生效；
//! 实际生效组合在 [`HotkeyRegistration::effective`] 中返回，供设置页提示与 `--diagnose`。
//!
//! 本模块只做「注册」这一层，不处理 `WM_HOTKEY` 消息（由 `thread.rs` 分发）。
#![allow(dead_code)] // Sys 域内部工具：部分函数仅供测试/诊断/后续接入使用


use crate::logging;
use crate::model::HotkeyChord;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

/// `ERROR_HOTKEY_ALREADY_REGISTERED`：组合已被系统或其他程序占用。
pub const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

/// 注册失败时的降级候选（按顺序尝试，去重后使用）。
pub fn fallback_chords() -> [HotkeyChord; 3] {
    [
        HotkeyChord::new(true, false, true, false, 0x56), // Ctrl+Shift+V
        HotkeyChord::new(false, true, false, false, 0xC0), // Alt+`
        HotkeyChord::new(true, true, true, false, 0x56),  // Ctrl+Alt+Shift+V
    ]
}

/// 一次快捷键注册的结果（含降级信息）。
#[derive(Clone, Debug)]
pub struct HotkeyRegistration {
    /// 是否有一个组合注册成功。
    pub ok: bool,
    /// 用户配置的（请求的）组合。
    pub requested: HotkeyChord,
    /// 实际生效的组合（失败时等于 requested）。
    pub effective: HotkeyChord,
    /// 非空表示发生了降级或失败，可直接展示给用户（中文）。
    pub note: Option<String>,
    /// 最后一个 Win32 错误码（成功后为 0 或中途尝试的错误码，仅供参考）。
    pub last_error: u32,
}

impl HotkeyRegistration {
    /// 供 `Event::HotkeyStatus` 使用的 (ok, reason)。
    pub fn status(&self) -> (bool, Option<String>) {
        (self.ok, self.note.clone())
    }
}

/// 快捷键组合 → `RegisterHotKey` 的 fsModifiers。
pub fn modifiers_of(chord: &HotkeyChord) -> u32 {
    // MOD_NOREPEAT (0x4000) 避免按住时重复触发；Win7+ 支持。
    const MOD_NOREPEAT: u32 = 0x4000;
    let mut mods = MOD_NOREPEAT;
    if chord.ctrl {
        mods |= MOD_CONTROL;
    }
    if chord.alt {
        mods |= MOD_ALT;
    }
    if chord.shift {
        mods |= MOD_SHIFT;
    }
    if chord.win {
        mods |= MOD_WIN;
    }
    mods
}

/// Win32 错误码 → 中文诊断文案。
pub fn describe_error(code: u32) -> String {
    if code == ERROR_HOTKEY_ALREADY_REGISTERED {
        "注册失败：快捷键已被系统或其他程序占用".to_string()
    } else {
        format!("注册失败：Win32 错误 {code}")
    }
}

/// 注册单个全局快捷键（调用方保证该 id 未注册）。失败返回 Win32 错误码。
pub fn register(hwnd: HWND, id: i32, chord: &HotkeyChord) -> Result<(), u32> {
    if !chord.is_set() {
        return Err(0);
    }
    let ok = unsafe { RegisterHotKey(hwnd, id, modifiers_of(chord), chord.vk) };
    if ok != 0 {
        logging::debug(&format!("热键注册成功：{}（id={id}）", chord.label()));
        Ok(())
    } else {
        let err = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(0) as u32;
        logging::warn(&format!(
            "热键注册失败：{}（id={id}，{}）",
            chord.label(),
            describe_error(err)
        ));
        Err(err)
    }
}

/// 注销全局快捷键（尽力而为，未注册也安全）。
pub fn unregister(hwnd: HWND, id: i32) {
    unsafe {
        UnregisterHotKey(hwnd, id);
    }
}

/// 注册「呼出面板」快捷键，失败时按降级链尝试备用组合。
///
/// 返回的 [`HotkeyRegistration::effective`] 是实际生效组合；全部失败时 `ok=false`，
/// 由调用方发 `Event::HotkeyStatus { ok:false, reason }`（不 panic、不返回 Err）。
pub fn register_with_fallback(hwnd: HWND, id: i32, requested: HotkeyChord) -> HotkeyRegistration {
    // 先注销同 id 的旧注册，避免 SetHotkeys 时 1409 误判
    unregister(hwnd, id);

    let mut candidates: Vec<HotkeyChord> = Vec::new();
    if requested.is_set() {
        candidates.push(requested);
    } else {
        logging::warn("未配置备用快捷键，使用默认 Ctrl+` 作为首选");
    }
    for chord in fallback_chords() {
        if candidates.iter().all(|c| *c != chord) {
            candidates.push(chord);
        }
    }

    let mut last_error = 0u32;
    for (index, candidate) in candidates.iter().enumerate() {
        match register(hwnd, id, candidate) {
            Ok(()) => {
                let note = if index == 0 {
                    None
                } else {
                    Some(format!(
                        "{} 被占用，已临时使用 {}",
                        if requested.is_set() {
                            requested.label()
                        } else {
                            "备用快捷键".to_string()
                        },
                        candidate.label()
                    ))
                };
                if let Some(n) = &note {
                    logging::warn(&format!("快捷键降级：{n}"));
                }
                return HotkeyRegistration {
                    ok: true,
                    requested,
                    effective: *candidate,
                    note,
                    last_error,
                };
            }
            Err(err) => {
                last_error = err;
                if index == 0 && err != ERROR_HOTKEY_ALREADY_REGISTERED {
                    // 非「占用」类错误（例如参数非法）也继续尝试降级组合
                    logging::warn(&format!(
                        "首选快捷键 {} {}",
                        requested.label(),
                        describe_error(err)
                    ));
                }
            }
        }
    }

    HotkeyRegistration {
        ok: false,
        requested,
        effective: requested,
        note: Some(format!(
            "{} 及备用组合均注册失败（{}），请到设置页改用其它快捷键",
            if requested.is_set() {
                requested.label()
            } else {
                "快捷键".to_string()
            },
            describe_error(last_error)
        )),
        last_error,
    }
}

// ---------------------------------------------------------------------------
// 字符串解析
// ---------------------------------------------------------------------------

/// 解析用户输入的快捷键字符串，例如：
/// `Ctrl+Shift+V`、`Ctrl+V`、`Win+V`、`` Alt+` ``、`Ctrl+F1`、`F12`、`Ctrl+Enter`。
///
/// 规则：修饰键不区分大小写、重复修饰键自动去重；必须且只能有一个主键；
/// 同时接受 `Ctrl`/`Control`、`Win`/`Windows`/`Meta`、`Esc`/`Escape` 等别名。
pub fn parse_chord(input: &str) -> Result<HotkeyChord, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("快捷键不能为空".to_string());
    }
    let parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!("快捷键格式非法：{input}（存在空分段，例如以 + 结尾）"));
    }

    let mut chord = HotkeyChord::default();
    chord.ctrl = false; // 清掉 Default 里的 Ctrl+` 预设，按输入解析
    let mut key: Option<&str> = None;
    for part in parts {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => chord.ctrl = true,
            "alt" => chord.alt = true,
            "shift" => chord.shift = true,
            "win" | "windows" | "meta" | "super" => chord.win = true,
            _ => {
                if key.is_some() {
                    return Err(format!(
                        "快捷键格式非法：{input}（只能有一个主键，多个主键：{key:?} 与 {part}）"
                    ));
                }
                key = Some(part);
            }
        }
    }

    let key = key.ok_or_else(|| format!("快捷键缺少主键：{input}"))?;
    chord.vk = parse_key(key)?;
    Ok(chord)
}

/// 单个主键 → VK 虚拟键码。
fn parse_key(key: &str) -> Result<u32, String> {
    let upper = key.to_ascii_uppercase();
    // 单字符
    if upper.chars().count() == 1 {
        let ch = upper.chars().next().unwrap_or('?');
        let vk = match ch {
            'A'..='Z' => ch as u32,
            '0'..='9' => ch as u32,
            '`' | '~' => 0xC0,
            '-' | '_' => 0xBD,
            '=' | '+' => 0xBB,
            '[' | '{' => 0xDB,
            ']' | '}' => 0xDD,
            '\\' | '|' => 0xDC,
            ';' | ':' => 0xBA,
            '\'' | '"' => 0xDE,
            ',' | '<' => 0xBC,
            '.' | '>' => 0xBE,
            '/' | '?' => 0xBF,
            _ => return Err(format!("不支持的按键：{key}")),
        };
        return Ok(vk);
    }

    // 功能键 F1..F24
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Ok(0x70 + (n - 1));
            }
            return Err(format!("功能键超出范围：{key}（支持 F1–F24）"));
        }
    }

    let vk = match upper.as_str() {
        "BACKSPACE" | "BACK" => 0x08,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0D,
        "ESC" | "ESCAPE" => 0x1B,
        "SPACE" | "SPACEBAR" => 0x20,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "END" => 0x23,
        "HOME" => 0x24,
        "LEFT" | "LEFTARROW" | "←" => 0x25,
        "UP" | "UPARROW" | "↑" => 0x26,
        "RIGHT" | "RIGHTARROW" | "→" => 0x27,
        "DOWN" | "DOWNARROW" | "↓" => 0x28,
        "INSERT" | "INS" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "PAUSE" => 0x13,
        "CAPSLOCK" => 0x14,
        "NUMLOCK" => 0x90,
        "SCROLLLOCK" => 0x91,
        "PRINTSCREEN" | "PRTSC" => 0x2C,
        other => return Err(format!("不支持的按键：{other}")),
    };
    Ok(vk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_common_chords() {
        let c = parse_chord("Ctrl+Shift+V").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && !c.win);
        assert_eq!(c.vk, 0x56);

        let c = parse_chord("Win+V").unwrap();
        assert!(c.win && !c.ctrl && !c.shift && !c.alt);
        assert!(c.is_win_v());

        let c = parse_chord("Alt+`").unwrap();
        assert!(c.alt);
        assert_eq!(c.vk, 0xC0);

        let c = parse_chord("Ctrl+`").unwrap();
        assert_eq!(c, HotkeyChord::default());
    }

    #[test]
    fn parse_is_case_insensitive_and_dedupes_modifiers() {
        assert_eq!(
            parse_chord("ctrl+shift+v").unwrap(),
            parse_chord("Ctrl+Shift+V").unwrap()
        );
        assert_eq!(
            parse_chord("Ctrl+Ctrl+V").unwrap(),
            parse_chord("Ctrl+V").unwrap()
        );
        assert_eq!(
            parse_chord("Windows+V").unwrap(),
            parse_chord("Win+V").unwrap()
        );
        assert_eq!(
            parse_chord("Control+Enter").unwrap(),
            HotkeyChord::new(true, false, false, false, 0x0D)
        );
    }

    #[test]
    fn parse_named_and_function_keys() {
        assert_eq!(parse_chord("F12").unwrap().vk, 0x7B);
        assert_eq!(parse_chord("PageUp").unwrap().vk, 0x21);
        assert_eq!(parse_chord("Space").unwrap().vk, 0x20);
        assert_eq!(parse_chord("Ctrl+F1").unwrap().vk, 0x70);
        assert_eq!(parse_chord("Ctrl+-").unwrap().vk, 0xBD);
    }

    #[test]
    fn parse_rejects_invalid() {
        for bad in ["", "   ", "Ctrl+", "+V", "Ctrl+Alt", "Foo+V", "Ctrl+A+B", "F25"] {
            assert!(parse_chord(bad).is_err(), "应判为非法：{bad:?}");
        }
    }

    #[test]
    fn label_roundtrip() {
        for text in ["Ctrl+Shift+V", "Win+V", "Alt+`", "Ctrl+F1", "Ctrl+Alt+Shift+V"] {
            let chord = parse_chord(text).unwrap();
            assert_eq!(chord.label(), text, "label 往返不一致：{text}");
        }
    }

    #[test]
    fn fallback_chain_is_valid_and_distinct() {
        let fallbacks = fallback_chords();
        let mut seen: Vec<HotkeyChord> = Vec::new();
        for chord in fallbacks {
            assert!(chord.is_set());
            assert!(!chord.is_win_v(), "降级链不应包含 Win+V");
            assert!(!seen.contains(&chord), "降级链存在重复组合");
            seen.push(chord);
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], parse_chord("Ctrl+Shift+V").unwrap());
        assert_eq!(seen[1], parse_chord("Alt+`").unwrap());
        assert_eq!(seen[2], parse_chord("Ctrl+Alt+Shift+V").unwrap());
    }

    #[test]
    fn modifiers_include_norepeat_and_flags() {
        let chord = parse_chord("Ctrl+Alt+Shift+Win+V").unwrap();
        let mods = modifiers_of(&chord);
        assert_eq!(mods & MOD_CONTROL, MOD_CONTROL);
        assert_eq!(mods & MOD_ALT, MOD_ALT);
        assert_eq!(mods & MOD_SHIFT, MOD_SHIFT);
        assert_eq!(mods & MOD_WIN, MOD_WIN);
        assert_eq!(mods & 0x4000, 0x4000, "应带 MOD_NOREPEAT");
    }

    #[test]
    fn error_description() {
        assert!(describe_error(ERROR_HOTKEY_ALREADY_REGISTERED).contains("占用"));
        assert!(describe_error(87).contains("87"));
    }

    #[test]
    fn unregister_and_register_with_null_hwnd_are_safe() {
        // NULL hwnd = 线程级热键（可能注册成功）；本测试只验证不 panic、结束时注销干净。
        // 用不太可能被占用的组合，避免与真实用户快捷键竞争。
        let requested = parse_chord("Ctrl+Alt+Shift+F9").unwrap();
        let reg = register_with_fallback(std::ptr::null_mut(), 9999, requested);
        // 无论成功与否都注销（成功时是线程级热键，测试进程内必须清理）
        unregister(std::ptr::null_mut(), 9999);
        // 失败时应有中文原因；成功时不应有降级说明
        if !reg.ok {
            assert!(reg.note.is_some());
        }
        // 注：注册结果取决于本机环境（可能被其它程序占用），不做强断言。
    }
}

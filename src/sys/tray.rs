//! 托盘图标与右键菜单（tray-icon 0.26）。
//!
//! 约定（CONTRACT §3.2）：**托盘由 Sys 线程创建并持有**，菜单事件通过全局 handler
//! 转发成 `WM_APP_MENU` 投递给 Sys 隐藏窗口，由 `thread.rs` 统一处理——
//! 这样业务状态（设置/事件）都留在 Sys 线程内，不跨线程共享 UI 句柄。
//!
//! 托盘左键单击 → 打开面板（`menu_on_left_click=false`，右键才弹菜单，符合 spec §20.B）；
//! 菜单项：打开面板 / 管理历史 / 暂停记录 / 清空历史 / 设置 / 开机自启 / 退出。

use crate::logging;
use crate::model::Settings;
use std::path::PathBuf;
use tray_icon::menu::{
    accelerator::Accelerator, CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::sys::thread::{WM_APP_MENU, WM_APP_TRAY};

/// 菜单动作编码（通过 `WM_APP_MENU` 的 wparam 传递）。
pub const MENU_OPEN: u32 = 1;
pub const MENU_MANAGER: u32 = 2;
pub const MENU_PAUSE: u32 = 3;
pub const MENU_CLEAR: u32 = 4;
pub const MENU_SETTINGS: u32 = 5;
pub const MENU_AUTOSTART: u32 = 6;
pub const MENU_QUIT: u32 = 7;

/// 托盘左键单击动作编码（通过 `WM_APP_TRAY` 的 wparam 传递）。
pub const TRAY_LEFT_CLICK: u32 = 1;

/// 托盘句柄集合（必须在 Sys 线程创建、持有、drop）。
pub struct TrayState {
    /// 图标本体；drop 时自动从通知区移除。
    _tray: TrayIcon,
    /// 暂停记录勾选项（需要动态更新勾选状态）。
    pause_item: CheckMenuItem,
    /// 开机自启勾选项。
    autostart_item: CheckMenuItem,
}

impl TrayState {
    pub fn set_paused(&self, paused: bool) {
        self.pause_item.set_checked(paused);
    }

    pub fn set_autostart(&self, enabled: bool) {
        self.autostart_item.set_checked(enabled);
    }
}

/// 菜单 id 字符串 → 动作编码。
fn menu_code(id: &str) -> Option<u32> {
    match id {
        "tray.open" => Some(MENU_OPEN),
        "tray.manager" => Some(MENU_MANAGER),
        "tray.pause" => Some(MENU_PAUSE),
        "tray.clear" => Some(MENU_CLEAR),
        "tray.settings" => Some(MENU_SETTINGS),
        "tray.autostart" => Some(MENU_AUTOSTART),
        "tray.quit" => Some(MENU_QUIT),
        _ => None,
    }
}

/// 安装托盘/菜单全局事件处理器（进程内只需一次；必须在创建托盘前调用）。
///
/// handler 在系统投递消息的线程执行（即 Sys 线程的消息泵），这里只做转投递，
/// 具体动作由 `thread.rs` 在 Sys 线程内处理。
pub fn install_event_handlers(hwnd: HWND) {
    let target = hwnd as isize;
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let id = event.id().0.clone();
        if let Some(code) = menu_code(&id) {
            logging::debug(&format!("托盘菜单事件：{id}"));
            unsafe {
                PostMessageW(
                    target as HWND,
                    WM_APP_MENU,
                    code as usize,
                    0,
                );
            }
        }
    }));

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // 托盘左键「抬起」→ 打开面板（单击动作）
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            unsafe {
                PostMessageW(
                    target as HWND,
                    WM_APP_TRAY,
                    TRAY_LEFT_CLICK as usize,
                    0,
                );
            }
        }
    }));
}

/// 加载托盘图标：优先 `assets/icon.png`，失败用内置 16×16 兜底（spec §14）。
fn load_icon() -> Option<Icon> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("assets").join("icon.png"));
            candidates.push(dir.join("icon.png"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png"));
    candidates.push(PathBuf::from("assets/icon.png"));

    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match image::load_from_memory(&bytes) {
            Ok(img) => {
                let rgba = img.into_rgba8();
                let (w, h) = (rgba.width(), rgba.height());
                match Icon::from_rgba(rgba.into_raw(), w, h) {
                    Ok(icon) => {
                        logging::debug(&format!("托盘图标加载成功：{}（{w}×{h}）", path.display()));
                        return Some(icon);
                    }
                    Err(e) => logging::warn(&format!("托盘图标格式无效（{}）：{e}", path.display())),
                }
            }
            Err(e) => logging::warn(&format!("托盘图标解码失败（{}）：{e}", path.display())),
        }
    }
    logging::warn("托盘图标加载失败，使用内置兜底图标");
    fallback_icon()
}

/// 内置 16×16 蓝色兜底图标（失败返回 None，由调用方降级为「无托盘」，绝不 panic）。
fn fallback_icon() -> Option<Icon> {
    let mut rgba = vec![0u8; 16 * 16 * 4];
    for px in rgba.chunks_exact_mut(4) {
        px[0] = 110;
        px[1] = 139;
        px[2] = 255;
        px[3] = 255;
    }
    Icon::from_rgba(rgba, 16, 16).ok()
}

/// 创建托盘 + 完整菜单。失败返回错误文案（调用方发 toast/日志，不 panic）。
pub fn create(settings: &Settings) -> Result<TrayState, String> {
    let menu = Menu::new();
    let item_open = MenuItem::with_id("tray.open", "打开面板", true, None::<Accelerator>);
    let item_manager = MenuItem::with_id("tray.manager", "管理历史", true, None::<Accelerator>);
    let item_pause = CheckMenuItem::with_id(
        "tray.pause",
        "暂停记录",
        true,
        settings.pause_capture,
        None::<Accelerator>,
    );
    let item_clear = MenuItem::with_id("tray.clear", "清空历史…", true, None::<Accelerator>);
    let item_settings = MenuItem::with_id("tray.settings", "设置", true, None::<Accelerator>);
    let item_autostart = CheckMenuItem::with_id(
        "tray.autostart",
        "开机自启",
        true,
        crate::sys::autostart::get(),
        None::<Accelerator>,
    );
    let item_quit = MenuItem::with_id("tray.quit", "退出 ClipNest", true, None::<Accelerator>);

    menu.append(&item_open)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_manager)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_pause)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_clear)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_settings)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_autostart)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_quit)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;

    let icon = load_icon().ok_or_else(|| "托盘图标创建失败（assets/icon.png 缺失且内置图标构造失败）".to_string())?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("ClipNest · 剪贴匣")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        // 左键单击打开面板、右键弹菜单（spec §20.B）
        .with_menu_on_left_click(false)
        .with_menu_on_right_click(true)
        .build()
        .map_err(|e| format!("托盘创建失败：{e}"))?;

    logging::log("托盘创建成功（打开面板/管理历史/暂停/清空/设置/开机自启/退出）");
    Ok(TrayState {
        _tray: tray,
        pause_item: item_pause,
        autostart_item: item_autostart,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_codes_are_unique() {
        let codes = [
            MENU_OPEN,
            MENU_MANAGER,
            MENU_PAUSE,
            MENU_CLEAR,
            MENU_SETTINGS,
            MENU_AUTOSTART,
            MENU_QUIT,
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len());
        assert_eq!(menu_code("tray.open"), Some(MENU_OPEN));
        assert_eq!(menu_code("tray.quit"), Some(MENU_QUIT));
        assert_eq!(menu_code("unknown"), None);
    }
}

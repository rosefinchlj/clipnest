//! 托盘图标与右键菜单（tray-icon 0.26）。
//!
//! 约定（CONTRACT §3.2）：**托盘由 Sys 线程创建并持有**，菜单事件通过全局 handler
//! 转发成 `WM_APP_MENU` 投递给 Sys 隐藏窗口，由 `thread.rs` 统一处理——
//! 这样业务状态（设置/事件）都留在 Sys 线程内，不跨线程共享 UI 句柄。
//!
//! 托盘左键单击 → 打开面板（`menu_on_left_click=false`，右键才弹菜单，符合 spec §20.B）；
//! 菜单项：打开面板 / 管理历史 / 暂停记录 / 清空历史 / 设置 / 开机自启 / 退出。

use crate::logging;
use crate::model::{Settings, TempPause};
use std::path::PathBuf;
use tray_icon::menu::{
    accelerator::Accelerator, CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuItem,
    PredefinedMenuItem,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::sys::thread::{WM_APP_MENU, WM_APP_TRAY};

/// 菜单动作编码（通过 `WM_APP_MENU` 的 wparam 传递）。
pub const MENU_OPEN: u32 = 1;
pub const MENU_MANAGER: u32 = 2;
pub const MENU_PAUSE: u32 = 3;
pub const MENU_SETTINGS: u32 = 5;
pub const MENU_AUTOSTART: u32 = 6;
pub const MENU_QUIT: u32 = 7;
/// 临时暂停 15 分钟（A3，运行期生效、不写 settings）。
pub const MENU_PAUSE_15: u32 = 8;
/// 临时暂停 1 小时。
pub const MENU_PAUSE_60: u32 = 9;
/// 临时暂停到本次运行结束（重启后恢复记录）。
pub const MENU_PAUSE_RESTART: u32 = 10;

/// 托盘左键单击动作编码（通过 `WM_APP_TRAY` 的 wparam 传递）。
pub const TRAY_LEFT_CLICK: u32 = 1;
/// 托盘右键单击动作编码（由 Sys 线程自己弹出菜单，见 [`TrayState::show_menu`]）。
pub const TRAY_RIGHT_CLICK: u32 = 2;

/// 托盘句柄集合（必须在 Sys 线程创建、持有、drop）。
pub struct TrayState {
    /// 图标本体；drop 时自动从通知区移除。
    tray: TrayIcon,
    /// 菜单本体：不由 tray-icon 自动弹出（它内部 TrackPopupMenu 不产生 MenuEvent），
    /// 而是右键时在 Sys 线程调用 `show_context_menu_for_hwnd`，这样点击项会通过
    /// muda 的 MenuEvent handler 回到 `install_event_handlers`。
    menu: Menu,
    /// 暂停记录勾选项（需要动态更新勾选状态）。
    pause_item: CheckMenuItem,
    /// 开机自启勾选项。
    autostart_item: CheckMenuItem,
    /// 临时暂停状态文字（禁用项，只用于展示）。
    temp_status_item: MenuItem,
}

impl TrayState {
    pub fn set_paused(&self, paused: bool) {
        self.pause_item.set_checked(paused);
    }

    pub fn set_autostart(&self, enabled: bool) {
        self.autostart_item.set_checked(enabled);
    }

    /// 更新临时暂停状态展示（A3）。
    pub fn set_temp_pause(&self, pause: TempPause, _permanent_paused: bool) {
        self.temp_status_item
            .set_text(format!("临时暂停：{}", temp_pause_short(pause)));
    }

    /// 托盘 tooltip 追加当前暂停状态（未暂停时恢复默认文案）。
    pub fn set_pause_tooltip(&self, pause: TempPause, permanent_paused: bool) {
        let text = if permanent_paused {
            "ClipNest · 剪贴匣（已暂停记录）".to_string()
        } else {
            match pause {
                TempPause::Off => "ClipNest · 剪贴匣".to_string(),
                _ => format!("ClipNest · 剪贴匣（{}）", temp_pause_short(pause)),
            }
        };
        if let Err(err) = self.tray.set_tooltip(Some(&text)) {
            logging::debug(&format!("更新托盘 tooltip 失败：{err}"));
        }
    }

    /// 在指定 owner 窗口上弹出托盘菜单（阻塞式 TrackPopupMenu，但事件循环正常）。
    /// 必须由 Sys 线程调用（菜单归属创建线程）。
    pub fn show_menu(&self, owner: HWND) {
        unsafe {
            self.menu.show_context_menu_for_hwnd(owner as isize, None);
        }
    }
}

/// 临时暂停的短文案（菜单/托盘 tooltip 用）。
pub fn temp_pause_short(pause: TempPause) -> String {
    match pause {
        TempPause::Off => "未开启".to_string(),
        TempPause::Timed { until_ms } => {
            let minutes = ((until_ms - crate::model::now_ms()).max(0) + 59_999) / 60_000;
            format!("{minutes} 分钟后恢复")
        }
        TempPause::UntilRestart => "直到重启".to_string(),
    }
}

/// 菜单 id 字符串 → 动作编码。
fn menu_code(id: &str) -> Option<u32> {
    match id {
        "tray.open" => Some(MENU_OPEN),
        "tray.manager" => Some(MENU_MANAGER),
        "tray.pause" => Some(MENU_PAUSE),
        "tray.settings" => Some(MENU_SETTINGS),
        "tray.autostart" => Some(MENU_AUTOSTART),
        "tray.quit" => Some(MENU_QUIT),
        "tray.pause15" => Some(MENU_PAUSE_15),
        "tray.pause60" => Some(MENU_PAUSE_60),
        "tray.pause_restart" => Some(MENU_PAUSE_RESTART),
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
                PostMessageW(target as HWND, WM_APP_MENU, code as usize, 0);
            }
        }
    }));

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        let code = match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => TRAY_LEFT_CLICK,
            TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            } => TRAY_RIGHT_CLICK,
            _ => return,
        };
        unsafe {
            PostMessageW(target as HWND, WM_APP_TRAY, code as usize, 0);
        }
    }));
}

/// 加载托盘图标：优先 `assets/icon.png`，失败用内置 16×16 兜底（spec §14）。
/// 系统 DPI 与托盘图标建议像素（16 逻辑像素 × 缩放）。
fn tray_icon_target() -> (u32, u32) {
    use windows_sys::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
    let dpi = unsafe {
        let dc = GetDC(std::ptr::null_mut());
        if dc.is_null() {
            96
        } else {
            let dpi = GetDeviceCaps(dc, LOGPIXELSX as i32);
            ReleaseDC(std::ptr::null_mut(), dc);
            if dpi <= 0 {
                96
            } else {
                dpi as u32
            }
        }
    };
    let px = ((16 * dpi + 48) / 96).clamp(16, 48);
    (dpi, px)
}

fn load_icon() -> Option<Icon> {
    // 1) 按系统 DPI 选**同尺寸**的预渲染 PNG，避免 256×256 被系统缩放到 16/20/24/32px 发虚。
    static P16: &[u8] = include_bytes!("../../assets/icon-16.png");
    static P20: &[u8] = include_bytes!("../../assets/icon-20.png");
    static P24: &[u8] = include_bytes!("../../assets/icon-24.png");
    static P28: &[u8] = include_bytes!("../../assets/icon-28.png");
    static P32: &[u8] = include_bytes!("../../assets/icon-32.png");
    let (dpi, want) = tray_icon_target();
    let choices: [(u32, &'static [u8]); 5] =
        [(16, P16), (20, P20), (24, P24), (28, P28), (32, P32)];
    let (size, bytes) = choices
        .iter()
        .min_by_key(|(s, _)| (*s as i64 - want as i64).abs())
        .copied()
        .unwrap_or((16, P16));
    if let Ok(img) = image::load_from_memory(bytes) {
        let rgba = img.into_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        if let Ok(icon) = Icon::from_rgba(rgba.into_raw(), w, h) {
            logging::log(&format!(
                "托盘图标：使用预渲染 {w}×{h} 图标（DPI {dpi}，目标 {want}px）"
            ));
            return Some(icon);
        }
    }
    // 2) 次选：exe 内嵌的多尺寸图标资源（如果资源编译成功，HICON 也会随 DPI 选用合适尺寸）。
    #[cfg(windows)]
    {
        match Icon::from_resource(1, None) {
            Ok(icon) => {
                logging::log("托盘图标：使用 exe 内嵌图标资源");
                return Some(icon);
            }
            Err(err) => logging::debug(&format!("内嵌图标资源加载失败（{err}）")),
        }
    }
    let _ = size;
    // 3) 开发期回退：运行时 assets 目录。
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
                    Err(e) => {
                        logging::warn(&format!("托盘图标格式无效（{}）：{e}", path.display()))
                    }
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
    // A3：临时暂停（运行期生效，不写 settings.json）
    let temp_status_item = MenuItem::with_id(
        "tray.temp_status",
        "临时暂停：未开启",
        false,
        None::<Accelerator>,
    );
    let item_pause15 = MenuItem::with_id("tray.pause15", "暂停 15 分钟", true, None::<Accelerator>);
    let item_pause60 = MenuItem::with_id("tray.pause60", "暂停 1 小时", true, None::<Accelerator>);
    let item_pause_restart = MenuItem::with_id(
        "tray.pause_restart",
        "暂停到重启",
        true,
        None::<Accelerator>,
    );
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
    menu.append(&temp_status_item)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_pause15)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_pause60)
        .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
    menu.append(&item_pause_restart)
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

    let icon = load_icon()
        .ok_or_else(|| "托盘图标创建失败（assets/icon.png 缺失且内置图标构造失败）".to_string())?;
    // 注意：不要把 menu 交给 tray-icon（它内部 TrackPopupMenu 没有 TPM_RETURNCMD，
    // 选中项只会发 WM_COMMAND 到无人处理的窗口，MenuEvent 永远不会触发）。
    // 菜单保存在 TrayState，右键时由 Sys 线程用 muda 的 show_context_menu_for_hwnd 弹出。
    let tray = TrayIconBuilder::new()
        .with_tooltip("ClipNest · 剪贴匣")
        .with_icon(icon)
        .build()
        .map_err(|e| format!("托盘创建失败：{e}"))?;

    logging::log("托盘创建成功（打开面板/管理历史/暂停/临时暂停/设置/开机自启/退出）");
    Ok(TrayState {
        tray,
        menu,
        pause_item: item_pause,
        autostart_item: item_autostart,
        temp_status_item,
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
            MENU_SETTINGS,
            MENU_AUTOSTART,
            MENU_QUIT,
            MENU_PAUSE_15,
            MENU_PAUSE_60,
            MENU_PAUSE_RESTART,
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len());
        assert_eq!(menu_code("tray.open"), Some(MENU_OPEN));
        assert_eq!(menu_code("tray.quit"), Some(MENU_QUIT));
        assert_eq!(menu_code("tray.pause15"), Some(MENU_PAUSE_15));
        assert_eq!(menu_code("tray.clear"), None);
        assert_eq!(menu_code("tray.resume"), None);
        assert_eq!(menu_code("tray.temp_status"), None);
        assert_eq!(menu_code("unknown"), None);
    }

    #[test]
    fn temp_pause_short_texts() {
        assert_eq!(temp_pause_short(TempPause::Off), "未开启");
        assert_eq!(temp_pause_short(TempPause::UntilRestart), "直到重启");
        let pause = TempPause::Timed {
            until_ms: crate::model::now_ms() + 15 * 60_000,
        };
        assert!(temp_pause_short(pause).contains("15 分钟"));
    }
}

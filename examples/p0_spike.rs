//! ClipNest P0 能力验证 spike（独立 example，不引用主程序模块）。
//!
//! 验证目标（spec §13 / 实施计划 Task 1）：
//! a) 亚克力：透明无边框 always-on-top 窗口 + `window_vibrancy`（供截图人工确认）；
//! b) `RegisterHotKey(MOD_WIN, 'V')` 能否成功（Windows 通常已被系统剪贴板历史占用）；
//! c) `WH_KEYBOARD_LL` 吞 Win+V 后是否误弹开始菜单 / Win 键状态是否正常（含补偿策略对比）；
//! d) `tray-icon` 能否在 eframe 主线程创建并响应菜单事件。
//!
//! 运行方式（测试脚本见 `examples/p0_test.ps1`）：
//! ```text
//! cargo run --example p0_spike -- --seconds 10 --headless --hook swallow_v --test-injected
//! cargo run --example p0_spike -- --seconds 12 --register-ctrl --register-winv
//! ```
//! 所有热键/钩子/托盘在退出前清理；进程退出即释放，不会长期占用输入。
//!
//! 本文件只用于 P0 风险验证，正式实现见 `src/sys/**`（由本 spike 结论驱动）。

#![allow(dead_code)]

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use tray_icon::menu::{
    accelerator::Accelerator, CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_F10, VK_LWIN, VK_OEM_3, VK_RWIN,
    VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW,
    KBDLLHOOKSTRUCT, PostMessageW, PostQuitMessage, RegisterClassW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, DestroyWindow, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL,
    WM_CLOSE, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW,
    HWND_MESSAGE,
};

// ---------------------------------------------------------------------------
// 日志
// ---------------------------------------------------------------------------

static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

/// 写一行日志（同时输出到 stderr）。P0 期间用统一前缀方便 PowerShell 解析。
fn p0log(msg: &str) {
    let line = format!(
        "[{}] {msg}\n",
        chrono::Local::now().format("%H:%M:%S%.3f")
    );
    eprint!("{line}");
    if let Some(file) = LOG_FILE.get() {
        if let Ok(mut guard) = file.lock() {
            let _ = guard.write_all(line.as_bytes());
            let _ = guard.flush();
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HookMode {
    /// 不安装钩子。
    Off,
    /// 吞 Win 按住时的 V down/up（Win up 放行）。
    SwallowV,
    /// 吞 V down/up，并吞掉紧随其后的 Win up（开始菜单补偿）。
    SwallowVWinUp,
    /// 同 SwallowVWinUp，但补吞 Win up 后 30ms 注入一次 Win up 清掉系统键状态（补偿修复版）。
    SwallowVWinUpFix,
}

impl HookMode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(HookMode::Off),
            "swallow_v" => Some(HookMode::SwallowV),
            "swallow_v_winup" | "swallow_v+winup" => Some(HookMode::SwallowVWinUp),
            "swallow_v_winup_fix" => Some(HookMode::SwallowVWinUpFix),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            HookMode::Off => "off",
            HookMode::SwallowV => "swallow_v",
            HookMode::SwallowVWinUp => "swallow_v_winup",
            HookMode::SwallowVWinUpFix => "swallow_v_winup_fix",
        }
    }
}

struct Args {
    /// 不创建 eframe 窗口（热键/钩子测试专用，快且不打扰桌面）。
    headless: bool,
    /// 运行秒数（从 READY 起算）。
    seconds: u64,
    hook: HookMode,
    /// 钩子是否处理注入事件（P0 用 SendInput 模拟按键必须开；正式实现必须忽略注入）。
    test_injected: bool,
    /// 是否尝试 L1：RegisterHotKey(MOD_WIN,'V')。
    register_winv: bool,
    /// 是否注册备用快捷键 Ctrl+`。
    register_ctrl: bool,
    /// 是否注册一个空闲测试组合 Ctrl+Alt+Shift+F10（本机 Ctrl+` 被 Ditto 占用，
    /// 用空闲组合验证 WM_HOTKEY + SendInput 整条链路）。
    register_test: bool,
    /// 是否创建托盘（headless 时在 Sys 线程创建；GUI 时在 eframe 主线程创建）。
    tray: bool,
    /// headless/GUI 模式下，READY 后第 N 秒模拟一次托盘左键点击（验证菜单响应）。
    synth_click_after: Option<u64>,
    log: PathBuf,
    ready: PathBuf,
    result: PathBuf,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            headless: false,
            seconds: 10,
            hook: HookMode::Off,
            test_injected: false,
            register_winv: false,
            register_ctrl: false,
            register_test: false,
            tray: true,
            synth_click_after: None,
            log: PathBuf::from("target/p0-spike.log"),
            ready: PathBuf::from("target/p0-spike.ready"),
            result: PathBuf::from("target/p0-spike.json"),
        }
    }
}

impl Args {
    fn parse() -> Self {
        let mut args = Args::default();
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--headless" => args.headless = true,
                "--seconds" => {
                    if let Some(v) = it.next().and_then(|s| s.parse().ok()) {
                        args.seconds = v;
                    }
                }
                "--hook" => {
                    if let Some(v) = it.next() {
                        match HookMode::parse(&v) {
                            Some(m) => args.hook = m,
                            None => p0log(&format!("ARGS|未知 hook 模式 {v}")),
                        }
                    }
                }
                "--test-injected" => args.test_injected = true,
                "--register-winv" => args.register_winv = true,
                "--register-ctrl" => args.register_ctrl = true,
                "--register-test" => args.register_test = true,
                "--no-tray" => args.tray = false,
                "--synth-click-after" => {
                    args.synth_click_after = it.next().and_then(|s| s.parse().ok());
                }
                "--log" => {
                    if let Some(v) = it.next() {
                        args.log = PathBuf::from(v);
                    }
                }
                "--ready" => {
                    if let Some(v) = it.next() {
                        args.ready = PathBuf::from(v);
                    }
                }
                "--result" => {
                    if let Some(v) = it.next() {
                        args.result = PathBuf::from(v);
                    }
                }
                other => p0log(&format!("ARGS|忽略未知参数 {other}")),
            }
        }
        args
    }
}

// ---------------------------------------------------------------------------
// 运行状态（跨线程共享，仅用于 UI 展示与结果 JSON）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Status {
    acrylic: String,
    tabbed: String,
    hotkey_ctrl: String,
    hotkey_winv: String,
    hotkey_test: String,
    hotkey_hits: Vec<String>,
    hook: String,
    hook_hits: u32,
    v_down_swallowed: u32,
    v_up_swallowed: u32,
    win_up_swallowed: u32,
    injected_seen: u32,
    tray: String,
    tray_items: u32,
    tray_events: u32,
    menu_events: Vec<String>,
    ready: bool,
    exit_clean: bool,
}

fn status_set(f: impl FnOnce(&mut Status)) {
    if let Some(st) = STATUS.get() {
        if let Ok(mut guard) = st.lock() {
            f(&mut guard);
        }
    }
}

static STATUS: OnceLock<Arc<Mutex<Status>>> = OnceLock::new();

fn status_get() -> Arc<Mutex<Status>> {
    STATUS
        .get_or_init(|| Arc::new(Mutex::new(Status::default())))
        .clone()
}

/// 线程退出用的公共句柄（主线程用它发 WM_CLOSE 结束 Sys 线程消息循环）。
struct SysShared {
    hwnd: AtomicIsize,
}

// ---------------------------------------------------------------------------
// Win+V 低级键盘钩子
// ---------------------------------------------------------------------------

struct HookCtx {
    mode: HookMode,
    test_injected: bool,
    status: Arc<Mutex<Status>>,
    win_down: AtomicBool,
    /// 本轮 Win 按住期间是否吞过 V（用于决定是否补偿吞 Win up）。
    swallowed_v_cycle: AtomicBool,
}

static HOOK_CTX: OnceLock<HookCtx> = OnceLock::new();

fn hook_ctx(mode: HookMode, test_injected: bool) -> &'static HookCtx {
    HOOK_CTX.get_or_init(|| HookCtx {
        mode,
        test_injected,
        status: status_get(),
        win_down: AtomicBool::new(false),
        swallowed_v_cycle: AtomicBool::new(false),
    })
}

unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        if let Some(ctx) = HOOK_CTX.get() {
            let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let injected = (kb.flags & LLKHF_INJECTED) != 0;
            let is_down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let is_up = matches!(wparam as u32, WM_KEYUP | WM_SYSKEYUP);
            let process = !injected || ctx.test_injected;
            if process && (is_down || is_up) {
                if injected {
                    if let Ok(mut s) = ctx.status.lock() {
                        s.injected_seen += 1;
                    }
                    // P0 调试：记录注入事件（正式实现不记录、不处理注入事件）
                    p0log(&format!(
                        "HOOK_EVENT|vk=0x{:02X} {} injected=1 win_down={}",
                        kb.vkCode,
                        if is_down { "down" } else { "up" },
                        ctx.win_down.load(Ordering::SeqCst)
                    ));
                }
                if let Some(swallow) = hook_decide(ctx, kb.vkCode, is_down) {
                    if swallow {
                        return 1; // 吞掉该按键事件
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// 注入一次「Win 抬起」：用于补偿模式清除被吞掉的 Win up 造成的系统键状态粘滞。
fn inject_win_up() {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_LWIN,
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        p0log(&format!(
            "HOOK_COMPENSATE|inject_win_up fail err={}",
            std::io::Error::last_os_error()
        ));
    }
}

/// 返回 Some(true)=吞掉，Some(false)=放行，None=与本策略无关。
fn hook_decide(ctx: &HookCtx, vk: u32, is_down: bool) -> Option<bool> {
    let vk_lwin = VK_LWIN as u32;
    let vk_rwin = VK_RWIN as u32;
    let vk_v = VK_V as u32;

    // Win 键状态跟踪
    if vk == vk_lwin || vk == vk_rwin {
        if is_down {
            ctx.win_down.store(true, Ordering::SeqCst);
            return Some(false);
        }
        ctx.win_down.store(false, Ordering::SeqCst);
        let swallowed_v = ctx.swallowed_v_cycle.swap(false, Ordering::SeqCst);
        if matches!(ctx.mode, HookMode::SwallowVWinUp | HookMode::SwallowVWinUpFix) && swallowed_v {
            // 开始菜单补偿：吞掉 Win up，避免系统看到「干净的一次 Win 点击」
            if let Ok(mut s) = ctx.status.lock() {
                s.win_up_swallowed += 1;
            }
            p0log("HOOK_SWALLOW|win_up（开始菜单补偿）");
            if ctx.mode == HookMode::SwallowVWinUpFix {
                // 补吞后系统异步键状态会停在「Win 按下」；30ms 后注入一次 Win up 清状态。
                // （正式实现应由 Sys 线程用 SetTimer 执行，不在钩子回调里做耗时操作）
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(30));
                    inject_win_up();
                    p0log("HOOK_COMPENSATE|inject_win_up（清除粘住的 Win 键状态）");
                });
            }
            return Some(true);
        }
        return Some(false);
    }

    if ctx.mode != HookMode::Off && vk == vk_v && ctx.win_down.load(Ordering::SeqCst) {
        let swallowed = ctx.swallowed_v_cycle.swap(true, Ordering::SeqCst);
        if let Ok(mut s) = ctx.status.lock() {
            s.hook_hits += 1;
            if is_down {
                s.v_down_swallowed += 1;
            } else {
                s.v_up_swallowed += 1;
            }
        }
        p0log(&format!(
            "HOOK_SWALLOW|v_{}（Win+V 命中）",
            if is_down { "down" } else { "up" }
        ));
        let _ = swallowed;
        return Some(true); // 吞掉 V down/up
    }
    None
}

// ---------------------------------------------------------------------------
// 隐藏消息窗口（热键 + 消息循环）
// ---------------------------------------------------------------------------

unsafe extern "system" fn hidden_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            let id = wparam as i32;
            let label = match id {
                1 => "Ctrl+`",
                2 => "Win+V",
                3 => "Ctrl+Alt+Shift+F10",
                _ => "?",
            };
            p0log(&format!("HOTKEY_HIT|id={id} label={label}"));
            if let Ok(mut s) = status_get().lock() {
                s.hotkey_hits.push(format!("{id}:{label}"));
            }
            0
        }
        WM_CLOSE => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register_hidden_class() -> Vec<u16> {
    let class_name = wide("ClipNestP0Hidden");
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(hidden_wndproc),
            hInstance: hinstance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        // 已注册时返回 0（ERROR_CLASS_ALREADY_EXISTS），忽略即可
        RegisterClassW(&wc);
    }
    class_name
}

fn create_hidden_window(class_name: &[u16]) -> HWND {
    let title = wide("ClipNestP0");
    unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    }
}

// ---------------------------------------------------------------------------
// Sys 线程（headless 时还负责托盘）
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ReadyFlag(Arc<Mutex<bool>>);

fn sys_thread_main(
    args: Arc<Args>,
    shared: Arc<SysShared>,
    ready_tx: mpsc::Sender<()>,
) -> Result<(), String> {
    let class_name = register_hidden_class();
    let hwnd = create_hidden_window(&class_name);
    if hwnd.is_null() {
        return Err(format!(
            "创建隐藏窗口失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    shared.hwnd.store(hwnd as isize, Ordering::SeqCst);
    p0log(&format!("HIDDEN_WINDOW|hwnd={hwnd:?}"));

    // 备用快捷键 Ctrl+`
    if args.register_ctrl {
        let ok = unsafe { RegisterHotKey(hwnd, 1, MOD_CONTROL, VK_OEM_3 as u32) };
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let desc = format!("result={} err={err}", if ok != 0 { "ok" } else { "fail" });
        p0log(&format!("HOTKEY_REG|id=1 label=Ctrl+` {desc}"));
        status_set(|s| s.hotkey_ctrl = desc);
    }

    // L1: 尝试直接注册 Win+V
    if args.register_winv {
        let ok = unsafe { RegisterHotKey(hwnd, 2, MOD_WIN, VK_V as u32) };
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let note = if err == 1409 {
            "（ERROR_HOTKEY_ALREADY_REGISTERED：已被系统/其他程序占用）"
        } else {
            ""
        };
        let desc = format!(
            "result={} err={err}{note}",
            if ok != 0 { "ok" } else { "fail" }
        );
        p0log(&format!("HOTKEY_REG|id=2 label=Win+V {desc}"));
        status_set(|s| s.hotkey_winv = desc);
    }

    // 测试用组合 Ctrl+Alt+Shift+F10：本机 Ctrl+`/Win+V 均被占用时验证热键通路
    if args.register_test {
        let ok = unsafe {
            RegisterHotKey(hwnd, 3, MOD_CONTROL | MOD_ALT | MOD_SHIFT, VK_F10 as u32)
        };
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let desc = format!("result={} err={err}", if ok != 0 { "ok" } else { "fail" });
        p0log(&format!("HOTKEY_REG|id=3 label=Ctrl+Alt+Shift+F10 {desc}"));
        status_set(|s| s.hotkey_test = desc);
    }

    // L2: 低级键盘钩子
    if args.hook != HookMode::Off {
        let ctx = hook_ctx(args.hook, args.test_injected);
        let hhook = unsafe {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), std::ptr::null_mut(), 0)
        };
        let _ = ctx;
        if hhook.is_null() {
            let err = std::io::Error::last_os_error();
            p0log(&format!("HOOK|install=fail err={err}"));
            status_set(|s| s.hook = format!("install_fail:{err}"));
        } else {
            p0log(&format!(
                "HOOK|install=ok mode={} test_injected={}",
                args.hook.as_str(),
                args.test_injected
            ));
            status_set(|s| s.hook = format!("active:{}", args.hook.as_str()));
        }
        // 钩子句柄通过局部变量保存在消息循环作用域内，退出前卸载
        let hook_handle = hhook;

        // headless 模式下托盘也由本线程创建（正式实现即如此）
        let _tray = if args.headless && args.tray {
            match create_tray() {
                Ok(t) => {
                    p0log("TRAY|create=ok thread=sys");
                    status_set(|s| s.tray = "ok:sys".into());
                    Some(t)
                }
                Err(e) => {
                    p0log(&format!("TRAY|create=fail err={e}"));
                    status_set(|s| s.tray = format!("fail:{e}"));
                    None
                }
            }
        } else {
            None
        };

        // 稍等一小会再置 READY，保证钩子消息队列进入泵状态
        let _ = ready_tx.send(());
        p0log("SYS_READY|");

        let mut msg: MSG = unsafe { std::mem::zeroed() };
        unsafe {
            loop {
                let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if r <= 0 {
                    break; // 0=WM_QUIT，-1=错误
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // 清理：卸载钩子、注销热键、销毁窗口
        if !hook_handle.is_null() {
            unsafe { UnhookWindowsHookEx(hook_handle) };
            p0log("CLEANUP|unhook=ok");
        }
        if args.register_ctrl {
            unsafe { UnregisterHotKey(hwnd, 1) };
        }
        if args.register_winv {
            unsafe { UnregisterHotKey(hwnd, 2) };
        }
        if args.register_test {
            unsafe { UnregisterHotKey(hwnd, 3) };
        }
        unsafe { DestroyWindow(hwnd) };
        shared.hwnd.store(0, Ordering::SeqCst);
        p0log("CLEANUP|sys_thread_exit");
        status_set(|s| s.exit_clean = true);
        return Ok(());
    }

    // 无钩子：同样要跑消息循环（热键）
    let _tray = if args.headless && args.tray {
        match create_tray() {
            Ok(t) => {
                p0log("TRAY|create=ok thread=sys");
                status_set(|s| s.tray = "ok:sys".into());
                Some(t)
            }
            Err(e) => {
                p0log(&format!("TRAY|create=fail err={e}"));
                status_set(|s| s.tray = format!("fail:{e}"));
                None
            }
        }
    } else {
        None
    };

    let _ = ready_tx.send(());
    p0log("SYS_READY|");

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe {
        loop {
            let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r <= 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if args.register_ctrl {
        unsafe { UnregisterHotKey(hwnd, 1) };
    }
    if args.register_winv {
        unsafe { UnregisterHotKey(hwnd, 2) };
    }
    if args.register_test {
        unsafe { UnregisterHotKey(hwnd, 3) };
    }
    unsafe { DestroyWindow(hwnd) };
    shared.hwnd.store(0, Ordering::SeqCst);
    p0log("CLEANUP|sys_thread_exit");
    status_set(|s| s.exit_clean = true);
    Ok(())
}

// ---------------------------------------------------------------------------
// 托盘
// ---------------------------------------------------------------------------

/// 安装全局托盘/菜单事件处理器（进程内只需一次）。handler 在系统投递的线程上执行，
/// 这里只做日志与计数，不做业务动作（P0 不接业务）。
fn install_tray_event_handlers() {
    let menu_status = status_get();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let id = event.id().0.clone();
        p0log(&format!("MENU_EVENT|id={id}"));
        if let Ok(mut s) = menu_status.lock() {
            s.menu_events.push(id);
        }
    }));
    let tray_status = status_get();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        p0log(&format!("TRAY_EVENT|{event:?}"));
        if let Ok(mut s) = tray_status.lock() {
            s.tray_events += 1;
        }
    }));
}

fn load_tray_icon() -> Result<Icon, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png");
    match std::fs::read(&path) {
        Ok(bytes) => match image::load_from_memory(&bytes) {
            Ok(img) => {
                let rgba = img.into_rgba8();
                let (w, h) = (rgba.width(), rgba.height());
                Icon::from_rgba(rgba.into_raw(), w, h).map_err(|e| format!("Icon::from_rgba: {e}"))
            }
            Err(e) => {
                p0log(&format!("TRAY|icon decode fail: {e}; 使用内置兜底"));
                fallback_icon()
            }
        },
        Err(e) => {
            p0log(&format!("TRAY|icon read fail: {e}; 使用内置兜底"));
            fallback_icon()
        }
    }
}

/// 内置 16×16 蓝色兜底图标。
fn fallback_icon() -> Result<Icon, String> {
    let mut rgba = vec![0u8; 16 * 16 * 4];
    for px in rgba.chunks_exact_mut(4) {
        px[0] = 110; // R
        px[1] = 139; // G
        px[2] = 255; // B
        px[3] = 255; // A
    }
    Icon::from_rgba(rgba, 16, 16).map_err(|e| format!("fallback Icon::from_rgba: {e}"))
}

fn create_tray() -> Result<TrayIcon, String> {
    let menu = Menu::new();
    let item_open = MenuItem::with_id("open", "打开面板", true, None::<Accelerator>);
    let item_manager = MenuItem::with_id("manager", "管理历史", true, None::<Accelerator>);
    let item_pause = MenuItem::with_id("pause", "暂停记录", true, None::<Accelerator>);
    let item_clear = MenuItem::with_id("clear", "清空历史", true, None::<Accelerator>);
    let item_settings = MenuItem::with_id("settings", "设置", true, None::<Accelerator>);
    let item_autostart =
        CheckMenuItem::with_id("autostart", "开机自启", true, false, None::<Accelerator>);
    let item_quit = MenuItem::with_id("quit", "退出", true, None::<Accelerator>);
    for item in [
        &item_open as &dyn tray_icon::menu::IsMenuItem,
        &item_manager,
        &PredefinedMenuItem::separator(),
        &item_pause,
        &item_clear,
        &PredefinedMenuItem::separator(),
        &item_settings,
        &item_autostart,
        &PredefinedMenuItem::separator(),
        &item_quit,
    ] {
        menu.append(item).map_err(|e| format!("menu.append: {e}"))?;
    }
    let menu_items = 8u32;

    let icon = load_tray_icon()?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("ClipNest P0 spike")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .build()
        .map_err(|e| format!("TrayIconBuilder.build: {e}"))?;
    status_set(|s| s.tray_items = menu_items);
    p0log(&format!("TRAY|menu_items={menu_items}"));
    Ok(tray)
}

/// 模拟一次托盘左键点击：给 tray-icon 的隐藏窗口发一条「Shell 通知」消息。
/// 仅 P0 测试用（真实点击由系统投递同名消息）。
fn post_synthetic_tray_click() {
    let class_name = wide("tray_icon_app");
    let hwnd = unsafe { FindWindowW(class_name.as_ptr(), std::ptr::null()) };
    if hwnd.is_null() {
        p0log("SYNCLICK|tray window not found");
        return;
    }
    // WM_USER_TRAYICON = 6002, wParam=internal_id(第一个托盘图标=1), lParam=WM_LBUTTONUP(0x0202)
    let ok = unsafe { PostMessageW(hwnd, 6002, 1, 0x0202) };
    p0log(&format!("SYNCLICK|hwnd={hwnd:?} ok={ok}"));
}

// ---------------------------------------------------------------------------
// 结果 / READY 文件
// ---------------------------------------------------------------------------

fn write_ready_file(args: &Args) {
    let text = format!(
        "ready_at={}\nhook={}\ntest_injected={}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        args.hook.as_str(),
        args.test_injected
    );
    if let Some(dir) = args.ready.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&args.ready, text);
    p0log(&format!("READY|{}", args.ready.display()));
}

fn write_result(args: &Args) -> std::io::Result<()> {
    let status = status_get();
    let guard = status.lock().unwrap_or_else(|e| e.into_inner());
    let value = serde_json::json!({
        "headless": args.headless,
        "seconds": args.seconds,
        "hook_mode": args.hook.as_str(),
        "test_injected": args.test_injected,
        "acrylic": guard.acrylic,
        "tabbed": guard.tabbed,
        "hotkey_ctrl": guard.hotkey_ctrl,
        "hotkey_winv": guard.hotkey_winv,
        "hotkey_test": guard.hotkey_test,
        "hotkey_hits": guard.hotkey_hits,
        "hook": guard.hook,
        "hook_hits": guard.hook_hits,
        "v_down_swallowed": guard.v_down_swallowed,
        "v_up_swallowed": guard.v_up_swallowed,
        "win_up_swallowed": guard.win_up_swallowed,
        "injected_seen": guard.injected_seen,
        "tray": guard.tray,
        "tray_items": guard.tray_items,
        "tray_events": guard.tray_events,
        "menu_events": guard.menu_events,
        "exit_clean": guard.exit_clean,
        "finished_at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
    });
    if let Some(dir) = args.result.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&args.result, serde_json::to_string_pretty(&value)?)?;
    p0log(&format!("RESULT|{}", args.result.display()));
    Ok(())
}

/// 带超时的线程 join（防止托盘菜单弹窗未关闭导致消息线程卡死、进程不退出）。
fn join_with_timeout(handle: std::thread::JoinHandle<()>, secs: u64, who: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if handle.is_finished() {
            let _ = handle.join();
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    p0log(&format!(
        "FATAL|{who} 线程 join 超时（可能托盘菜单弹窗未关闭），进程直接退出"
    ));
    false
}

fn signal_sys_quit(shared: &SysShared) {
    let hwnd = shared.hwnd.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) };
    }
}

// ---------------------------------------------------------------------------
// headless 模式
// ---------------------------------------------------------------------------

fn run_headless(args: Arc<Args>) -> Result<(), String> {
    let shared = Arc::new(SysShared {
        hwnd: AtomicIsize::new(0),
    });
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let shared2 = shared.clone();
    let args2 = args.clone();
    let handle = std::thread::Builder::new()
        .name("p0-sys".into())
        .spawn(move || {
            if let Err(e) = sys_thread_main(args2, shared2, ready_tx) {
                p0log(&format!("SYS_THREAD_ERROR|{e}"));
            }
        })
        .map_err(|e| format!("spawn sys thread: {e}"))?;

    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| format!("等待 Sys 线程就绪超时: {e}"))?;
    write_ready_file(&args);

    let started = Instant::now();
    if let Some(at) = args.synth_click_after {
        while started.elapsed() < Duration::from_secs(at) {
            std::thread::sleep(Duration::from_millis(50));
        }
        post_synthetic_tray_click();
    }
    while started.elapsed() < Duration::from_secs(args.seconds) {
        std::thread::sleep(Duration::from_millis(50));
    }

    signal_sys_quit(&shared);
    let _ = join_with_timeout(handle, 5, "sys");
    write_result(&args).map_err(|e| format!("写结果文件失败: {e}"))?;
    p0log("EXIT|headless");
    Ok(())
}

// ---------------------------------------------------------------------------
// GUI 模式（eframe 主线程 + 亚克力 + 托盘）
// ---------------------------------------------------------------------------

struct SpikeApp {
    args: Arc<Args>,
    shared: Arc<SysShared>,
    sys_ready_rx: mpsc::Receiver<()>,
    sys_ready: bool,
    started: Instant,
    first_frame: bool,
    tray: Option<TrayIcon>,
    ready_written: bool,
    synth_done: bool,
    closing: bool,
}

impl SpikeApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        args: Arc<Args>,
        shared: Arc<SysShared>,
        sys_ready_rx: mpsc::Receiver<()>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        p0log("GUI|eframe started");
        Self {
            args,
            shared,
            sys_ready_rx,
            sys_ready: false,
            started: Instant::now(),
            first_frame: true,
            tray: None,
            ready_written: false,
            synth_done: false,
            closing: false,
        }
    }

    fn try_apply_effects(&mut self, frame: &mut eframe::Frame) {
        // 亚克力（spec §7 首选）；失败再尝试 tabbed（Lead 已实测本机可用）
        let acrylic = window_vibrancy::apply_acrylic(&*frame, Some((14, 17, 22, 180)));
        let acrylic_desc = match &acrylic {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("fail:{e}"),
        };
        p0log(&format!("ACRYLIC|{acrylic_desc}"));
        let tabbed_desc = if acrylic.is_err() {
            match window_vibrancy::apply_tabbed(&*frame, Some(true)) {
                Ok(()) => "ok".to_string(),
                Err(e) => format!("fail:{e}"),
            }
        } else {
            "skipped".to_string()
        };
        p0log(&format!("TABBED|{tabbed_desc}"));
        status_set(|s| {
            s.acrylic = acrylic_desc;
            s.tabbed = tabbed_desc;
        });
    }

    fn try_create_tray(&mut self) {
        if self.tray.is_some() || !self.args.tray {
            return;
        }
        match create_tray() {
            Ok(t) => {
                p0log("TRAY|create=ok thread=main(eframe)");
                status_set(|s| s.tray = "ok:main".into());
                self.tray = Some(t);
            }
            Err(e) => {
                p0log(&format!("TRAY|create=fail err={e}"));
                status_set(|s| s.tray = format!("fail:{e}"));
                // 失败也标记为已尝试，避免每帧重试
                self.tray = None;
                status_set(|s| {
                    if s.tray.is_empty() {
                        s.tray = "fail:unknown".into();
                    }
                });
            }
        }
    }
}

impl eframe::App for SpikeApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.first_frame {
            self.first_frame = false;
            self.try_apply_effects(frame);
            self.try_create_tray();
        }

        if !self.sys_ready
            && self.sys_ready_rx.try_recv().is_ok() {
                self.sys_ready = true;
                p0log("GUI|sys ready");
            }

        // 所有能力就绪后写 READY 文件（测试脚本以此为准）
        if !self.ready_written && self.sys_ready && (!self.args.tray || self.tray.is_some()) {
            self.ready_written = true;
            write_ready_file(&self.args);
        }

        // 模拟托盘左键点击
        if let Some(at) = self.args.synth_click_after {
            if !self.synth_done && self.started.elapsed() >= Duration::from_secs(at) {
                self.synth_done = true;
                post_synthetic_tray_click();
            }
        }

        // 到时退出
        if !self.closing && self.ready_written && self.started.elapsed() >= Duration::from_secs(self.args.seconds + 1) {
            self.closing = true;
            if let Some(tray) = self.tray.take() {
                drop(tray);
                p0log("CLEANUP|tray removed");
            }
            signal_sys_quit(&self.shared);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let status = status_get();
        let guard = status.lock().unwrap_or_else(|e| e.into_inner());
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgba_unmultiplied(14, 17, 22, 210))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.heading(
                    egui::RichText::new("ClipNest P0 spike")
                        .color(egui::Color32::from_rgb(230, 237, 243)),
                );
                ui.add_space(6.0);
                let rows = [
                    ("亚克力 acrylic", guard.acrylic.clone()),
                    ("tabbed 回退", guard.tabbed.clone()),
                    ("热键 Ctrl+`", guard.hotkey_ctrl.clone()),
                    ("热键 Win+V (L1)", guard.hotkey_winv.clone()),
                    ("热键测试组合", guard.hotkey_test.clone()),
                    ("钩子模式", format!("{} · {}", guard.hook, self.args.hook.as_str())),
                    (
                        "钩子命中 / 注入事件",
                        format!("{} / {}", guard.hook_hits, guard.injected_seen),
                    ),
                    (
                        "吞 V down / up / Win up",
                        format!(
                            "{} / {} / {}",
                            guard.v_down_swallowed, guard.v_up_swallowed, guard.win_up_swallowed
                        ),
                    ),
                    ("托盘", format!("{}（{} 项）", guard.tray, guard.tray_items)),
                    (
                        "托盘事件 / 菜单事件",
                        format!("{} / {}", guard.tray_events, guard.menu_events.join(",")),
                    ),
                    (
                        "运行时间",
                        format!("{:.1}s / {}s", self.started.elapsed().as_secs_f64(), self.args.seconds),
                    ),
                ];
                for (k, v) in rows {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{k}："))
                                .color(egui::Color32::from_rgb(139, 152, 169)),
                        );
                        ui.label(
                            egui::RichText::new(v).color(egui::Color32::from_rgb(230, 237, 243)),
                        );
                    });
                }
            });
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

fn run_gui(args: Arc<Args>) -> Result<(), String> {
    let shared = Arc::new(SysShared {
        hwnd: AtomicIsize::new(0),
    });
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let shared2 = shared.clone();
    let args2 = args.clone();
    let handle = std::thread::Builder::new()
        .name("p0-sys".into())
        .spawn(move || {
            if let Err(e) = sys_thread_main(args2, shared2, ready_tx) {
                p0log(&format!("SYS_THREAD_ERROR|{e}"));
            }
        })
        .map_err(|e| format!("spawn sys thread: {e}"))?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 420.0])
            .with_position([90.0, 90.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_title("ClipNest P0 spike")
            .with_active(true),
        ..Default::default()
    };
    let args3 = args.clone();
    let shared3 = shared.clone();
    let run_result = eframe::run_native(
        "p0_spike",
        options,
        Box::new(move |cc| {
            Ok(Box::new(SpikeApp::new(cc, args3, shared3, ready_rx)))
        }),
    );

    signal_sys_quit(&shared);
    let _ = join_with_timeout(handle, 5, "sys");
    write_result(&args).map_err(|e| format!("写结果文件失败: {e}"))?;
    p0log("EXIT|gui");
    run_result.map_err(|e| format!("eframe run_native: {e}"))
}

// ---------------------------------------------------------------------------

fn main() {
    let args = Arc::new(Args::parse());

    // 初始化日志文件
    if let Some(dir) = args.log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&args.log)
    {
        Ok(f) => {
            let _ = LOG_FILE.set(Mutex::new(f));
        }
        Err(e) => eprintln!("[p0] 打开日志文件失败: {e}"),
    }
    // 清掉上一轮的 READY 文件，避免测试脚本误判
    let _ = std::fs::remove_file(&args.ready);

    let _ = status_get();
    install_tray_event_handlers();
    p0log(&format!(
        "START|headless={} seconds={} hook={} test_injected={} register_ctrl={} register_winv={} register_test={} tray={}",
        args.headless,
        args.seconds,
        args.hook.as_str(),
        args.test_injected,
        args.register_ctrl,
        args.register_winv,
        args.register_test,
        args.tray
    ));

    let result = if args.headless {
        run_headless(args.clone())
    } else {
        run_gui(args.clone())
    };
    if let Err(e) = result {
        p0log(&format!("FATAL|{e}"));
        std::process::exit(2);
    }
    std::process::exit(0);
}

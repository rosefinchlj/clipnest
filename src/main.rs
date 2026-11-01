//! ClipNest 入口：命令分发。
//!
//! 进程模型（低内存关键）：
//! - **默认（无参数）= 守护进程**：托盘/全局热键/剪贴板监听/SQLite 采集，不创建 GUI 窗口；
//! - `--ui-picker --target <hwnd>` / `--ui-manager --page <page>`：由守护进程按需 spawn 的
//!   UI 子进程（eframe 窗口），关闭后退出、内存全部释放；
//! - `--selftest` / `--diagnose` / `--demo-seed` / `--ui-preview`：诊断与开发用。
//!
//! release 为 Windows GUI 子系统（双击不弹控制台）；CLI 模式会 AttachConsole 到调用方终端。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clipboard;
mod config;
mod daemon;
mod diag;
mod logging;
mod model;
mod store;
mod sys;
mod ui;
mod window;

use eframe::egui;
use ui::ManagerPage;

enum Mode {
    Daemon,
    UiPicker { target_hwnd: isize },
    UiManager { page: ManagerPage },
    UiPreview(String),
    Selftest(String),
    Diagnose(String),
    DemoSeed { count: usize, confirmed: bool },
    Help,
    Version,
}

fn parse_args() -> Mode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Mode::Help,
            "--version" | "-V" => return Mode::Version,
            "--ui-picker" => {
                let mut target = 0isize;
                if let Some(value) = args.get(i + 1) {
                    if value == "--target" {
                        target = args
                            .get(i + 2)
                            .and_then(|s| s.parse::<isize>().ok())
                            .unwrap_or(0);
                    } else if let Ok(parsed) = value.parse::<isize>() {
                        target = parsed;
                    }
                }
                return Mode::UiPicker {
                    target_hwnd: target,
                };
            }
            "--ui-manager" => {
                let page = args
                    .get(i + 1)
                    .filter(|s| !s.starts_with("--"))
                    .map(|s| parse_page(s))
                    .unwrap_or(ManagerPage::History);
                if args.get(i + 1).map(|s| s == "--page").unwrap_or(false) {
                    let page = args
                        .get(i + 2)
                        .map(|s| parse_page(s))
                        .unwrap_or(ManagerPage::History);
                    return Mode::UiManager { page };
                }
                return Mode::UiManager { page };
            }
            "--selftest" => {
                let path = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| "selftest.json".to_string());
                return Mode::Selftest(path);
            }
            "--diagnose" => {
                let path = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| "diagnose.json".to_string());
                return Mode::Diagnose(path);
            }
            "--demo-seed" => {
                let count = args
                    .get(i + 1)
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(200);
                let confirmed = args.iter().any(|a| a == "--yes" || a == "-y");
                return Mode::DemoSeed { count, confirmed };
            }
            "--ui-preview" => {
                let mode = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| "picker".to_string());
                return Mode::UiPreview(mode);
            }
            other => eprintln!("未知参数：{other}（使用 --help 查看用法）"),
        }
        i += 1;
    }
    Mode::Daemon
}

fn parse_page(value: &str) -> ManagerPage {
    match value {
        "favorites" | "收藏" => ManagerPage::Favorites,
        "settings" | "设置" => ManagerPage::Settings,
        "about" | "关于" => ManagerPage::About,
        _ => ManagerPage::History,
    }
}

fn print_help() {
    println!(
        "ClipNest 剪贴匣 {}\n\n\
用法:\n  \
clipnest.exe                             启动守护进程（托盘常驻，无窗口）\n  \
clipnest.exe --ui-picker --target HWND   打开候选窗（内部使用）\n  \
clipnest.exe --ui-manager [--page P]     打开管理台（内部使用；P=history/favorites/settings/about）\n  \
clipnest.exe --selftest [out.json]       无界面全链路自检\n  \
clipnest.exe --diagnose [out.json]       环境与能力诊断\n  \
clipnest.exe --demo-seed N --yes         写入 N 条演示数据\n  \
clipnest.exe --ui-preview <mode>         UI 预览（picker/manager/favorites/settings/about）\n  \
clipnest.exe --version                   显示版本\n  \
clipnest.exe --help                      显示本帮助\n\n\
数据目录: %APPDATA%\\ClipNest（可用 CLIPNEST_DATA_DIR 覆盖）\n\
默认面板 {}×{}，备用快捷键 Ctrl+`（冲突时自动降级），Win+V 自动接管。",
        env!("CARGO_PKG_VERSION"),
        model::Settings::default().picker_size[0] as u32,
        model::Settings::default().picker_size[1] as u32,
    );
}

/// 单实例：命名互斥体。返回 true 表示本进程是第一个实例（可继续启动）；
/// false 表示已有实例，调用方应尝试唤起它并退出。
///
/// 互斥体句柄故意不关闭（进程生命周期内有效，退出时系统回收）。
fn ensure_single_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\ClipNest.SingleInstance\0".encode_utf16().collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return true;
    }
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    !already_exists
}

/// release 为 GUI 子系统（无控制台）；CLI 模式尝试附加到父进程控制台，让输出可见。
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() {
    let mode = parse_args();
    // 只有纯 CLI 模式需要控制台；GUI 模式（守护进程/子进程/预览）不附加。
    if matches!(
        mode,
        Mode::Selftest(_) | Mode::Diagnose(_) | Mode::DemoSeed { .. } | Mode::Help | Mode::Version
    ) {
        attach_parent_console();
    }

    match mode {
        Mode::Help => print_help(),
        Mode::Version => println!("ClipNest {}", env!("CARGO_PKG_VERSION")),
        Mode::Selftest(path) => {
            let code = match diag::selftest(&path) {
                Ok(report) => {
                    println!("selftest: PASS → {report}");
                    0
                }
                Err(err) => {
                    eprintln!("selftest: FAIL — {err}");
                    1
                }
            };
            std::process::exit(code);
        }
        Mode::Diagnose(path) => {
            let code = match diag::diagnose(&path) {
                Ok(report) => {
                    println!("diagnose: OK → {report}");
                    0
                }
                Err(err) => {
                    eprintln!("diagnose: FAIL — {err}");
                    1
                }
            };
            std::process::exit(code);
        }
        Mode::DemoSeed { count, confirmed } => {
            let code = match diag::demo_seed(count, confirmed) {
                Ok(msg) => {
                    println!("{msg}");
                    0
                }
                Err(err) => {
                    eprintln!("demo-seed: FAIL — {err}");
                    1
                }
            };
            std::process::exit(code);
        }
        Mode::UiPreview(preview) => {
            if let Err(err) = app::run_preview(preview) {
                eprintln!("UI 预览启动失败: {err}");
                std::process::exit(1);
            }
        }
        Mode::UiPicker { target_hwnd } => {
            let opts = app::UiSessionOptions {
                kind: daemon::UiKind::Picker,
                target_hwnd,
                page: ManagerPage::History,
            };
            if let Err(err) = app::run_ui_session(opts) {
                eprintln!("候选窗启动失败: {err}");
                std::process::exit(1);
            }
        }
        Mode::UiManager { page } => {
            let opts = app::UiSessionOptions {
                kind: daemon::UiKind::Manager,
                target_hwnd: 0,
                page,
            };
            if let Err(err) = app::run_ui_session(opts) {
                eprintln!("管理台启动失败: {err}");
                std::process::exit(1);
            }
        }
        Mode::Daemon => {
            if !ensure_single_instance() {
                logging::init(&config::logs_dir(), "info");
                logging::log("检测到已有 ClipNest 实例，尝试唤起后退出");
                let activated = sys::activate_existing_instance();
                eprintln!(
                    "ClipNest 已在运行（{}），本实例退出",
                    if activated {
                        "已唤起现有窗口"
                    } else {
                        "唤起现有实例失败"
                    }
                );
                return;
            }
            if let Err(err) = daemon::run() {
                logging::error(&format!("守护进程启动失败：{err}"));
                eprintln!("ClipNest 启动失败: {err}");
                std::process::exit(1);
            }
        }
    }
}

/// 保留给其它模块使用（避免 `use eframe::egui` 未使用告警）。
#[allow(dead_code)]
fn _egui_marker() -> egui::Vec2 {
    egui::Vec2::ZERO
}

//! ClipNest 入口：命令行分发 + 正常 GUI 启动。
//!
//! 命令行（spec 附录 C）：
//! ```text
//! clipnest.exe                      正常启动
//! clipnest.exe --selftest out.json  无界面全链路自检
//! clipnest.exe --diagnose out.json  环境与能力诊断
//! clipnest.exe --demo-seed 300 --yes 写入演示数据
//! clipnest.exe --ui-preview picker  仅 UI 预览（开发/截图用）
//! clipnest.exe --version / --help
//! ```

// release 为 Windows GUI 子系统：从资源管理器双击不会弹控制台；
// CLI 模式在 main 里 AttachConsole 到调用方终端以保留输出（见 attach_parent_console）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clipboard;
mod config;
mod db_crypto;
mod diag;
mod logging;
mod manager_window;
mod model;
mod store;
mod sys;
mod ui;
mod window;

use eframe::egui;

enum Mode {
    Normal,
    UiPreview(String),
    UiManager {
        page: ui::ManagerPage,
        target: isize,
    },
    Selftest(String),
    Diagnose(String),
    DemoSeed {
        count: usize,
        confirmed: bool,
    },
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
            "--ui-manager" => {
                let mut page = ui::ManagerPage::History;
                let mut target = 0isize;
                let mut j = i + 1;
                while j < args.len() {
                    match args[j].as_str() {
                        "--page" => {
                            if let Some(value) = args.get(j + 1) {
                                page = match value.as_str() {
                                    "favorites" => ui::ManagerPage::Favorites,
                                    "trash" => ui::ManagerPage::Trash,
                                    "settings" => ui::ManagerPage::Settings,
                                    "about" => ui::ManagerPage::About,
                                    _ => ui::ManagerPage::History,
                                };
                                j += 1;
                            }
                        }
                        "--target" => {
                            if let Some(value) = args.get(j + 1) {
                                target = value.parse::<isize>().unwrap_or(0);
                                j += 1;
                            }
                        }
                        other if !other.starts_with("--") => {
                            page = match other {
                                "favorites" => ui::ManagerPage::Favorites,
                                "trash" => ui::ManagerPage::Trash,
                                "settings" => ui::ManagerPage::Settings,
                                "about" => ui::ManagerPage::About,
                                _ => ui::ManagerPage::History,
                            };
                        }
                        _ => break,
                    }
                    j += 1;
                }
                return Mode::UiManager { page, target };
            }
            "--ui-preview" => {
                let mode = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| "picker".to_string());
                return Mode::UiPreview(mode);
            }
            other => {
                eprintln!("未知参数：{other}（使用 --help 查看用法）");
            }
        }
        i += 1;
    }
    Mode::Normal
}

fn print_help() {
    println!(
        "ClipNest 剪贴匣 {}\n\n\
用法:\n  \
clipnest.exe                        正常启动（托盘常驻）\n  \
clipnest.exe --selftest [out.json]  无界面全链路自检\n  \
clipnest.exe --diagnose [out.json]  环境与能力诊断\n  \
clipnest.exe --demo-seed N --yes    写入 N 条演示数据\n  \
clipnest.exe --ui-manager [--page P] [--target HWND]  管理台独立窗口（内部使用）\n  \
clipnest.exe --ui-preview <mode>    UI 预览（picker / manager）\n  \
clipnest.exe --version              显示版本\n  \
clipnest.exe --help                 显示本帮助\n\n\
数据目录: %APPDATA%\\ClipNest（可用 CLIPNEST_DATA_DIR 覆盖）\n\
默认面板 {}×{}，快捷键 Ctrl+`（可在设置中修改，立即生效；v1.2 起不再接管 Win+V）。",
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
        // 无法创建互斥体时允许启动，避免因权限等问题打不开程序。
        return true;
    }
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    !already_exists
}

/// release 为 GUI 子系统（无控制台）；CLI 模式尝试附加到父进程控制台，让输出可见。
/// 必须在任何 stdout/stderr 输出之前调用（Rust 的 stdout 句柄是惰性获取的）。
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() {
    let mode = parse_args();
    if !matches!(mode, Mode::Normal | Mode::UiManager { .. }) {
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
        Mode::UiManager { page, target } => {
            if let Err(err) = manager_window::run(page, target) {
                eprintln!("管理台窗口启动失败: {err}");
                std::process::exit(1);
            }
        }
        Mode::UiPreview(preview) => {
            if let Err(err) = run_native(Some(preview)) {
                eprintln!("UI 预览启动失败: {err}");
                std::process::exit(1);
            }
        }
        Mode::Normal => {
            if !ensure_single_instance() {
                logging::init(&config::logs_dir(), "info");
                logging::log("检测到已有 ClipNest 实例，尝试唤起后退出");
                let activated = sys::activate_existing_instance();
                // A5：不静默退出 —— 用系统消息框明确告知用户，并提示旧版本如何重启。
                let version = env!("CARGO_PKG_VERSION");
                let text = if activated {
                    format!(
                        "ClipNest 已在运行（v{version}）。\n\n已唤起现有窗口。\n如果界面看起来是旧版本，请从托盘菜单退出 ClipNest 后重新启动新版本。"
                    )
                } else {
                    format!(
                        "ClipNest 已在运行（v{version}），但唤起现有窗口失败。\n\n请从托盘图标退出 ClipNest 后重新启动；若仍失败，请检查任务管理器中是否有残留的 ClipNest 进程。"
                    )
                };
                sys::util::message_box("ClipNest · 剪贴匣", &text, true);
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
            if let Err(err) = run_native(None) {
                eprintln!("ClipNest 启动失败: {err}");
                std::process::exit(1);
            }
        }
    }
}

fn run_native(preview: Option<String>) -> eframe::Result<()> {
    let settings = config::load_settings();
    let _ = config::ensure_dirs();
    logging::init(&config::logs_dir(), &settings.log_level);
    logging::log(&format!(
        "ClipNest {} 启动（preview={preview:?}，数据目录={})",
        env!("CARGO_PKG_VERSION"),
        config::data_dir().display()
    ));

    let is_picker_preview = preview.as_deref() == Some("picker");
    let size = if preview.is_some() {
        [1100.0, 720.0]
    } else {
        settings.picker_size
    };

    // v1.2：回到单进程常驻方案——窗口在启动时就创建（OpenGL/字体已就绪，面板秒开），
    // 但先放在屏幕外；收到快捷键/托盘唤醒时再移回光标屏幕并抢前台。
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(if is_picker_preview {
            [660.0, 620.0]
        } else {
            size
        })
        .with_title(model::APP_TITLE)
        .with_transparent(true)
        .with_visible(true);
    if preview.is_none() {
        viewport = viewport
            .with_position([-30000.0, -30000.0])
            // 屏幕外窗口不能抢焦点（否则启动瞬间会把用户当前窗口的前台抢走）。
            .with_active(false)
            .with_decorations(false)
            .with_taskbar(false)
            .with_always_on_top()
            // v1.11：候选窗允许拖动边缘缩放，尺寸写回 settings.picker_size。
            .with_resizable(true);
    } else if is_picker_preview {
        viewport = viewport
            .with_decorations(false)
            .with_taskbar(false)
            .with_resizable(false);
    }

    if let Some(icon) = window::app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        model::APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::ClipNestApp::new(cc, preview)))),
    )
}

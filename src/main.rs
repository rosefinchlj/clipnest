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

mod app;
mod clipboard;
mod config;
mod diag;
mod logging;
mod model;
mod store;
mod sys;
mod ui;

use eframe::egui;

enum Mode {
    Normal,
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
clipnest.exe --ui-preview <mode>    UI 预览（picker / manager）\n  \
clipnest.exe --version              显示版本\n  \
clipnest.exe --help                 显示本帮助\n\n\
数据目录: %APPDATA%\\ClipNest（可用 CLIPNEST_DATA_DIR 覆盖）\n\
默认面板 {}×{}，备用快捷键 Ctrl+`，Win+V 自动接管。",
        env!("CARGO_PKG_VERSION"),
        model::Settings::default().picker_size[0] as u32,
        model::Settings::default().picker_size[1] as u32,
    );
}

fn main() {
    let mode = parse_args();

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
            if let Err(err) = run_native(Some(preview)) {
                eprintln!("UI 预览启动失败: {err}");
                std::process::exit(1);
            }
        }
        Mode::Normal => {
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

    let size = if preview.is_some() {
        [1100.0, 720.0]
    } else {
        settings.picker_size
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([320.0, 240.0])
            .with_title(model::APP_TITLE)
            .with_transparent(true)
            .with_decorations(true),
        ..Default::default()
    };

    eframe::run_native(
        model::APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::ClipNestApp::new(cc, preview)))),
    )
}

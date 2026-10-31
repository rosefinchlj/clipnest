//! 轻量日志：写入 `%APPDATA%\ClipNest\logs\app.log`，超过 1MB 轮转一份。
//!
//! 设计原则：
//! - 初始化失败绝不影响主流程（仅 stderr 提示）；
//! - 永不记录剪贴板内容全文，只记录类型/大小/哈希前缀（由调用方保证）；
//! - 单文件轮转，最多保留 3 份，避免长期运行占满磁盘。

use chrono::Local;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }

    pub fn parse(s: &str) -> Level {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" => Level::Debug,
            _ => Level::Info,
        }
    }
}

struct State {
    file: Option<File>,
    path: Option<PathBuf>,
    level: Level,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| {
        Mutex::new(State {
            file: None,
            path: None,
            level: Level::Info,
        })
    })
}

/// 初始化日志（幂等）：`logs_dir` 通常为 `config::logs_dir()`。
pub fn init(logs_dir: &Path, level: &str) {
    let level = Level::parse(level);
    let path = logs_dir.join("app.log");
    if let Err(err) = fs::create_dir_all(logs_dir) {
        eprintln!("[ClipNest] 创建日志目录失败: {err}");
        let _ = state().lock().map(|mut g| {
            g.level = level;
        });
        return;
    }
    let _ = rotate_if_needed(&path);
    let file = OpenOptions::new().create(true).append(true).open(&path);
    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    guard.level = level;
    match file {
        Ok(f) => {
            guard.file = Some(f);
            guard.path = Some(path);
        }
        Err(err) => eprintln!("[ClipNest] 打开日志文件失败: {err}"),
    }
}

fn rotate_if_needed(path: &Path) -> std::io::Result<()> {
    const MAX: u64 = 1024 * 1024;
    let Ok(meta) = fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= MAX {
        return Ok(());
    }
    for i in (1..=3).rev() {
        let src = if i == 1 {
            path.to_path_buf()
        } else {
            path.with_extension(format!("log.{}", i - 1))
        };
        let dst = path.with_extension(format!("log.{i}"));
        if src.exists() {
            if i == 3 && dst.exists() {
                let _ = fs::remove_file(&dst);
            }
            let _ = fs::rename(&src, &dst);
        }
    }
    Ok(())
}

pub fn log(msg: &str) {
    write(Level::Info, msg);
}

pub fn warn(msg: &str) {
    write(Level::Warn, msg);
}

pub fn error(msg: &str) {
    write(Level::Error, msg);
}

pub fn debug(msg: &str) {
    write(Level::Debug, msg);
}

fn write(level: Level, msg: &str) {
    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if level > guard.level {
        return;
    }
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("[{ts}] [{level}] {msg}\n", level = level.as_str());
    if cfg!(debug_assertions) {
        eprint!("{line}");
    }
    if let Some(file) = guard.file.as_mut() {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// 当前日志文件路径（诊断用）。
pub fn path() -> Option<PathBuf> {
    state().lock().ok().and_then(|g| g.path.clone())
}

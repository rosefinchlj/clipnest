//! 命令行诊断：`--selftest`、`--diagnose`、`--demo-seed`。
//!
//! 原则：诊断只读、不写用户数据；自检/演示数据必须显式指定/确认。
//! `--diagnose` 在 P0 阶段已可用；`--selftest`/`--demo-seed` 依赖 Store 域，稍后接入。

use serde_json::json;
use std::path::Path;

/// 无界面全链路自检（隔离临时目录，绝不碰真实数据）。
pub fn selftest(out_path: &str) -> Result<String, String> {
    let _ = out_path;
    Err("--selftest 尚未接入 Store 域（等待 store milestone）".to_string())
}

/// 环境与能力诊断：OS/路径/SQLite FTS5/窗口材质/数据统计。
pub fn diagnose(out_path: &str) -> Result<String, String> {
    crate::logging::init(&crate::config::logs_dir(), "info");
    let settings = crate::config::load_settings();
    let data_dir = crate::config::data_dir();
    let db = crate::config::db_path();
    let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    let (major, build) = crate::window::os_version();

    let report = json!({
        "app": {
            "name": crate::model::APP_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "arch": std::env::consts::ARCH,
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "os": {
            "platform": std::env::consts::OS,
            "version_major": major,
            "build": build,
            "is_windows_11": build >= 22000,
        },
        "paths": {
            "data_dir": data_dir.display().to_string(),
            "db": db.display().to_string(),
            "logs": crate::config::logs_dir().display().to_string(),
            "db_bytes": db_bytes,
            "settings_exists": crate::config::settings_path().exists(),
        },
        "capabilities": {
            "sqlite_version": rusqlite::version(),
            "fts5_trigram": fts5_trigram_available(),
            "dwm_mica_tabbed": crate::window::supports_mica(),
        },
        "settings": settings,
        "log": crate::logging::path().map(|p| p.display().to_string()),
    });

    write_json(out_path, &report)?;
    Ok(out_path.to_string())
}

/// 写入演示数据（需要 `--yes` 确认）。
pub fn demo_seed(count: usize, confirmed: bool) -> Result<String, String> {
    if !confirmed {
        return Err("写入演示数据会修改真实数据库，请加 --yes 确认".to_string());
    }
    Ok(format!(
        "demo-seed 尚未接入 Store 域（请求 {count} 条，等待 store milestone）"
    ))
}

fn write_json(path: &str, value: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| format!("序列化诊断结果失败: {e}"))?;
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    std::fs::write(path, text).map_err(|e| format!("写诊断文件失败: {e}"))?;
    Ok(())
}

/// 运行时探测 FTS5 + trigram tokenizer 是否可用（内存库，不碰用户数据）。
pub fn fts5_trigram_available() -> bool {
    let Ok(conn) = rusqlite::Connection::open_in_memory() else {
        return false;
    };
    conn.execute_batch(
        "CREATE VIRTUAL TABLE probe USING fts5(body, tokenize='trigram');
         INSERT INTO probe(body) VALUES ('clipnest 中文搜索 probe');
         SELECT * FROM probe WHERE probe MATCH '中文';",
    )
    .is_ok()
}

//! 路径与设置持久化。
//!
//! 数据目录默认 `%APPDATA%\ClipNest`，可用环境变量 `CLIPNEST_DATA_DIR` 覆盖
//! （自动化测试与 `--selftest` 使用临时目录，绝不碰用户真实数据）。

use crate::model::Settings;
use std::fs;
use std::path::PathBuf;

/// 数据根目录。
pub fn data_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("CLIPNEST_DATA_DIR") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if cfg!(windows) {
        match std::env::var_os("APPDATA") {
            Some(base) => PathBuf::from(base).join("ClipNest"),
            None => PathBuf::from(".").join("ClipNestData"),
        }
    } else {
        PathBuf::from("./clipnest-data")
    }
}

pub fn db_path() -> PathBuf {
    data_dir().join("data.db")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn blobs_dir() -> PathBuf {
    data_dir().join("blobs")
}

pub fn thumbs_dir() -> PathBuf {
    blobs_dir().join("thumbs")
}

pub fn restored_dir() -> PathBuf {
    data_dir().join("restored")
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

/// 创建所有需要的目录（幂等）。
pub fn ensure_dirs() -> std::io::Result<()> {
    for dir in [
        data_dir(),
        logs_dir(),
        blobs_dir(),
        blobs_dir().join("images"),
        blobs_dir().join("files"),
        thumbs_dir(),
        restored_dir(),
    ] {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// 读取设置；文件缺失/损坏时返回默认值，损坏文件备份为 `settings.json.bad`。
pub fn load_settings() -> Settings {
    let path = settings_path();
    let Ok(raw) = fs::read_to_string(&path) else {
        return Settings::default();
    };
    match serde_json::from_str::<Settings>(&raw) {
        Ok(s) => s,
        Err(err) => {
            crate::logging::warn(&format!("settings.json 解析失败（{err}），使用默认设置"));
            let _ = fs::copy(&path, path.with_extension("json.bad"));
            Settings::default()
        }
    }
}

/// 原子写设置：先写 `.tmp`，再 rename 覆盖。
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    ensure_dirs().map_err(|e| format!("创建数据目录失败: {e}"))?;
    let path = settings_path();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(settings).map_err(|e| format!("序列化设置失败: {e}"))?;
    fs::write(&tmp, json).map_err(|e| format!("写临时设置失败: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("替换设置文件失败: {e}"))?;
    Ok(())
}

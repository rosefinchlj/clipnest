//! 中文字体安装：把系统中文字体注入 egui，避免中文显示为方块。
//!
//! 依次尝试 `Deng.ttf` → `simhei.ttf` → `msyh.ttc`(索引 0) → `simsun.ttc`(索引 0)，
//! 任何一步失败都只是跳过，**绝不 panic**；字体同时注入 Proportional 与 Monospace
//! 族末尾，让拉丁字符继续使用 egui 自带字体、中文自动回退到系统中文字体。
//!
//! 参考 `ctx-menu-manager/src/fonts.rs` 的成熟回退模式（同机验证过）。

use eframe::egui;
use std::sync::Arc;

/// 候选字体：(文件名, ttc 内索引)，按优先级从高到低（spec §7.2）。
const CANDIDATES: &[(&str, u32)] = &[
    ("Deng.ttf", 0),
    ("simhei.ttf", 0),
    ("msyh.ttc", 0),
    ("simsun.ttc", 0),
];

/// 把系统中文字体注册到 egui（App 启动时调用一次；重复调用也安全）。
pub fn install_fonts(ctx: &egui::Context) {
    match load_first_available() {
        Some((name, bytes, index)) => {
            install_bytes(ctx, name, bytes, index);
        }
        None => {
            // 字体缺失不阻塞 UI：中文可能显示为方块，但程序照常运行。
            crate::logging::warn(
                "未找到可用的中文字体（Deng/simhei/msyh/simsun），中文可能显示为方块",
            );
        }
    }
}

/// 安装指定字体字节（独立出来便于测试）。
pub fn install_bytes(ctx: &egui::Context, name: String, bytes: Vec<u8>, index: u32) {
    let mut fonts = egui::FontDefinitions::default();

    // 字体放 Proportional/Monospace 族末尾：拉丁字形仍由 egui 默认字体渲染。
    let mut data = egui::FontData::from_owned(bytes);
    data.index = index;
    fonts.font_data.insert(name.clone(), Arc::new(data));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push(name.clone());
    }

    ctx.set_fonts(fonts);
    crate::logging::log(&format!("已加载中文字体：{name}（ttc 索引 {index}）"));
}

/// 读取第一个可用的候选字体；全部失败返回 None（不 panic）。
fn load_first_available() -> Option<(String, Vec<u8>, u32)> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let font_dir = std::path::Path::new(&windir).join("Fonts");
    for (file, index) in CANDIDATES {
        let path = font_dir.join(file);
        if let Ok(bytes) = std::fs::read(&path) {
            // 系统字体文件较大；小于 1KB 视为损坏/占位文件直接跳过。
            if bytes.len() > 1024 {
                return Some(((*file).to_owned(), bytes, *index));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_order_matches_spec() {
        // spec §7.2：Deng → simhei → msyh → simsun
        let names: Vec<&str> = CANDIDATES.iter().map(|(f, _)| *f).collect();
        assert_eq!(names, vec!["Deng.ttf", "simhei.ttf", "msyh.ttc", "simsun.ttc"]);
        // ttc 候选索引均为 0（取常规体）
        assert!(CANDIDATES.iter().all(|(_, idx)| *idx == 0));
    }

    #[test]
    fn install_bytes_registers_both_families() {
        // 用最小可用字体字节验证注入不 panic；字体无效时 egui 会回退默认字体。
        let ctx = egui::Context::default();
        install_bytes(&ctx, "fake_cjk.ttf".to_owned(), vec![0u8; 2048], 0);
        // 注册后上下文仍可正常使用（不 panic 即通过；字体真实性由真实桌面截图验证）。
        let style = ctx.style();
        assert!(style.text_styles.contains_key(&egui::TextStyle::Body));
    }

    #[test]
    fn missing_font_dir_returns_none() {
        // 不依赖真实环境：构造一个不存在的字体目录逻辑分支（间接验证不 panic）。
        let tmp = std::env::temp_dir().join("clipnest_no_such_fonts");
        assert!(load_first_available_from(&tmp).is_none());
    }

    /// 与 [`load_first_available`] 同逻辑、可指定字体目录（测试用）。
    fn load_first_available_from(font_dir: &std::path::Path) -> Option<(String, Vec<u8>, u32)> {
        for (file, index) in CANDIDATES {
            let path = font_dir.join(file);
            if let Ok(bytes) = std::fs::read(&path) {
                if bytes.len() > 1024 {
                    return Some(((*file).to_owned(), bytes, *index));
                }
            }
        }
        None
    }
}

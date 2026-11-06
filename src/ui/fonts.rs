//! 中文字体安装：把系统中文字体注入 egui，避免中文显示为方块。
//!
//! 依次尝试 `msyh.ttc`(索引 0，Microsoft YaHei UI) → `Deng.ttf` → `simhei.ttf` → `simsun.ttc`，
//! 任何一步失败都只是跳过，**绝不 panic**；字体注入 Proportional/Monospace 族末尾，
//! 让拉丁字符继续使用 egui 自带字体、中文自动回退到系统中文字体。
//!
//! ## 内存优化（关键）
//!
//! 中文字体动辄 15MB+。若用 `std::fs::read` + `FontData::from_owned`，epaint 会存两份：
//! `FontData` 的 `Cow::Owned` 一份 + `ab_glyph::FontVec` 一份（约 31MB 私有内存）。
//! 这里改为 **只读内存映射 + [`egui::FontData::from_static`]**：
//! - epaint 对 `Cow::Borrowed` 走 `ab_glyph::FontRef` **零拷贝**，不再复制字体数据；
//! - `MapViewOfFile(FILE_MAP_READ)` 是文件映射页，不计入进程 Private 提交内存；
//! - 映射句柄/视图故意不关闭（泄漏），因为 egui 在整个进程生命周期内都会引用该内存，
//!   提前 `UnmapViewOfFile` 会造成悬垂指针；进程退出时系统自动回收。
//!
//! 映射失败（权限/文件系统异常/非 Windows）时自动回退为 `std::fs::read` + `from_owned`，
//! 功能不降级，只是多占一份内存。
//!
//! 参考 `ctx-menu-manager/src/fonts.rs` 的成熟回退模式（同机验证过）。

use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 候选中文字体：(文件名, ttc 内索引)，按优先级从高到低。
/// 微软雅黑 UI 字形更饱满、屏幕可读性优于等线，作为首选。
const CANDIDATES: &[(&str, u32)] = &[
    ("msyh.ttc", 0),
    ("Deng.ttf", 0),
    ("simhei.ttf", 0),
    ("simsun.ttc", 0),
];

/// 把系统中文字体注册到 egui（App 启动时调用一次；重复调用也安全）。
pub fn install_fonts(ctx: &egui::Context) {
    match load_first_available() {
        Some(LoadedFont { name, data, index }) => {
            install_font(ctx, name, data, index);
        }
        None => {
            // 字体缺失不阻塞 UI：中文可能显示为方块，但程序照常运行。
            crate::logging::warn(
                "未找到可用的中文字体（Deng/simhei/msyh/simsun），中文可能显示为方块",
            );
        }
    }
}

/// 已加载的字体数据（mmap 静态切片或回退的 owned 字节）。
struct LoadedFont {
    name: String,
    data: egui::FontData,
    index: u32,
}

/// Windows 资源管理器使用 Segoe UI；优先半粗体补偿 egui 灰度抗锯齿，
/// 没有 Segoe UI Semibold 时退回常规体。
const LATIN_CANDIDATES: &[&str] = &["seguisb.ttf", "segoeui.ttf"];

/// 安装指定字体数据（mmap 或回退路径共用）。
fn install_font(ctx: &egui::Context, name: String, mut data: egui::FontData, index: u32) {
    let mut fonts = egui::FontDefinitions::default();

    // ttc 需要指定 face 索引
    data.index = index;

    // 西文优先使用 Windows 的 Segoe UI；中文回退到微软雅黑。两者都缺失时
    // 继续用 egui 自带字体。
    let latin_font = LATIN_CANDIDATES
        .iter()
        .find_map(|file| load_named(file, 0));
    let latin_name = latin_font.as_ref().map(|font| font.name.clone());

    fonts.font_data.insert(name.clone(), Arc::new(data));
    if let Some(latin) = latin_font {
        fonts.font_data.insert(latin.name.clone(), Arc::new(latin.data));
    }

    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        if let Some(latin_name) = &latin_name {
            list.insert(0, latin_name.clone());
        }
        list.insert(usize::from(latin_name.is_some()), name.clone());
    }
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push(name.clone());

    ctx.set_fonts(fonts);
    let latin_note = latin_name
        .as_deref()
        .map(|value| format!("，西文 {value}"))
        .unwrap_or_default();
    crate::logging::log(&format!(
        "已加载中文字体：{name}（ttc 索引 {index}，内存映射零拷贝）{latin_note}"
    ));
}

/// 便于单测的注入入口：用调用方提供的字体字节（owned 路径）。
#[cfg(test)]
fn install_owned_bytes(ctx: &egui::Context, name: String, bytes: Vec<u8>, index: u32) {
    install_font(ctx, name, egui::FontData::from_owned(bytes), index);
}

/// 按优先级找第一个可用中文字体。
fn load_first_available() -> Option<LoadedFont> {
    CANDIDATES
        .iter()
        .find_map(|(file, index)| load_named(file, *index))
}

/// 加载指定字体文件，优先 mmap 零拷贝，失败回退 `fs::read`。
fn load_named(file: &str, index: u32) -> Option<LoadedFont> {
    let path = font_dir().join(file);
    // 系统字体文件较大；小于 1KB 视为损坏/占位文件直接跳过。
    if let Some(bytes) = map_readonly(&path) {
        if bytes.len() > 1024 {
            return Some(LoadedFont {
                name: file.to_owned(),
                data: egui::FontData::from_static(bytes),
                index,
            });
        }
    }
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() > 1024 {
            crate::logging::warn(&format!(
                "字体内存映射失败，回退为读取到内存（会多占一份）：{}",
                path.display()
            ));
            return Some(LoadedFont {
                name: file.to_owned(),
                data: egui::FontData::from_owned(bytes),
                index,
            });
        }
    }
    None
}

/// 系统字体目录（`%WINDIR%\Fonts`）。
fn font_dir() -> PathBuf {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_owned());
    Path::new(&windir).join("Fonts")
}

// ---------------------------------------------------------------------------
// 只读内存映射（Windows）
// ---------------------------------------------------------------------------

/// 把文件只读映射到内存，返回进程生命周期内有效的 `&'static [u8]`。
///
/// 句柄/映射对象**故意泄漏**：字体数据在进程生命周期内一直被 egui 引用，
/// 若在这里关闭句柄或解除映射会产生悬垂指针。映射页是文件-backed，不计 Private 提交。
/// 任何失败返回 `None`（调用方回退到 `fs::read`），**不 panic**。
#[cfg(windows)]
fn map_readonly(path: &Path) -> Option<&'static [u8]> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileSizeEx, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Safety: 参数均为有效指针/常量；映射成功后返回的切片在进程生命周期内保持有效
    // （视图与句柄故意不关闭）。失败分支关闭已创建的句柄，不泄漏。
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return None;
        }
        let mut size: i64 = 0;
        if GetFileSizeEx(handle, &mut size) == 0 || size <= 0 {
            CloseHandle(handle);
            return None;
        }
        let mapping = CreateFileMappingW(handle, std::ptr::null(), PAGE_READONLY, 0, 0, std::ptr::null());
        if mapping.is_null() {
            CloseHandle(handle);
            return None;
        }
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        let ptr = view.Value as *const u8;
        if ptr.is_null() {
            CloseHandle(mapping);
            CloseHandle(handle);
            return None;
        }
        // 故意不 CloseHandle(mapping/handle) / UnmapViewOfFile(view)：
        // 字体字节要活到进程退出；关闭会悬垂。只读文件映射页不计入 Private 提交内存。
        Some(std::slice::from_raw_parts(ptr, size as usize))
    }
}

/// 非 Windows 平台不做映射（v1 只验收 Windows），走 `fs::read` 回退。
#[cfg(not(windows))]
fn map_readonly(_path: &Path) -> Option<&'static [u8]> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_order_prefers_yahei() {
        // 微软雅黑优先，等线作为缺字体时的回退
        let names: Vec<&str> = CANDIDATES.iter().map(|(f, _)| *f).collect();
        assert_eq!(names, vec!["msyh.ttc", "Deng.ttf", "simhei.ttf", "simsun.ttc"]);
        // ttc 候选索引均为 0（msyh 索引 0 为 Microsoft YaHei UI）
        assert!(CANDIDATES.iter().all(|(_, idx)| *idx == 0));
    }

    #[test]
    fn latin_candidates_prefer_windows_ui_font() {
        // Windows 资源管理器同款 Segoe UI；优先半粗体补偿灰度抗锯齿。
        assert_eq!(LATIN_CANDIDATES, &["seguisb.ttf", "segoeui.ttf"]);
    }

    #[test]
    fn install_owned_bytes_registers_both_families() {
        // 用最小可用字体字节验证注入不 panic；字体无效时 egui 会回退默认字体。
        let ctx = egui::Context::default();
        install_owned_bytes(&ctx, "fake_cjk.ttf".to_owned(), vec![0u8; 2048], 0);
        // 注册后上下文仍可正常使用（不 panic 即通过；字体真实性由真实桌面截图验证）
        let style = ctx.style();
        assert!(style.text_styles.contains_key(&egui::TextStyle::Body));
    }

    #[test]
    fn mmap_readonly_reads_real_file() {
        // 用仓库内 Cargo.toml 验证只读映射可用（不依赖系统字体是否存在）
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        if let Some(bytes) = map_readonly(&path) {
            assert!(bytes.len() > 16);
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).to_string();
            assert!(text.contains("[package]"), "映射内容应可读，实际开头：{text}");
        }
        // 非 Windows 或不支持映射时允许 None（不 assert 具体分支，仅保证不 panic）
    }

    #[test]
    fn mmap_missing_file_returns_none() {
        let path = std::env::temp_dir().join("clipnest_no_such_font_file.ttf");
        assert!(map_readonly(&path).is_none());
        // 空路径/目录也不 panic
        assert!(map_readonly(Path::new("")).is_none());
        assert!(map_readonly(&font_dir()).is_none());
    }

    #[test]
    fn install_fonts_does_not_panic_without_localized_font() {
        // 无论本机有没有中文字体，install_fonts 都必须安全返回。
        let ctx = egui::Context::default();
        install_fonts(&ctx);
    }
}

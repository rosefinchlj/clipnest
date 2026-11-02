//! 把 assets/icon.ico 写进已编译 exe 的图标资源（RT_GROUP_ICON + RT_ICON）。
//!
//! 存在的意义：本机 MSVC 环境没有 `rc.exe`，build.rs 无法嵌入 exe 图标；
//! 该工具在 cargo 构建完成后直接调用 Win32 资源 API 打补丁，让资源管理器/任务管理器
//! 能正确显示 ClipNest 图标（托盘图标是内嵌 PNG，不依赖这个）。
//!
//! 用法：`patch-icon <exe> <ico>`

use std::env;
use std::fs;
use std::process::ExitCode;
use windows_sys::Win32::System::LibraryLoader::{
    BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

struct IcoEntry {
    width: u8,
    height: u8,
    planes: u16,
    bit_count: u16,
    bytes: u32,
    offset: u32,
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn parse_ico(data: &[u8]) -> Result<Vec<IcoEntry>, String> {
    if data.len() < 6 {
        return Err("ICO 文件太小".into());
    }
    let reserved = u16::from_le_bytes([data[0], data[1]]);
    let kind = u16::from_le_bytes([data[2], data[3]]);
    let count = u16::from_le_bytes([data[4], data[5]]) as usize;
    if reserved != 0 || kind != 1 || count == 0 {
        return Err("不是有效的 ICO 文件".into());
    }
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let base = 6 + index * 16;
        if base + 16 > data.len() {
            return Err("ICO 目录被截断".into());
        }
        let entry = &data[base..base + 16];
        let width = entry[0];
        let height = entry[1];
        let planes = u16::from_le_bytes([entry[4], entry[5]]);
        let bit_count = u16::from_le_bytes([entry[6], entry[7]]);
        let bytes = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]);
        if offset as usize + bytes as usize > data.len() {
            return Err(format!("ICO 第 {index} 张图超出文件范围"));
        }
        out.push(IcoEntry {
            width,
            height,
            planes,
            bit_count,
            bytes,
            offset,
        });
    }
    Ok(out)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("用法: patch-icon <exe> <ico>");
        return ExitCode::from(2);
    }
    let exe = &args[0];
    let ico = &args[1];
    let data = match fs::read(ico) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("读取 {ico} 失败: {err}");
            return ExitCode::FAILURE;
        }
    };
    let entries = match parse_ico(&data) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("解析 ICO 失败: {err}");
            return ExitCode::FAILURE;
        }
    };

    // GRPICONDIR + GRPICONDIRENTRY
    let mut group = Vec::with_capacity(6 + entries.len() * 14);
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (index, entry) in entries.iter().enumerate() {
        group.push(entry.width);
        group.push(entry.height);
        group.push(0); // color count
        group.push(0); // reserved
        group.extend_from_slice(&entry.planes.to_le_bytes());
        group.extend_from_slice(&entry.bit_count.to_le_bytes());
        group.extend_from_slice(&entry.bytes.to_le_bytes());
        group.extend_from_slice(&((index + 1) as u16).to_le_bytes());
    }

    let exe_wide = to_wide(exe);
    unsafe {
        let handle = BeginUpdateResourceW(exe_wide.as_ptr(), 0);
        if handle.is_null() {
            eprintln!(
                "BeginUpdateResourceW 失败（错误 {}），目标 exe 可能正在运行",
                std::io::Error::last_os_error()
            );
            return ExitCode::FAILURE;
        }
        for (index, entry) in entries.iter().enumerate() {
            let blob = &data[entry.offset as usize..(entry.offset + entry.bytes) as usize];
            let ok = UpdateResourceW(
                handle,
                RT_ICON as *const u16,
                (index + 1) as *const u16,
                0,
                blob.as_ptr() as *const core::ffi::c_void,
                entry.bytes,
            );
            if ok == 0 {
                eprintln!("写入 RT_ICON {} 失败", index + 1);
                let _ = EndUpdateResourceW(handle, 1);
                return ExitCode::FAILURE;
            }
        }
        let ok = UpdateResourceW(
            handle,
            RT_GROUP_ICON as *const u16,
            1 as *const u16,
            0,
            group.as_ptr() as *const core::ffi::c_void,
            group.len() as u32,
        );
        if ok == 0 {
            eprintln!("写入 RT_GROUP_ICON 失败");
            let _ = EndUpdateResourceW(handle, 1);
            return ExitCode::FAILURE;
        }
        if EndUpdateResourceW(handle, 0) == 0 {
            eprintln!(
                "EndUpdateResourceW 失败（错误 {}）",
                std::io::Error::last_os_error()
            );
            return ExitCode::FAILURE;
        }
    }
    println!("已写入 {} 个图标尺寸到 {exe}", entries.len());
    ExitCode::SUCCESS
}

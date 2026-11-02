//! 构建脚本：把应用图标嵌入 exe（可选，失败不影响构建）。
//!
//! 资源编译依赖 Windows SDK 的 rc.exe；如果环境缺失，`embed_resource` 会失败，
//! 此时 exe 只是没有图标，功能完全不受影响，因此这里允许失败并打印提示。

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(target_os = "windows")]
    {
        // rc.exe 属于 Windows SDK；某些精简 MSVC 环境没有它。托盘图标已在代码里内嵌
        // 多尺寸 PNG，不依赖 exe 资源，所以这里缺 rc.exe 只影响 Explorer 里显示的 exe 图标。
        let rc_available = std::process::Command::new("where")
            .arg("rc.exe")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !rc_available {
            println!("cargo:warning=未找到 rc.exe，跳过 exe 图标资源嵌入（托盘图标已内嵌，不受影响）");
        } else {
            let result = std::panic::catch_unwind(|| {
                // 绝对路径生成 .rc：避免 rc.exe 以不同 cwd 解析相对图标路径导致资源缺失。
                let manifest =
                    std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
                let icon = manifest.join("assets").join("icon.ico");
                let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
                let rc_path = out_dir.join("clipnest-icon.rc");
                let _ = std::fs::write(&rc_path, format!("1 ICON \"{}\"\n", icon.display()));
                let _ = embed_resource::compile(&rc_path, embed_resource::NONE);
            });
            if result.is_err() {
                println!("cargo:warning=嵌入 exe 图标失败（不影响功能）");
            }
        }
    }
}

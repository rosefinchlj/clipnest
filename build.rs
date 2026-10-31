//! 构建脚本：把应用图标嵌入 exe（可选，失败不影响构建）。
//!
//! 资源编译依赖 Windows SDK 的 rc.exe；如果环境缺失，`embed_resource` 会失败，
//! 此时 exe 只是没有图标，功能完全不受影响，因此这里允许失败并打印提示。

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(target_os = "windows")]
    {
        let result = std::panic::catch_unwind(|| {
            let _ = embed_resource::compile("assets/app.rc", embed_resource::NONE);
        });
        if result.is_err() {
            println!("cargo:warning=嵌入 exe 图标失败（不影响功能）");
        }
    }
}

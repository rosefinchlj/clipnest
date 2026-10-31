//! 命令行诊断：`--selftest`、`--diagnose`、`--demo-seed`。
//!
//! 骨架阶段先返回占位错误；由 Lead 在 Task 17 实现完整逻辑。

/// 无界面全链路自检（隔离临时目录，绝不碰真实数据）。
pub fn selftest(out_path: &str) -> Result<String, String> {
    let _ = out_path;
    Err("--selftest 尚未实现（Task 17）".to_string())
}

/// 环境与能力诊断。
pub fn diagnose(out_path: &str) -> Result<String, String> {
    let _ = out_path;
    Err("--diagnose 尚未实现（Task 17）".to_string())
}

/// 写入演示数据（需要 `--yes` 确认）。
pub fn demo_seed(count: usize, confirmed: bool) -> Result<String, String> {
    if !confirmed {
        return Err("写入演示数据会修改真实数据库，请加 --yes 确认".to_string());
    }
    Ok(format!("demo-seed 尚未实现（Task 17）；请求 {count} 条"))
}

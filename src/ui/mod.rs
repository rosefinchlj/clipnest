//! UI 域：主题、字体、Picker 面板、Manager 管理台、复用控件、缩略图。
//!
//! 由 ui teammate 实现。对外只暴露 [`crate::ui::ClipNestApp`] 相关的渲染入口
//! （由 Lead 在 `app.rs` 中接线）；`ui::preview` 提供 `--ui-preview` 独立预览模式。

#![allow(dead_code)]

pub mod preview;

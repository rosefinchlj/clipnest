//! 管理台页面：历史 / 收藏 / 回收站 / 设置 / 关于。
//!
//! 每个页面只负责渲染与产生 [`crate::ui::UiAction`]，不直接改设置文件或数据库。

pub mod about;
pub mod favorites;
pub mod history;
pub mod settings;
pub mod trash;

//! 剪贴板域：Windows 剪贴板读取/归一化/回写、DIB ↔ PNG、内容哈希。
//!
//! 由 win32 teammate 实现；只被 `sys` 域调用，UI 不直接依赖本域。
//! 计划中的子模块：`reader.rs`（读取+归一化）、`writer.rs`（回写）、`dib.rs`（DIB 解析）、
//! `hash.rs`（SHA-256）。

#![allow(dead_code)]

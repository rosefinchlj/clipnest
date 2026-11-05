//! 存储域（Store）：SQLite 持久化、去重、搜索、容量治理、blob 文件库、导出。
//!
//! 对外仅暴露 [`StoreHandle`] + [`StoreError`]（CONTRACT §2.1）：
//! `StoreHandle::spawn` 打开数据库后启动一个独占 SQLite 连接的线程，
//! 通过 `Sender<StoreCmd>` 接收命令、通过 `Sender<Event>` 回发事件。
//!
//! 设计要点：
//! - UI 线程永不直接碰数据库；所有 `StoreCmd` 串行处理，天然无锁；
//! - 同一轮排空的多条 `Upsert` 合并为一个 `ClipsChanged`（spec §9.3 批量事务精神）；
//! - 每条命令处理完调用 `ctx.request_repaint()` 唤醒 UI；
//! - 命令处理中的 panic 被捕获并写日志，线程继续存活（CONTRACT §0）；
//! - 日志只记录类型/大小/哈希前 8 位，绝不记录剪贴板全文。

pub(crate) mod backup;
pub(crate) mod blob;
pub(crate) mod db;
pub(crate) mod export;
pub(crate) mod query;
pub(crate) mod retention;
pub(crate) mod schema;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_backup;
#[cfg(test)]
mod tests_blob;
#[cfg(test)]
mod tests_thread;

use crate::model::{BackupEvent, CopyState, Event, Settings, StoreCmd};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use db::{Db, DbOptions, UpsertOutcome};

/// 存储线程句柄：持有命令通道；`Drop` 时发送 `Shutdown` 并等待线程退出。
pub struct StoreHandle {
    tx: Sender<StoreCmd>,
    join: Option<JoinHandle<()>>,
}

impl StoreHandle {
    /// 正常启动：使用 `config::data_dir()`。阻塞到数据库打开/迁移完成。
    pub fn spawn(
        events: Sender<Event>,
        settings: Arc<Mutex<Settings>>,
        ctx: egui::Context,
    ) -> Result<Self, StoreError> {
        Self::spawn_at(crate::config::data_dir(), events, settings, ctx)
    }

    /// 指定数据目录启动（`--selftest` / 单元测试用，绝不碰真实数据）。
    pub fn spawn_at(
        data_dir: PathBuf,
        events: Sender<Event>,
        settings: Arc<Mutex<Settings>>,
        ctx: egui::Context,
    ) -> Result<Self, StoreError> {
        std::fs::create_dir_all(&data_dir)?;
        let database = Db::open_in_dir(&data_dir, DbOptions::from_env(&data_dir))?;
        crate::logging::log(&format!(
            "Store 就绪：数据库={}，FTS={}，schema=v{}",
            database.db_path.display(),
            if database.fts_enabled {
                "trigram".to_string()
            } else {
                "LIKE 降级".to_string()
            },
            database.schema_version().unwrap_or(0)
        ));

        let (tx, rx) = mpsc::channel::<StoreCmd>();
        let thread_name = "clipnest-store".to_string();
        let join = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let store = Store {
                    db: database,
                    settings,
                    events,
                };
                store.run(rx, ctx);
            })
            .map_err(|err| StoreError::Msg(format!("启动 Store 线程失败：{err}")))?;
        Ok(Self {
            tx,
            join: Some(join),
        })
    }

    /// 命令发送端（UI 侧克隆使用）。
    pub fn cmd_tx(&self) -> Sender<StoreCmd> {
        self.tx.clone()
    }
}

impl Drop for StoreHandle {
    fn drop(&mut self) {
        // 通道可能已断开（线程异常退出），忽略发送错误
        let _ = self.tx.send(StoreCmd::Shutdown);
        if let Some(join) = self.join.take() {
            if join.join().is_err() {
                crate::logging::error("Store 线程异常退出（join 失败）");
            }
        }
    }
}

/// 线程内部状态（仅 Store 线程可见）。
struct Store {
    db: Db,
    settings: Arc<Mutex<Settings>>,
    events: Sender<Event>,
}

impl Store {
    /// 线程主循环：批量取命令 → 串行处理 → 合并事件 → 唤醒 UI。
    fn run(mut self, rx: Receiver<StoreCmd>, ctx: egui::Context) {
        // 启动清理（spec §10.4：启动后执行一次 retention）
        {
            let settings = self.settings_snapshot();
            match retention::prune(&self.db, &settings) {
                Ok(report)
                    if report.deleted_clips > 0
                        || report.deleted_blobs > 0
                        || report.purged_trash > 0 =>
                {
                    crate::logging::log(&format!(
                        "Store 启动清理：删除 {} 条记录、{} 个孤儿 blob、{} 条过期回收站",
                        report.deleted_clips, report.deleted_blobs, report.purged_trash
                    ));
                }
                Ok(_) => {}
                Err(err) => crate::logging::warn(&format!("Store 启动清理失败：{err}")),
            }
        }

        let mut dirty = false; // 需要发 ClipsChanged
        let mut stats_due = false; // 需要发 Stats
        let mut writes = 0usize; // 距上次 retention 的写入次数
        loop {
            let Ok(first) = rx.recv() else {
                break; // 所有发送端已释放
            };
            // 同一轮可用的命令合并处理（同一帧多条 Upsert 只发一次 ClipsChanged）
            let mut batch = vec![first];
            while let Ok(next) = rx.try_recv() {
                batch.push(next);
            }

            let mut shutdown = false;
            for cmd in batch {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.handle(cmd, &mut dirty, &mut stats_due, &mut writes)
                }));
                match outcome {
                    Ok(Ok(should_exit)) => {
                        if should_exit {
                            shutdown = true;
                            break;
                        }
                    }
                    Ok(Err(err)) => {
                        crate::logging::error(&format!("Store 命令处理失败：{err}"));
                        self.toast(format!("存储操作失败：{err}"));
                    }
                    Err(payload) => {
                        let reason = panic_message(&payload);
                        crate::logging::error(&format!(
                            "Store 命令处理 panic（已捕获，线程继续）：{reason}"
                        ));
                        self.toast("存储线程内部错误，请重试".to_string());
                    }
                }
                ctx.request_repaint();
            }

            // 每 100 次写入触发一次 retention
            if writes >= 100 {
                writes = 0;
                let settings = self.settings_snapshot();
                match retention::prune(&self.db, &settings) {
                    Ok(report)
                        if report.deleted_clips > 0
                            || report.deleted_blobs > 0
                            || report.purged_trash > 0 =>
                    {
                        dirty = true;
                        crate::logging::log(&format!(
                            "Store 周期清理：删除 {} 条记录、{} 个孤儿 blob、{} 条过期回收站",
                            report.deleted_clips, report.deleted_blobs, report.purged_trash
                        ));
                    }
                    Ok(_) => {}
                    Err(err) => crate::logging::warn(&format!("Store 周期清理失败：{err}")),
                }
            }

            // 合并后的变更/统计事件
            if dirty {
                self.emit(Event::ClipsChanged);
                dirty = false;
            }
            if stats_due {
                match self.db.counts() {
                    Ok(stats) => self.emit(Event::Stats(stats)),
                    Err(err) => crate::logging::warn(&format!("Store 统计失败：{err}")),
                }
                stats_due = false;
            }
            ctx.request_repaint();

            if shutdown {
                break;
            }
        }
        // 退出前把 WAL 落盘，避免 -wal 文件残留
        if let Err(err) = self
            .db
            .conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        {
            crate::logging::warn(&format!("Store 退出 checkpoint 失败：{err}"));
        }
        crate::logging::log("Store 线程退出");
    }

    /// 处理一条命令；返回 true 表示收到 `Shutdown`。
    fn handle(
        &mut self,
        cmd: StoreCmd,
        dirty: &mut bool,
        stats_due: &mut bool,
        writes: &mut usize,
    ) -> StoreResult<bool> {
        match cmd {
            StoreCmd::Upsert(input) => {
                let outcome = self.db.upsert(&input)?;
                *writes += 1;
                *dirty = true;
                let verb = match outcome {
                    UpsertOutcome::Inserted(_) => "新增",
                    UpsertOutcome::Touched(_) => "去重",
                };
                crate::logging::debug(&format!(
                    "Store Upsert {verb}：id={} kind={:?} bytes={}",
                    outcome.id(),
                    input.kind,
                    input.byte_len
                ));
                Ok(false)
            }
            StoreCmd::Search(query) => {
                let page = self.db.search(&query)?;
                crate::logging::debug(&format!(
                    "Store 搜索 gen={} 结果={}/{} 耗时={}ms",
                    page.gen,
                    page.items.len(),
                    page.total,
                    page.elapsed_ms
                ));
                self.emit(Event::SearchPage(page));
                Ok(false)
            }
            StoreCmd::ListSourceApps => {
                let apps = self.db.source_apps()?;
                self.emit(Event::SourceApps(apps));
                Ok(false)
            }
            StoreCmd::SetFavorited { id, favorited } => {
                if !self.db.set_favorited(id, favorited)? {
                    self.toast("记录已不存在".to_string());
                    return Ok(false);
                }
                *dirty = true;
                if favorited {
                    if let Some(crate::model::ClipKind::Files) = self.db.clip_kind(id)? {
                        let settings = self.settings_snapshot();
                        self.report_favorite_copy(id, &settings)?;
                    }
                }
                Ok(false)
            }
            StoreCmd::SetPinned { id, pinned } => {
                if !self.db.set_pinned(id, pinned)? {
                    self.toast("只有收藏项可以置顶".to_string());
                    return Ok(false);
                }
                *dirty = true;
                Ok(false)
            }
            StoreCmd::Delete(ids) => {
                let deleted = self.db.soft_delete(&ids)?;
                if deleted > 0 {
                    *dirty = true;
                }
                Ok(false)
            }
            StoreCmd::Restore(ids) => {
                let restored = self.db.restore(&ids)?;
                if restored > 0 {
                    *dirty = true;
                    *stats_due = true;
                }
                Ok(false)
            }
            StoreCmd::Purge(ids) => {
                let purged = self.db.purge(&ids)?;
                if purged > 0 {
                    *dirty = true;
                    *stats_due = true;
                }
                Ok(false)
            }
            StoreCmd::EmptyTrash => {
                let purged = self.db.empty_trash()?;
                if purged > 0 {
                    *dirty = true;
                    *stats_due = true;
                }
                Ok(false)
            }
            StoreCmd::Clear {
                delete_blobs,
                include_favorites,
            } => {
                let outcome = self.db.clear(delete_blobs, include_favorites)?;
                *dirty = true;
                *stats_due = true;
                if include_favorites {
                    self.toast(format!("已清空全部 {} 条历史", outcome.cleared));
                } else if outcome.kept > 0 {
                    self.toast(format!(
                        "已清空 {} 条未收藏记录，保留收藏 {} 条",
                        outcome.cleared, outcome.kept
                    ));
                } else {
                    self.toast(format!("已清空 {} 条历史", outcome.cleared));
                }
                Ok(false)
            }
            StoreCmd::ClearForExit(policy) => {
                if policy.clears() {
                    // 退出清理属于隐私动作：被删除记录引用的图片/文件实体一并物理删除；
                    // 收藏项（及收藏文件实体）在该分支不会被删除，引用计数保护其 blob。
                    let outcome = self.db.clear(true, policy.include_favorites())?;
                    crate::logging::log(&format!(
                        "退出清理（{}）：删除 {} 条、保留收藏 {} 条",
                        policy.label(),
                        outcome.cleared,
                        outcome.kept
                    ));
                    self.emit(Event::ClipsChanged);
                }
                // 让 Store 线程在清理完成后自行退出；UI 侧 `store=None` 的 Drop join 会等它结束。
                Ok(true)
            }
            StoreCmd::GetPastePayload(id) => {
                match self.db.paste_payload(id)? {
                    Some(payload) => {
                        // 「粘贴后置顶」（spec §6.3 设置项，默认开）
                        let settings = self.settings_snapshot();
                        if settings.move_to_top_after_paste && self.db.bump_used(id)? {
                            *dirty = true;
                        }
                        self.emit(Event::PastePayload(payload));
                    }
                    None => self.toast("记录已不存在".to_string()),
                }
                Ok(false)
            }

            StoreCmd::SaveFavoriteFiles(id) => {
                let settings = self.settings_snapshot();
                if self.db.save_favorite_files(id, &settings)?.is_some() {
                    *dirty = true;
                    self.report_favorite_copy(id, &settings)?;
                } else {
                    self.toast("仅文件类型条目需要保存副本".to_string());
                }
                Ok(false)
            }
            StoreCmd::RefreshStats => {
                let settings = self.settings_snapshot();
                match retention::prune(&self.db, &settings) {
                    Ok(report) => {
                        if report.deleted_clips > 0 || report.purged_trash > 0 {
                            *dirty = true;
                        }
                        if report.favorites_limit_hit {
                            self.toast("收藏文件副本已达上限，后续仅保留路径".to_string());
                        }
                    }
                    Err(err) => crate::logging::warn(&format!("Store 刷新时清理失败：{err}")),
                }
                *stats_due = true;
                Ok(false)
            }
            StoreCmd::ExportJson { path } => {
                let report = export::export_json(&self.db, &path)?;
                self.toast(format!(
                    "已导出 {} 条记录到 {}",
                    report.clips,
                    report.path.display()
                ));
                Ok(false)
            }
            StoreCmd::CreateBackup { path } => {
                match backup::create_backup(&self.db, &path) {
                    Ok(report) => self.emit(Event::Backup(BackupEvent::Created(report))),
                    Err(err) => {
                        crate::logging::error(&format!("创建完整备份失败：{err}"));
                        self.emit(Event::Backup(BackupEvent::Failed(format!(
                            "创建备份失败：{err}"
                        ))));
                    }
                }
                Ok(false)
            }
            StoreCmd::InspectBackup { path } => {
                match backup::inspect_backup(&path, self.db.key_hex.as_deref()) {
                    Ok(preview) => {
                        self.emit(Event::Backup(BackupEvent::Inspected(Box::new(preview))))
                    }
                    Err(err) => {
                        crate::logging::warn(&format!("检查备份失败：{err}"));
                        self.emit(Event::Backup(BackupEvent::Failed(format!(
                            "检查备份失败：{err}"
                        ))));
                    }
                }
                Ok(false)
            }
            StoreCmd::RestoreBackup { path } => {
                match backup::restore_backup(&self.db, &path, self.db.key_hex.as_deref()) {
                    Ok(report) => {
                        *dirty = true;
                        *stats_due = true;
                        self.emit(Event::Backup(BackupEvent::Restored(report)));
                    }
                    Err(err) => {
                        crate::logging::error(&format!("从备份恢复失败：{err}"));
                        self.emit(Event::Backup(BackupEvent::Failed(format!(
                            "恢复失败：{err}"
                        ))));
                    }
                }
                Ok(false)
            }
            StoreCmd::SaveImageAs { id, path } => {
                match self.db.save_image_as(id, &path) {
                    Ok(true) => self.toast(format!("已保存图片到 {}", path.display())),
                    Ok(false) => self.toast("图片内容不存在或已被清理".to_string()),
                    Err(err) => self.toast(format!("保存图片失败：{err}")),
                }
                Ok(false)
            }
            StoreCmd::SaveTextAs { id, path } => {
                match self.db.save_text_as(id, &path) {
                    Ok(true) => self.toast(format!("已保存文本到 {}", path.display())),
                    Ok(false) => self.toast("文本内容不存在或已被清理".to_string()),
                    Err(err) => self.toast(format!("保存文本失败：{err}")),
                }
                Ok(false)
            }
            StoreCmd::SetEncryption { enabled } => {
                match self.db.set_encryption(enabled) {
                    Ok(changed) => {
                        *stats_due = true;
                        self.emit(Event::EncryptionChanged {
                            enabled,
                            error: None,
                        });
                        self.toast(if enabled {
                            "数据库已加密（Windows 当前用户密钥，下次启动自动解锁）".to_string()
                        } else {
                            "数据库加密已关闭".to_string()
                        });
                        if changed {
                            crate::logging::log(&format!(
                                "Store: 数据库加密{}",
                                if enabled { "已开启" } else { "已关闭" }
                            ));
                        }
                    }
                    Err(err) => {
                        crate::logging::error(&format!("切换数据库加密失败：{err}"));
                        self.emit(Event::EncryptionChanged {
                            enabled: !enabled,
                            error: Some(format!("{err}")),
                        });
                        self.toast(format!("切换加密失败：{err}"));
                    }
                }
                Ok(false)
            }
            StoreCmd::Shutdown => Ok(true),
        }
    }

    /// 收藏文件实体复制 + 状态事件 + Toast 提示（A4）。
    ///
    /// 复制前先发 `Pending`（UI 立刻显示「正在保存副本…」，不冻结界面），
    /// 复制完成后再发最终状态（Stored/Failed/TooLarge/Missing）并刷新列表。
    fn report_favorite_copy(&self, id: i64, settings: &Settings) -> StoreResult<()> {
        self.emit(Event::FileCopyState {
            clip_id: id,
            state: CopyState::Pending,
        });
        let Some(copy) = self.db.save_favorite_files(id, settings)? else {
            self.emit(Event::FileCopyState {
                clip_id: id,
                state: CopyState::NotNeeded,
            });
            return Ok(());
        };
        let state = copy.overall_state();
        self.emit(Event::FileCopyState { clip_id: id, state });
        if copy.limit_hit {
            self.toast("收藏文件副本已达上限，仅保留路径".to_string());
        } else if copy.failed > 0 {
            self.toast(format!(
                "{} 个文件副本保存失败，可在详情里重试",
                copy.failed
            ));
        } else if copy.copied > 0 {
            self.toast(format!("已保存 {} 个文件副本", copy.copied));
        } else if copy.skipped_too_large > 0 {
            self.toast(format!(
                "{} 个文件超过单文件上限，未保存副本",
                copy.skipped_too_large
            ));
        }
        if copy.skipped_missing > 0 {
            crate::logging::warn(&format!(
                "收藏文件：{} 个源文件不存在，仅保留路径",
                copy.skipped_missing
            ));
        }
        Ok(())
    }

    fn settings_snapshot(&self) -> Settings {
        self.settings
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn emit(&self, event: Event) {
        // UI 线程可能已退出：发送失败不影响存储线程
        let _ = self.events.send(event);
    }

    fn toast(&self, message: impl Into<String>) {
        self.emit(Event::Toast(message.into()));
    }
}

/// 从 panic payload 中提取可读信息（日志用）。
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "未知 panic".to_string()
    }
}

/// 存储域错误（CONTRACT §2.1）。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("SQLite 错误: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

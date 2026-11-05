//! SQLite 主库：打开/连接参数、迁移入口、写入与去重、删除、收藏文件实体编排。
//!
//! 线程模型：`Db` 由 Store 线程独占（`rusqlite::Connection` 非 Sync），
//! 但所有写方法内部仍用事务保证「行 + blob 引用 + FTS 触发器」的原子性。
//! 查询相关方法在 [`crate::store::query`] 中实现（`impl Db`）。

use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

use crate::model::{now_ms, ClipId, ClipInput, ClipKind, FileEntry, Settings};
use crate::store::blob::{self, FavoriteCopy};
use crate::store::{schema, StoreError, StoreResult};

/// 主库文件名（位于数据目录下）。
pub const DB_FILE: &str = "data.db";

/// 打开数据库时的选项（测试可强制关闭 FTS 以覆盖 LIKE 降级路径）。
#[derive(Clone, Copy, Debug, Default)]
pub struct DbOptions {
    /// 强制禁用 FTS5，等价于环境变量 `CLIPNEST_DISABLE_FTS=1`。
    pub disable_fts: bool,
}

impl DbOptions {
    /// 从环境变量读取：`CLIPNEST_DISABLE_FTS=1` 时禁用 FTS（--selftest / 排查用）。
    pub fn from_env() -> Self {
        let disabled = std::env::var("CLIPNEST_DISABLE_FTS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Self {
            disable_fts: disabled,
        }
    }
}

/// 写入结果：新建条目 / 命中已有条目（去重）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Inserted(ClipId),
    Touched(ClipId),
}

impl UpsertOutcome {
    pub fn id(self) -> ClipId {
        match self {
            UpsertOutcome::Inserted(id) | UpsertOutcome::Touched(id) => id,
        }
    }
}

/// 清空历史的结果（A2）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClearOutcome {
    /// 被删除的条目数（含回收站）。
    pub cleared: usize,
    /// 因收藏而保留下来的条目数。
    pub kept: usize,
    /// 涉及并释放的 blob 实体数。
    pub blobs: usize,
}

/// 独占的 SQLite 连接 + 数据目录 + FTS 能力标记。
pub struct Db {
    pub conn: Connection,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    /// 本连接是否可使用 FTS5 搜索（迁移时探测）。
    pub fts_enabled: bool,
    /// FTS 降级原因（启用时为 None）。
    pub fts_error: Option<String>,
}

impl Db {
    /// 打开（必要时修复）数据库文件；损坏时备份并重建空库（spec §14）。
    pub fn open(db_path: &Path, options: DbOptions) -> StoreResult<Self> {
        match Self::open_inner(db_path, options) {
            Ok(db) => Ok(db),
            Err(err) if db_corrupted(&err) => {
                crate::logging::error(&format!("数据库损坏或无法识别（{err}），备份后将重建空库"));
                let backup = backup_database(db_path)?;
                crate::logging::warn(&format!("原数据库已备份：{}", backup.display()));
                Self::open_inner(db_path, options)
            }
            Err(err) => Err(err),
        }
    }

    /// 在数据目录下打开 `data.db`。
    pub fn open_in_dir(data_dir: &Path, options: DbOptions) -> StoreResult<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db = Self::open(&data_dir.join(DB_FILE), options)?;
        if let Err(err) = blob::ensure_dirs(&db.data_dir) {
            crate::logging::warn(&format!("创建 blob 目录失败（不影响数据库）：{err}"));
        }
        Ok(db)
    }

    fn open_inner(db_path: &Path, options: DbOptions) -> StoreResult<Self> {
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(db_path)?;
        Self::tune(&conn)?;
        let data_dir = db_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut db = Self {
            conn,
            data_dir,
            db_path: db_path.to_path_buf(),
            fts_enabled: false,
            fts_error: None,
        };
        // 启动完整性检查（spec §14：quick_check 失败 → 备份重建）
        if !quick_check(&db.conn)? {
            return Err(StoreError::Db(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                Some("PRAGMA quick_check 未通过".to_string()),
            )));
        }
        db.migrate(options)?;
        Ok(db)
    }

    /// PRAGMA 参数（spec §10.2）：WAL + synchronous=NORMAL + 2MB 页缓存 + 内存临时表 + 外键。
    fn tune(conn: &Connection) -> StoreResult<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-2048;
             PRAGMA temp_store=MEMORY;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        let mode: String = conn.pragma_query_one(None, "journal_mode")?;
        if !mode.eq_ignore_ascii_case("wal") {
            crate::logging::warn(&format!("Store: journal_mode 期望 wal，实际 {mode}"));
        }
        Ok(())
    }

    /// 执行 schema 迁移（幂等，可重复调用）。
    pub fn migrate(&mut self, options: DbOptions) -> StoreResult<schema::MigrateOutcome> {
        let outcome = schema::migrate(&mut self.conn, options)?;
        self.fts_enabled = outcome.fts_enabled;
        self.fts_error = outcome.fts_error.clone();
        Ok(outcome)
    }

    /// `meta.schema_version`（未初始化时为 0）。
    pub fn schema_version(&self) -> StoreResult<i64> {
        let raw = schema::read_meta(&self.conn, "schema_version")?;
        Ok(raw.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0))
    }

    /// 读取 meta 键（诊断/测试用；v1.1 也用于读取缓存统计）。
    #[allow(dead_code)]
    pub fn meta_get(&self, key: &str) -> StoreResult<Option<String>> {
        schema::read_meta(&self.conn, key)
    }

    /// 写入 meta 键（upsert）。
    pub fn meta_set(&self, key: &str, value: &str) -> StoreResult<()> {
        schema::write_meta(&self.conn, key, value)
    }

    /// 数据库文件占用（含 WAL/SHM），用于 Stats.db_bytes。
    pub fn db_file_bytes(&self) -> u64 {
        let mut total = 0;
        for suffix in ["", "-wal", "-shm"] {
            if let Ok(meta) = std::fs::metadata(sibling_path(&self.db_path, suffix)) {
                total += meta.len();
            }
        }
        total
    }

    // -----------------------------------------------------------------------
    // 写入 / 去重
    // -----------------------------------------------------------------------

    /// 写入一条剪贴板内容：hash 已存在则更新 last_used_at/use_count（pinned 不变），
    /// 否则插入新行；图片内容寻址落盘并登记 blob。
    pub fn upsert(&self, input: &ClipInput) -> StoreResult<UpsertOutcome> {
        if let Some((existing, deleted_at)) = self.find_id_by_hash(&input.hash)? {
            if deleted_at.is_some() {
                self.restore(&[existing])?;
                crate::logging::log(&format!(
                    "Store 重新复制命中回收站条目 id={existing}，已自动恢复"
                ));
            } else {
                self.touch(existing)?;
            }
            crate::logging::debug(&format!(
                "Store 去重命中 id={existing} kind={:?} hash={}…",
                input.kind,
                short_hash(&input.hash)
            ));
            return Ok(UpsertOutcome::Touched(existing));
        }

        // 图片先落盘；blob 行随 clip 行同事务提交。
        let blob_id = match (input.kind, input.image_png.as_deref()) {
            (ClipKind::Image, Some(png)) => Some(blob::put_image(&self.conn, &self.data_dir, png)?),
            (ClipKind::Image, None) => {
                return Err(StoreError::Msg("图片条目缺少 PNG 数据，已跳过".to_string()))
            }
            _ => None,
        };
        match self.insert_clip_row(input, blob_id.clone()) {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                // 插入失败：回收刚登记的 blob 引用，避免产生 refs>0 的孤儿
                if let Some(id) = blob_id.as_deref() {
                    if let Err(cleanup_err) = blob::release_blob(&self.conn, &self.data_dir, id) {
                        crate::logging::warn(&format!("回滚 blob 引用失败：{cleanup_err}"));
                    }
                }
                Err(err)
            }
        }
    }

    /// 导入用写入：blob 已存在（不重复写文件、不增加引用计数，由导入方保证一致性）。
    /// 预留给 JSON 导入功能（Task 5 要求接口先留好），v1 仅在导入代码路径使用。
    #[allow(dead_code)]
    pub fn import_upsert(
        &self,
        input: &ClipInput,
        blob_id: Option<String>,
    ) -> StoreResult<UpsertOutcome> {
        if let Some((existing, deleted_at)) = self.find_id_by_hash(&input.hash)? {
            if deleted_at.is_some() {
                self.restore(&[existing])?;
            } else {
                self.touch(existing)?;
            }
            return Ok(UpsertOutcome::Touched(existing));
        }
        self.insert_clip_row(input, blob_id)
    }

    fn insert_clip_row(
        &self,
        input: &ClipInput,
        blob_id: Option<String>,
    ) -> StoreResult<UpsertOutcome> {
        let files_json = if input.kind == ClipKind::Files {
            Some(
                serde_json::to_string(&input.files)
                    .map_err(|err| StoreError::Msg(format!("文件清单序列化失败：{err}")))?,
            )
        } else {
            None
        };
        let created_at = if input.captured_at > 0 {
            input.captured_at
        } else {
            now_ms()
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO clips
               (kind, hash, text, html, rtf, preview, blob_id, files_json, byte_len, width, height,
                created_at, last_used_at, use_count, favorited, pinned, source_app, source_title)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, 1, 0, 0, ?13, ?14)",
            params![
                input.kind.as_i32(),
                input.hash,
                input.text,
                input.html,
                input.rtf,
                input.preview,
                blob_id,
                files_json,
                input.byte_len as i64,
                input.width.map(i64::from),
                input.height.map(i64::from),
                created_at,
                input.source_app,
                input.source_title,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        crate::logging::debug(&format!(
            "Store 新增 id={id} kind={:?} bytes={} hash={}…",
            input.kind,
            input.byte_len,
            short_hash(&input.hash)
        ));
        Ok(UpsertOutcome::Inserted(id))
    }

    /// 命中已有 hash：更新 last_used_at 并累加 use_count（pinned 不变）。
    fn touch(&self, id: ClipId) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE clips SET last_used_at = ?1, use_count = use_count + 1
             WHERE id = ?2 AND deleted_at IS NULL",
            params![now_ms(), id],
        )?;
        Ok(())
    }

    fn find_id_by_hash(&self, hash: &str) -> StoreResult<Option<(ClipId, Option<i64>)>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, deleted_at FROM clips WHERE hash = ?1",
                params![hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// 按 id 查询条目类型（不存在 → None）。
    pub fn clip_kind(&self, id: ClipId) -> StoreResult<Option<ClipKind>> {
        let kind: Option<i32> = self
            .conn
            .query_row(
                "SELECT kind FROM clips WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(kind.and_then(ClipKind::from_i32))
    }

    // -----------------------------------------------------------------------
    // 收藏 / 删除 / 清空
    // -----------------------------------------------------------------------

    /// 设置收藏标记；取消收藏时一并取消置顶。返回是否存在该条目。
    pub fn set_favorited(&self, id: ClipId, favorited: bool) -> StoreResult<bool> {
        let changed = self.conn.execute(
            "UPDATE clips
             SET favorited = ?1,
                 pinned = CASE WHEN ?1 = 0 THEN 0 ELSE pinned END
             WHERE id = ?2 AND deleted_at IS NULL",
            params![i32::from(favorited), id],
        )?;
        Ok(changed > 0)
    }

    /// 设置置顶；只有收藏项可置顶。返回是否实际更新。
    pub fn set_pinned(&self, id: ClipId, pinned: bool) -> StoreResult<bool> {
        let changed = self.conn.execute(
            "UPDATE clips SET pinned = ?1
             WHERE id = ?2 AND deleted_at IS NULL AND (?1 = 0 OR favorited = 1)",
            params![i32::from(pinned), id],
        )?;
        Ok(changed > 0)
    }

    /// 把文件条目涉及的文件实体复制进数据目录（收藏时调用，幂等）。
    ///
    /// 返回 None 表示该条目不是文件类型；`FavoriteCopy` 带复制统计与超限标记。
    pub fn save_favorite_files(
        &self,
        id: ClipId,
        settings: &Settings,
    ) -> StoreResult<Option<FavoriteCopy>> {
        let row: Option<(i32, Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT kind, files_json, blob_id FROM clips WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((kind, files_json, blob_id)) = row else {
            return Ok(None);
        };
        if kind != ClipKind::Files.as_i32() {
            return Ok(None);
        }
        let files: Vec<FileEntry> = files_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        let hash: String = self
            .conn
            .query_row(
                "SELECT hash FROM clips WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |row| row.get(0),
            )
            .unwrap_or_default();
        let copy = blob::copy_favorite_files(
            &self.conn,
            &self.data_dir,
            &hash,
            blob_id.as_deref(),
            &files,
            settings.max_file_bytes,
            settings.max_favorite_file_bytes,
        )?;
        // 只有确实存在实体目录时才把 blob_id 写回（超限/失败时保留「仅路径」语义）；
        // copy_state（每个文件的副本状态）无论成败都写回，便于 UI 展示与「重试」。
        let has_entity = copy
            .blob_id
            .as_deref()
            .map(|blob_id| blob::favorite_files_dir(&self.data_dir, blob_id).is_dir())
            .unwrap_or(false);
        if has_entity || copy.files.iter().any(|file| file.copy_state.is_some()) {
            let json = serde_json::to_string(&copy.files)
                .map_err(|err| StoreError::Msg(format!("文件清单序列化失败：{err}")))?;
            if has_entity {
                if let Some(blob_id) = copy.blob_id.as_deref() {
                    self.conn.execute(
                        "UPDATE clips SET files_json = ?1, blob_id = COALESCE(blob_id, ?2) WHERE id = ?3",
                        params![json, blob_id, id],
                    )?;
                }
            } else {
                self.conn.execute(
                    "UPDATE clips SET files_json = ?1 WHERE id = ?2",
                    params![json, id],
                )?;
            }
        }
        Ok(Some(copy))
    }

    /// 手动删除：移入回收站，不释放 blob/文件实体，等待 2 天后清理。
    pub fn soft_delete(&self, ids: &[ClipId]) -> StoreResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let now = now_ms();
        let mut deleted = 0usize;
        for id in ids {
            deleted += tx.execute(
                "UPDATE clips SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
                params![now, id],
            )?;
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// 从回收站恢复，并把 `last_used_at` 推到当前历史顶部，恢复后立即可见。
    pub fn restore(&self, ids: &[ClipId]) -> StoreResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let now = now_ms();
        let mut clock: i64 = tx.query_row(
            "SELECT COALESCE(MAX(last_used_at), 0) FROM clips",
            [],
            |row| row.get(0),
        )?;
        clock = clock.max(now);
        let mut restored = 0usize;
        for id in ids {
            let changed = tx.execute(
                "UPDATE clips
                 SET deleted_at = NULL, last_used_at = ?1
                 WHERE id = ?2 AND deleted_at IS NOT NULL",
                params![clock, id],
            )?;
            if changed > 0 {
                restored += 1;
                clock += 1;
            }
        }
        tx.commit()?;
        Ok(restored)
    }

    /// 彻底删除回收站条目（只接受 deleted_at 非空的行），并释放 blob/文件实体。
    pub fn purge(&self, ids: &[ClipId]) -> StoreResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut blob_ids: Vec<String> = Vec::new();
        for id in ids {
            let blob_id: Option<Option<String>> = tx
                .query_row(
                    "SELECT blob_id FROM clips WHERE id = ?1 AND deleted_at IS NOT NULL",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(Some(blob_id)) = blob_id {
                blob_ids.push(blob_id);
            }
        }
        let mut purged = 0usize;
        for id in ids {
            purged += tx.execute(
                "DELETE FROM clips WHERE id = ?1 AND deleted_at IS NOT NULL",
                params![id],
            )?;
        }
        blob_ids.sort();
        blob_ids.dedup();
        for blob_id in blob_ids {
            blob::release_blob(&tx, &self.data_dir, &blob_id)?;
        }
        tx.commit()?;
        Ok(purged)
    }

    /// 清空回收站。
    pub fn empty_trash(&self) -> StoreResult<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM clips WHERE deleted_at IS NOT NULL")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, ClipId>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        self.purge(&ids)
    }

    /// 自动清理超过保留期的回收站条目（启动、统计刷新、定期 prune 时调用）。
    pub fn purge_expired_trash(&self, now: i64, retention_ms: i64) -> StoreResult<usize> {
        let cutoff = now.saturating_sub(retention_ms);
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM clips WHERE deleted_at IS NOT NULL AND deleted_at <= ?1")?;
        let ids = stmt
            .query_map(params![cutoff], |row| row.get::<_, ClipId>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        self.purge(&ids)
    }

    /// 删除若干条目（FTS 触发器同步；引用计数归零的 blob 文件立即物理删除）。
    pub fn delete(&self, ids: &[ClipId]) -> StoreResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut blob_ids: Vec<String> = Vec::new();
        for id in ids {
            let blob_id: Option<Option<String>> = tx
                .query_row(
                    "SELECT blob_id FROM clips WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(Some(blob_id)) = blob_id {
                blob_ids.push(blob_id);
            }
        }
        let mut deleted = 0usize;
        for id in ids {
            deleted += tx.execute("DELETE FROM clips WHERE id = ?1", params![id])?;
        }
        for blob_id in blob_ids {
            blob::release_blob(&tx, &self.data_dir, &blob_id)?;
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// 清空历史（A2）：默认只删未收藏记录并保留收藏文件副本。
    ///
    /// - `include_favorites=false`：只删 `favorited = 0` 的行（含回收站中的未收藏项），
    ///   收藏项、收藏文件实体副本目录和它们的 blob 行全部原样保留；
    /// - `include_favorites=true`：连同收藏一起删除，并打开 `secure_delete`
    ///   并在提交后 `VACUUM`，避免被删除的文本残留在数据库空闲页中；
    /// - `delete_blobs=true` 时，被删除记录引用的图片/文件实体一并物理删除
    ///   （收藏项若保留，其被共享的 blob 因引用计数未归零而不会被删）。
    pub fn clear(&self, delete_blobs: bool, include_favorites: bool) -> StoreResult<ClearOutcome> {
        // 含收藏的清空会把历史文本从页中抹掉：先开启 secure_delete。
        if include_favorites {
            let _ = self.conn.execute_batch("PRAGMA secure_delete = ON;");
        }
        let tx = self.conn.unchecked_transaction()?;
        // 1) 收集待删除行的 blob 引用（事务内读取，避免并发变化）。
        let sql = if include_favorites {
            "SELECT id, blob_id FROM clips"
        } else {
            "SELECT id, blob_id FROM clips WHERE favorited = 0"
        };
        let mut stmt = tx.prepare(sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, ClipId>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let kept: usize = if include_favorites {
            0
        } else {
            tx.query_row(
                "SELECT COUNT(*) FROM clips WHERE favorited = 1",
                [],
                |row| row.get::<_, i64>(0),
            )? as usize
        };
        // 2) 删除行（FTS 触发器同步删除索引）。
        let cleared = if include_favorites {
            tx.execute("DELETE FROM clips", [])?;
            rows.len()
        } else {
            tx.execute("DELETE FROM clips WHERE favorited = 0", [])?
        };
        // 3) blob 引用：删除实体文件时走 release（引用计数归零才物理删除），
        //    不删实体时只清空元数据行，保留磁盘文件（由 cleanup_orphans/设置决定）。
        let mut blob_ids: Vec<String> = rows.into_iter().filter_map(|(_, blob)| blob).collect();
        blob_ids.sort();
        blob_ids.dedup();
        for blob_id in &blob_ids {
            if delete_blobs {
                blob::release_blob(&tx, &self.data_dir, blob_id)?;
            } else {
                tx.execute("DELETE FROM blobs WHERE id = ?1", params![blob_id])?;
            }
        }
        // 4) 没有被任何剩余条目引用的 blob 行一并清掉（不删磁盘文件，除非 delete_blobs）。
        if include_favorites && !delete_blobs {
            tx.execute("DELETE FROM blobs", [])?;
        }
        tx.commit()?;
        // 5) 含收藏的清空后压缩数据库，抹掉空闲页中的残留内容。
        if include_favorites {
            let _ = self.conn.execute_batch("VACUUM;");
            let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        }
        // 5.1) 删除实体且没有保留任何条目：清空 blobs 目录，顺带清掉旧版本遗留的孤儿文件。
        if delete_blobs && kept == 0 {
            if let Err(err) = blob::remove_all_blob_dirs(&self.data_dir) {
                crate::logging::warn(&format!("清空 blob 目录失败：{err}"));
            }
        }
        crate::logging::log(&format!(
            "清空历史：删除 {cleared} 条、保留收藏 {kept} 条、{} 个 blob 实体",
            blob_ids.len()
        ));
        Ok(ClearOutcome {
            cleared,
            kept,
            blobs: blob_ids.len(),
        })
    }
}

/// 判断错误是否为「数据库损坏 / 非数据库文件」。
fn db_corrupted(err: &StoreError) -> bool {
    match err {
        StoreError::Db(rusqlite::Error::SqliteFailure(inner, _)) => matches!(
            inner.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        ),
        _ => false,
    }
}

/// `PRAGMA quick_check`（spec §14 启动检测）。
fn quick_check(conn: &Connection) -> StoreResult<bool> {
    match conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)) {
        Ok(value) => Ok(value.eq_ignore_ascii_case("ok")),
        Err(err) => {
            let wrapped = StoreError::Db(err);
            if db_corrupted(&wrapped) {
                Ok(false)
            } else {
                Err(wrapped)
            }
        }
    }
}

/// 把损坏的库文件重命名为 `.bad-<时间戳>` 备份，返回主库备份路径。
fn backup_database(db_path: &Path) -> StoreResult<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut main_backup = PathBuf::new();
    for suffix in ["", "-wal", "-shm"] {
        let src = sibling_path(db_path, suffix);
        if !src.exists() {
            continue;
        }
        let dst = sibling_path(db_path, &format!("{suffix}.bad-{stamp}"));
        match std::fs::rename(&src, &dst) {
            Ok(()) => {
                if suffix.is_empty() {
                    main_backup = dst;
                }
            }
            Err(err) => crate::logging::warn(&format!("备份损坏文件失败 {}：{err}", src.display())),
        }
    }
    if main_backup.as_os_str().is_empty() {
        main_backup = db_path.to_path_buf();
    }
    Ok(main_backup)
}

/// 在路径后追加后缀（Windows 下用于 `data.db` → `data.db-wal`）。
pub(crate) fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

/// 日志里只出现 hash 前 8 位（不泄露内容）。
fn short_hash(hash: &str) -> String {
    hash.chars().take(8).collect()
}

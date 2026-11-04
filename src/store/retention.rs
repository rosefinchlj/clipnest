//! 容量治理（retention）：条数超限、图片总量超限、孤儿 blob 清理、收藏文件实体上限。
//!
//! 触发点（CONTRACT §2.2）：Store 线程启动后、每 100 次写入、`RefreshStats` 时。
//! 回收站条目超过 2 天时也会在这里物理删除。
//! 规则（spec §10.4）：
//! 1. 条数 > `max_items` → 删最旧的非收藏条目；
//! 2. 图片总量 > `max_image_bytes` → 按最旧优先删非收藏图片条目；
//! 3. 收藏文件实体总量超限 → **只停止新增副本，绝不删除已有收藏实体**（保护用户数据）；
//! 4. 清理 `refs <= 0` 的孤儿 blob（文件 + 记录）。

use rusqlite::params;

use crate::model::{now_ms, Settings, TRASH_RETENTION_MS};
use crate::store::blob;
use crate::store::db::Db;
use crate::store::StoreResult;

/// 一次清理的结果（用于日志/UI 提示/测试断言）。
#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    /// 本次删除的条目数。
    pub deleted_clips: usize,
    /// 本次因超过 2 天保留期而清理的回收站条目数。
    pub purged_trash: usize,
    /// 本次删除的孤儿 blob 数。
    pub deleted_blobs: usize,
    /// 随孤儿 blob 删除释放的字节数。
    pub freed_bytes: u64,
    /// 收藏文件实体总量是否已达上限（达上限后不再复制新实体）。
    pub favorites_limit_hit: bool,
    /// 清理后的条目总数。
    pub total: usize,
    /// 清理后的图片占用。
    pub image_bytes: u64,
    /// 清理后的收藏文件实体占用。
    pub favorite_file_bytes: u64,
}

/// 执行一次容量治理。`max_items` / `max_image_bytes` 为 0 视为不限制；
/// `max_favorite_file_bytes` 为 0 表示不再复制实体（只保留路径）。
pub fn prune(db: &Db, settings: &Settings) -> StoreResult<PruneReport> {
    let mut report = PruneReport::default();
    report.purged_trash = db.purge_expired_trash(now_ms(), TRASH_RETENTION_MS)?;
    if report.purged_trash > 0 {
        crate::logging::log(&format!(
            "retention: 回收站超过 2 天，物理删除 {} 条",
            report.purged_trash
        ));
    }

    // 1. 条数超限：删最旧的非收藏条目
    let total_before: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM clips WHERE deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    if settings.max_items > 0 && total_before > settings.max_items as i64 {
        let excess = (total_before - settings.max_items as i64) as usize;
        let ids = select_oldest_non_favorite(db, excess)?;
        let deleted = db.delete(&ids)?;
        report.deleted_clips += deleted;
        crate::logging::log(&format!(
            "retention: 条数超限（{total_before} > {}），删除最旧非收藏 {deleted} 条",
            settings.max_items
        ));
    }

    // 2. 图片总量超限：按最旧优先删非收藏图片（spec 风险：只删非收藏引用）
    let image_bytes_before: i64 = db.conn.query_row(
        "SELECT COALESCE(SUM(byte_len), 0) FROM clips WHERE kind = 1 AND deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    if settings.max_image_bytes > 0 && image_bytes_before > settings.max_image_bytes as i64 {
        let mut excess = image_bytes_before - settings.max_image_bytes as i64;
        let mut ids: Vec<i64> = Vec::new();
        {
            let mut stmt = db.conn.prepare(
                "SELECT id, byte_len FROM clips
                 WHERE kind = 1 AND favorited = 0 AND deleted_at IS NULL
                 ORDER BY last_used_at ASC, id ASC",
            )?;
            let iter =
                stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
            for row in iter {
                let (id, bytes) = row?;
                if excess <= 0 {
                    break;
                }
                excess -= bytes.max(1);
                ids.push(id);
            }
        }
        let deleted = db.delete(&ids)?;
        report.deleted_clips += deleted;
        crate::logging::log(&format!(
            "retention: 图片占用超限（{image_bytes_before} > {} 字节），删除最旧图片条目 {deleted} 条",
            settings.max_image_bytes
        ));
    }

    // 3. 收藏文件实体：超限只记录标记，绝不清除已有实体
    let favorite_file_bytes = blob::favorite_file_bytes(&db.conn)?;
    report.favorites_limit_hit = settings.max_favorite_file_bytes > 0
        && favorite_file_bytes >= settings.max_favorite_file_bytes;
    if report.favorites_limit_hit {
        crate::logging::warn(&format!(
            "retention: 收藏文件实体已达上限（{favorite_file_bytes} ≥ {} 字节），后续不再复制新实体",
            settings.max_favorite_file_bytes
        ));
    }

    // 4. 孤儿 blob（引用计数归零）清理
    let orphans = blob::cleanup_orphans(&db.conn, &db.data_dir)?;
    report.deleted_blobs = orphans.blobs;
    report.freed_bytes = orphans.bytes;

    // 5. 刷新统计与 meta 缓存（spec §10.4：容量统计缓存，避免每帧全表 SUM）
    let (total, image_bytes): (i64, i64) = db.conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN kind = 1 THEN byte_len ELSE 0 END), 0)
         FROM clips WHERE deleted_at IS NULL",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    report.total = total.max(0) as usize;
    report.image_bytes = image_bytes.max(0) as u64;
    report.favorite_file_bytes = blob::favorite_file_bytes(&db.conn)?;
    db.meta_set("clip_count", &report.total.to_string())?;
    db.meta_set("image_bytes", &report.image_bytes.to_string())?;
    db.meta_set("fav_file_bytes", &report.favorite_file_bytes.to_string())?;
    db.meta_set("last_prune_at", &crate::model::now_ms().to_string())?;

    Ok(report)
}

/// 取最旧的非收藏条目 id（最多 `limit` 条）。
fn select_oldest_non_favorite(db: &Db, limit: usize) -> StoreResult<Vec<i64>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = db.conn.prepare(
        "SELECT id FROM clips WHERE favorited = 0 AND deleted_at IS NULL
         ORDER BY last_used_at ASC, id ASC LIMIT ?1",
    )?;
    let iter = stmt.query_map(params![limit as i64], |row| row.get::<_, i64>(0))?;
    let ids = iter.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

//! 查询侧：分页搜索（FTS5 trigram / LIKE 双路径）、单条读取、统计、粘贴载荷。
//!
//! 全部为 [`Db`] 的 `impl` 块，与 [`crate::store::db`] 分离以控制单文件行数。
//! 搜索实现要点（spec §11）：
//! - 空查询按 `pinned DESC, last_used_at DESC` 分页；
//! - 文本 ≥3 字符且 FTS 可用 → `clips_fts MATCH '"…"'`（双引号翻倍转义，避免注入）；
//! - 更短、含路径分隔符或 FTS 关闭 → `LIKE '%'||?||'%' ESCAPE '\'`（转义 `% _ \`）；
//! - 含 `/` 或 `\` 时额外用另一分隔符再匹配一次，路径分隔符等价（spec §11）。

use rusqlite::types::Value;
use rusqlite::params_from_iter;
use std::time::Instant;

use crate::model::{
    now_ms, ClipFilter, ClipId, ClipItem, ClipKind, FileEntry, PastePayload, SearchPage, SearchQuery,
    Stats,
};
use crate::store::blob;
use crate::store::db::Db;
use crate::store::StoreResult;

/// 条目列（顺序必须与 [`row_to_item`] 一致）。
const ITEM_COLUMNS: &str = "clips.id, clips.kind, clips.preview, clips.text, clips.files_json, \
     clips.blob_id, clips.byte_len, clips.width, clips.height, clips.created_at, \
     clips.last_used_at, clips.use_count, clips.pinned, clips.source_app, clips.source_title";

/// 统一排序：收藏优先、最近使用优先、id 兜底保证稳定分页。
const ORDER_BY: &str = "ORDER BY clips.pinned DESC, clips.last_used_at DESC, clips.id DESC";

impl Db {
    /// 读取单条（含 files 解析）。
    pub fn item(&self, id: ClipId) -> StoreResult<Option<ClipItem>> {
        use rusqlite::OptionalExtension;
        let sql = format!("SELECT {ITEM_COLUMNS} FROM clips WHERE clips.id = ?1");
        let item = self
            .conn
            .query_row(&sql, [id], row_to_item)
            .optional()?;
        Ok(item)
    }

    /// 分页搜索（返回 total 与耗时，供诊断）。
    pub fn search(&self, query: &SearchQuery) -> StoreResult<SearchPage> {
        let started = Instant::now();
        let mut conditions: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();

        match query.filter {
            ClipFilter::All => {}
            ClipFilter::Text => conditions.push("clips.kind = 0".to_string()),
            ClipFilter::Image => conditions.push("clips.kind = 1".to_string()),
            ClipFilter::Files => conditions.push("clips.kind = 2".to_string()),
            ClipFilter::Pinned => conditions.push("clips.pinned = 1".to_string()),
        }

        let text = query.text.trim();
        let mut join_fts = false;
        if !text.is_empty() {
            let has_separator = text.contains('/') || text.contains('\\');
            let use_fts = self.fts_enabled && text.chars().count() >= 3 && !has_separator;
            if use_fts {
                conditions.push("clips_fts MATCH ?".to_string());
                args.push(Value::Text(fts_phrase(text)));
                join_fts = true;
            } else if has_separator {
                // 路径分隔符 / 与 \ 等价：两种写法都查
                let swapped = swap_path_separators(text);
                conditions.push(
                    "(clips.preview LIKE ? ESCAPE '\\' OR clips.text LIKE ? ESCAPE '\\' \
                      OR clips.preview LIKE ? ESCAPE '\\' OR clips.text LIKE ? ESCAPE '\\')"
                        .to_string(),
                );
                let pattern = like_pattern(text);
                let pattern_alt = like_pattern(&swapped);
                args.push(Value::Text(pattern.clone()));
                args.push(Value::Text(pattern));
                args.push(Value::Text(pattern_alt.clone()));
                args.push(Value::Text(pattern_alt));
            } else {
                conditions.push(
                    "(clips.preview LIKE ? ESCAPE '\\' OR clips.text LIKE ? ESCAPE '\\')"
                        .to_string(),
                );
                let pattern = like_pattern(text);
                args.push(Value::Text(pattern.clone()));
                args.push(Value::Text(pattern));
            }
        }

        let from = if join_fts {
            "clips JOIN clips_fts ON clips_fts.rowid = clips.id"
        } else {
            "clips"
        };
        let where_sql = if conditions.is_empty() {
            "1".to_string()
        } else {
            conditions.join(" AND ")
        };

        let total_sql = format!("SELECT COUNT(*) FROM {from} WHERE {where_sql}");
        let total: i64 = self
            .conn
            .query_row(&total_sql, params_from_iter(args.clone()), |row| row.get(0))?;

        let limit = query.limit.clamp(1, 10_000) as i64;
        let offset = query.offset as i64;
        let items_sql = format!(
            "SELECT {ITEM_COLUMNS} FROM {from} WHERE {where_sql} {ORDER_BY} LIMIT ? OFFSET ?"
        );
        let mut item_args = args;
        item_args.push(Value::Integer(limit));
        item_args.push(Value::Integer(offset));
        let mut stmt = self.conn.prepare(&items_sql)?;
        let rows = stmt.query_map(params_from_iter(item_args), row_to_item)?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(SearchPage {
            gen: query.gen,
            total: total.max(0) as usize,
            items,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// 容量统计（RefreshStats / 底部提示条）。
    pub fn counts(&self) -> StoreResult<Stats> {
        let (total, text, image, files, pinned): (i64, i64, i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN kind = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN kind = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN kind = 2 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN pinned = 1 THEN 1 ELSE 0 END), 0)
             FROM clips",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let image_bytes: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(byte_len), 0) FROM clips WHERE kind = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(Stats {
            total: total.max(0) as usize,
            text: text.max(0) as usize,
            image: image.max(0) as usize,
            files: files.max(0) as usize,
            pinned: pinned.max(0) as usize,
            image_bytes: image_bytes.max(0) as u64,
            favorite_file_bytes: blob::favorite_file_bytes(&self.conn)?,
            db_bytes: self.db_file_bytes(),
        })
    }

    /// 组装回写剪贴板所需的载荷；收藏文件原路径失效时自动恢复到 `restored/`。
    pub fn paste_payload(&self, id: ClipId) -> StoreResult<Option<PastePayload>> {
        let Some(item) = self.item(id)? else {
            return Ok(None);
        };
        match item.kind {
            ClipKind::Text => Ok(Some(PastePayload {
                clip_id: item.id,
                kind: item.kind,
                text: item.text.clone().or_else(|| Some(item.preview.clone())),
                png_path: None,
                files: Vec::new(),
            })),
            ClipKind::Image => {
                let png_path = match item.blob_id.as_deref() {
                    Some(blob_id) => {
                        let rel: Option<String> = {
                            use rusqlite::OptionalExtension;
                            self.conn
                                .query_row(
                                    "SELECT rel_path FROM blobs WHERE id = ?1",
                                    [blob_id],
                                    |row| row.get(0),
                                )
                                .optional()?
                        };
                        Some(match rel {
                            Some(rel) => blob::blob_path(&self.data_dir, &rel),
                            None => blob::image_path(&self.data_dir, blob_id),
                        })
                    }
                    None => None,
                };
                Ok(Some(PastePayload {
                    clip_id: item.id,
                    kind: item.kind,
                    text: None,
                    png_path,
                    files: Vec::new(),
                }))
            }
            ClipKind::Files => {
                let mut files = item.files.clone();
                let mut changed = false;
                if let Some(blob_id) = item.blob_id.as_deref() {
                    let restored = blob::restore_favorite_files(&self.data_dir, blob_id, &mut files)?;
                    changed = restored > 0;
                }
                if changed {
                    let json = serde_json::to_string(&files).map_err(|err| {
                        crate::store::StoreError::Msg(format!("文件清单序列化失败：{err}"))
                    })?;
                    self.conn.execute(
                        "UPDATE clips SET files_json = ?1 WHERE id = ?2",
                        rusqlite::params![json, id],
                    )?;
                }
                Ok(Some(PastePayload {
                    clip_id: item.id,
                    kind: item.kind,
                    text: None,
                    png_path: None,
                    files,
                }))
            }
        }
    }

    /// 把条目置顶（粘贴后调用）：last_used_at=now、use_count+1。
    ///
    /// 同一毫秒内连续粘贴多条时，单纯用 now 会产生相同时间戳、排序退化为 id 顺序，
    /// 导致"刚粘贴的条目没有置顶"。这里保证时间戳严格大于当前最大值（逻辑时钟）。
    pub fn bump_used(&self, id: ClipId) -> StoreResult<bool> {
        let changed = self.conn.execute(
            "UPDATE clips
             SET last_used_at = MAX(?1, COALESCE((SELECT MAX(last_used_at) FROM clips), 0) + 1),
                 use_count = use_count + 1
             WHERE id = ?2",
            rusqlite::params![now_ms(), id],
        )?;
        Ok(changed > 0)
    }
}

/// 把 `clips` 行映射为 [`ClipItem`]（列顺序见 [`ITEM_COLUMNS`]）。
fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClipItem> {
    let kind_raw: i32 = row.get(1)?;
    let files_json: Option<String> = row.get(4)?;
    let files: Vec<FileEntry> = files_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    let width: Option<i64> = row.get(7)?;
    let height: Option<i64> = row.get(8)?;
    let use_count: i64 = row.get(11)?;
    let pinned: i64 = row.get(12)?;
    Ok(ClipItem {
        id: row.get(0)?,
        kind: ClipKind::from_i32(kind_raw).unwrap_or_default(),
        preview: row.get(2)?,
        text: row.get(3)?,
        files,
        blob_id: row.get(5)?,
        byte_len: row.get::<_, i64>(6)?.max(0) as u64,
        width: width.map(|v| v.max(0) as u32),
        height: height.map(|v| v.max(0) as u32),
        created_at: row.get(9)?,
        last_used_at: row.get(10)?,
        use_count: use_count.max(0) as u32,
        pinned: pinned != 0,
        source_app: row.get(13)?,
        source_title: row.get(14)?,
    })
}

/// FTS5 短语查询：双引号内是精确子串；`"` 用 `""` 转义。
fn fts_phrase(text: &str) -> String {
    let escaped = text.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// LIKE 通配转义：`%`、`_`、`\` 前加 `\`（配合 `ESCAPE '\'`）。
fn like_pattern(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('%');
    for ch in text.chars() {
        match ch {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('%');
    out
}

/// 路径分隔符互换：用于 `/` 与 `\` 等价查询。
fn swap_path_separators(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '/' => '\\',
            '\\' => '/',
            other => other,
        })
        .collect()
}

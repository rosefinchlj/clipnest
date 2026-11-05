//! 查询侧：分页搜索（FTS5 trigram / LIKE 双路径）、单条读取、统计、粘贴载荷。
//!
//! 全部为 [`Db`] 的 `impl` 块，与 [`crate::store::db`] 分离以控制单文件行数。
//! 搜索实现要点（spec §11）：
//! - 普通列表按 `last_used_at DESC` 分页；收藏列表内按 `pinned DESC, last_used_at DESC` 分页；
//! - 文本 ≥3 字符且 FTS 可用 → `clips_fts MATCH '"…"'`（双引号翻倍转义，避免注入）；
//! - 更短、含路径分隔符或 FTS 关闭 → `LIKE '%'||?||'%' ESCAPE '\'`（转义 `% _ \`）；
//! - 含 `/` 或 `\` 时额外用另一分隔符再匹配一次，路径分隔符等价（spec §11）。

use rusqlite::params_from_iter;
use rusqlite::types::Value;
use std::path::Path;
use std::time::Instant;

use crate::model::{
    now_ms, ClipFilter, ClipId, ClipItem, ClipKind, FileEntry, PastePayload, SearchPage,
    SearchQuery, Stats,
};
use crate::store::blob;
use crate::store::db::Db;
use crate::store::StoreResult;

/// 条目列（顺序必须与 [`row_to_item`] 一致）。
const ITEM_COLUMNS: &str = "clips.id, clips.kind, clips.preview, clips.text, clips.files_json, \
     clips.blob_id, clips.byte_len, clips.width, clips.height, clips.created_at, \
     clips.last_used_at, clips.use_count, clips.favorited, clips.pinned, clips.source_app, \
     clips.source_title, clips.deleted_at";

/// 统一排序：最近使用优先、id 兜底保证稳定分页。
///
/// 置顶只在收藏列表内生效；普通列表保持纯时间序，避免收藏把新内容压下去。
const ORDER_BY: &str = "ORDER BY clips.last_used_at DESC, clips.id DESC";
/// 收藏列表排序：置顶优先，再按最近使用。
const FAVORITES_ORDER_BY: &str =
    "ORDER BY clips.pinned DESC, clips.last_used_at DESC, clips.id DESC";

impl Db {
    /// 读取单条（含 files 解析）。
    pub fn item(&self, id: ClipId) -> StoreResult<Option<ClipItem>> {
        use rusqlite::OptionalExtension;
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM clips WHERE clips.id = ?1 AND clips.deleted_at IS NULL"
        );
        let item = self.conn.query_row(&sql, [id], row_to_item).optional()?;
        Ok(item)
    }

    /// 分页搜索（返回 total 与耗时，供诊断）。
    pub fn search(&self, query: &SearchQuery) -> StoreResult<SearchPage> {
        let started = Instant::now();
        let mut conditions: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        conditions.push(if query.trash {
            "clips.deleted_at IS NOT NULL".to_string()
        } else {
            "clips.deleted_at IS NULL".to_string()
        });

        match query.filter {
            ClipFilter::All => {}
            ClipFilter::Text => conditions.push("clips.kind = 0".to_string()),
            ClipFilter::Image => conditions.push("clips.kind = 1".to_string()),
            ClipFilter::Files => conditions.push("clips.kind = 2".to_string()),
            ClipFilter::Favorites => conditions.push("clips.favorited = 1".to_string()),
        }

        if let Some((start, end)) = query.time_range.bounds(
            now_ms(),
            query.custom_day,
            query.custom_end_day,
        ) {
            // 时间列由筛选依据决定：最近使用（last_used_at）或复制时间（created_at）。
            let column = query.time_basis.column();
            conditions.push(format!("{column} >= ?"));
            args.push(Value::Integer(start));
            if let Some(end) = end {
                conditions.push(format!("{column} < ?"));
                args.push(Value::Integer(end));
            }
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
        let where_sql = conditions.join(" AND ");

        let total_sql = format!("SELECT COUNT(*) FROM {from} WHERE {where_sql}");
        let total: i64 =
            self.conn
                .query_row(&total_sql, params_from_iter(args.clone()), |row| row.get(0))?;

        let limit = query.limit.clamp(1, 10_000) as i64;
        let offset = query.offset as i64;
        let order_by = if query.trash {
            "ORDER BY clips.deleted_at DESC, clips.id DESC"
        } else if query.filter == ClipFilter::Favorites {
            FAVORITES_ORDER_BY
        } else {
            ORDER_BY
        };
        let items_sql = format!(
            "SELECT {ITEM_COLUMNS} FROM {from} WHERE {where_sql} {order_by} LIMIT ? OFFSET ?"
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
        let (total, text, image, files, favorited): (i64, i64, i64, i64, i64) =
            self.conn.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN kind = 0 THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN kind = 1 THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN kind = 2 THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN favorited = 1 THEN 1 ELSE 0 END), 0)
                 FROM clips WHERE deleted_at IS NULL",
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
            "SELECT COALESCE(SUM(byte_len), 0) FROM clips WHERE kind = 1 AND deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(Stats {
            total: total.max(0) as usize,
            text: text.max(0) as usize,
            image: image.max(0) as usize,
            files: files.max(0) as usize,
            favorited: favorited.max(0) as usize,
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
                    let restored =
                        blob::restore_favorite_files(&self.data_dir, blob_id, &mut files)?;
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
             WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![now_ms(), id],
        )?;
        Ok(changed > 0)
    }

    /// 图片另存为：把该条记录的图片 blob 复制到 `dest`。
    ///
    /// 返回 `Ok(false)` 表示记录不存在、类型不是图片或 blob 文件缺失（调用方提示即可）。
    pub fn save_image_as(&self, id: ClipId, dest: &Path) -> StoreResult<bool> {
        let Some(item) = self.item(id)? else {
            return Ok(false);
        };
        let Some(blob_id) = item.blob_id.as_deref() else {
            return Ok(false);
        };
        let src = self.image_blob_path(blob_id)?;
        if !src.is_file() {
            return Ok(false);
        }
        std::fs::copy(&src, dest)
            .map_err(|err| crate::store::StoreError::Msg(format!("保存图片失败：{err}")))?;
        Ok(true)
    }

    /// 文本另存为：把该条记录的文本内容以 UTF-8 写入 `dest`。
    ///
    /// 返回 `Ok(false)` 表示记录不存在或类型不是文本。
    pub fn save_text_as(&self, id: ClipId, dest: &Path) -> StoreResult<bool> {
        let Some(item) = self.item(id)? else {
            return Ok(false);
        };
        if item.kind != ClipKind::Text {
            return Ok(false);
        }
        let text = item.text.unwrap_or(item.preview);
        std::fs::write(dest, text.as_bytes())
            .map_err(|err| crate::store::StoreError::Msg(format!("保存文本失败：{err}")))?;
        Ok(true)
    }

    /// 图片 blob 的绝对路径：优先 `blobs.rel_path`，缺失时按约定路径回退。
    fn image_blob_path(&self, blob_id: &str) -> StoreResult<std::path::PathBuf> {
        use rusqlite::OptionalExtension;
        let rel: Option<String> = self
            .conn
            .query_row(
                "SELECT rel_path FROM blobs WHERE id = ?1",
                [blob_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match rel {
            Some(rel) => blob::blob_path(&self.data_dir, &rel),
            None => blob::image_path(&self.data_dir, blob_id),
        })
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
    let favorited: i64 = row.get(12)?;
    let pinned: i64 = row.get(13)?;
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
        favorited: favorited != 0,
        pinned: pinned != 0,
        source_app: row.get(14)?,
        source_title: row.get(15)?,
        deleted_at: row.get(16)?,
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

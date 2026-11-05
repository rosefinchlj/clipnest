//! 内容寻址 blob 文件库：图片 PNG / 缩略图 / 收藏文件实体副本 / 恢复目录。
//!
//! 目录布局（spec §10.1）：
//! ```text
//! blobs/images/<xx>/<yy>/<sha256>.png   原图（内容寻址，相同图片只落盘一次）
//! blobs/thumbs/<xx>/<yy>/<sha256>.jpg   96px 缩略图（JPEG q80）
//! blobs/files/<blob_id>/<原文件名>       收藏文件实体副本
//! restored/<yyyyMMdd-HHmmss>/           原文件失效时恢复出来的副本
//! ```
//! 所有写盘操作先写临时文件再 rename，避免半截文件。

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{now_ms, CopyState, FileEntry};
use crate::store::StoreResult;

/// blobs.kind：1 = 图片，2 = 收藏文件实体。
pub const KIND_IMAGE: i32 = 1;
pub const KIND_FILES: i32 = 2;
/// 缩略图最长边（spec §12.1）。
pub const THUMB_MAX_EDGE: u32 = 96;

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

pub fn blobs_root(data_dir: &Path) -> PathBuf {
    data_dir.join("blobs")
}

pub fn images_dir(data_dir: &Path) -> PathBuf {
    blobs_root(data_dir).join("images")
}

pub fn thumbs_dir(data_dir: &Path) -> PathBuf {
    blobs_root(data_dir).join("thumbs")
}

pub fn files_dir(data_dir: &Path) -> PathBuf {
    blobs_root(data_dir).join("files")
}

pub fn restored_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("restored")
}

/// blob_id 的 2 级散列目录片段；非法 id 兜底为 "00"。
fn shard(blob_id: &str) -> (&str, &str) {
    let first = blob_id.get(0..2).unwrap_or("00");
    let second = blob_id.get(2..4).unwrap_or("00");
    (first, second)
}

/// 相对 `blobs/` 的图片路径（入库用，统一用 `/` 分隔）。
pub fn image_rel_path(blob_id: &str) -> String {
    let (a, b) = shard(blob_id);
    format!("images/{a}/{b}/{blob_id}.png")
}

/// 相对 `blobs/` 的缩略图路径。
pub fn thumb_rel_path(blob_id: &str) -> String {
    let (a, b) = shard(blob_id);
    format!("thumbs/{a}/{b}/{blob_id}.jpg")
}

/// 图片绝对路径。
pub fn image_path(data_dir: &Path, blob_id: &str) -> PathBuf {
    blobs_root(data_dir).join(image_rel_path(blob_id))
}

/// 缩略图绝对路径。
pub fn thumb_path(data_dir: &Path, blob_id: &str) -> PathBuf {
    blobs_root(data_dir).join(thumb_rel_path(blob_id))
}

/// 由 blobs.rel_path 得到绝对路径。
pub fn blob_path(data_dir: &Path, rel_path: &str) -> PathBuf {
    blobs_root(data_dir).join(rel_path)
}

/// 收藏文件实体目录。
pub fn favorite_files_dir(data_dir: &Path, blob_id: &str) -> PathBuf {
    files_dir(data_dir).join(blob_id)
}

/// 创建所有 blob 相关目录（幂等）。
pub fn ensure_dirs(data_dir: &Path) -> std::io::Result<()> {
    for dir in [
        blobs_root(data_dir),
        images_dir(data_dir),
        thumbs_dir(data_dir),
        files_dir(data_dir),
        restored_dir(data_dir),
    ] {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// SHA-256 十六进制小写（内容寻址 id）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest.as_slice() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// 图片
// ---------------------------------------------------------------------------

/// 写入图片（内容寻址）：文件只落盘一次，blobs 行引用计数 +1。返回 blob_id。
pub fn put_image(conn: &Connection, data_dir: &Path, png: &[u8]) -> StoreResult<String> {
    let id = sha256_hex(png);
    let abs = image_path(data_dir, &id);
    if !abs.exists() {
        write_atomic(&abs, png)?;
    }
    // 缩略图失败不影响主流程（UI 可回退原图/占位）
    if let Err(err) = write_thumbnail(data_dir, &id, png) {
        crate::logging::warn(&format!("缩略图生成失败 blob={}…：{err}", short_id(&id)));
    }
    conn.execute(
        "INSERT INTO blobs (id, kind, rel_path, byte_len, sha256, refs, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
         ON CONFLICT(id) DO UPDATE SET refs = refs + 1",
        params![
            id,
            KIND_IMAGE,
            image_rel_path(&id),
            png.len() as i64,
            id,
            now_ms()
        ],
    )?;
    Ok(id)
}

/// 生成 96px 缩略图（JPEG q80）；已存在则跳过。
fn write_thumbnail(data_dir: &Path, blob_id: &str, png: &[u8]) -> StoreResult<()> {
    let dest = thumb_path(data_dir, blob_id);
    if dest.exists() {
        return Ok(());
    }
    let image = image::load_from_memory(png)
        .map_err(|err| crate::store::StoreError::Msg(format!("缩略图解码失败：{err}")))?;
    let thumb = image.thumbnail(THUMB_MAX_EDGE, THUMB_MAX_EDGE).to_rgb8();
    let mut buf: Vec<u8> = Vec::new();
    {
        use image::ImageEncoder;
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80);
        encoder
            .write_image(
                thumb.as_raw(),
                thumb.width(),
                thumb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|err| crate::store::StoreError::Msg(format!("缩略图编码失败：{err}")))?;
    }
    write_atomic(&dest, &buf)
}

/// 先写 `.tmp` 再 rename，避免进程中断留下半截文件。
fn write_atomic(path: &Path, bytes: &[u8]) -> StoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    if let Err(err) = fs::rename(&tmp, path) {
        // 目标已存在时 rename 在 Windows 会失败，此时直接覆盖写
        let _ = fs::remove_file(&tmp);
        fs::write(path, bytes)?;
        crate::logging::debug(&format!("blob 原子替换失败，改为直接写入：{err}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 引用计数 / 孤儿清理
// ---------------------------------------------------------------------------

/// 引用计数 -1；归零时删除 blob 行与物理文件。
pub fn release_blob(conn: &Connection, data_dir: &Path, blob_id: &str) -> StoreResult<()> {
    let changed = conn.execute(
        "UPDATE blobs SET refs = refs - 1 WHERE id = ?1",
        params![blob_id],
    )?;
    if changed == 0 {
        return Ok(());
    }
    let row: Option<(i32, String, i64)> = {
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT kind, rel_path, refs FROM blobs WHERE id = ?1",
            params![blob_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
    };
    let Some((kind, rel_path, refs)) = row else {
        return Ok(());
    };
    if refs > 0 {
        return Ok(());
    }
    conn.execute("DELETE FROM blobs WHERE id = ?1", params![blob_id])?;
    remove_blob_files(data_dir, kind, &rel_path, blob_id);
    Ok(())
}

/// 删除 blob 对应的物理文件/目录。
pub fn remove_blob_files(data_dir: &Path, kind: i32, rel_path: &str, blob_id: &str) {
    if kind == KIND_IMAGE {
        let _ = fs::remove_file(blob_path(data_dir, rel_path));
        let _ = fs::remove_file(thumb_path(data_dir, blob_id));
    } else if kind == KIND_FILES {
        let _ = fs::remove_dir_all(blob_path(data_dir, rel_path));
    }
}

/// 孤儿 blob 清理统计。
#[derive(Debug, Clone, Default)]
pub struct OrphanReport {
    pub blobs: usize,
    pub bytes: u64,
}

/// 清理孤儿 blob：引用计数归零，或没有任何 clips 引用的残留（写入中断产生的）。
pub fn cleanup_orphans(conn: &Connection, data_dir: &Path) -> StoreResult<OrphanReport> {
    let rows: Vec<(String, i32, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT id, kind, rel_path, byte_len FROM blobs
             WHERE refs <= 0
                OR NOT EXISTS (SELECT 1 FROM clips WHERE clips.blob_id = blobs.id)",
        )?;
        let iter = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        iter.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut report = OrphanReport::default();
    for (id, kind, rel_path, bytes) in rows {
        conn.execute("DELETE FROM blobs WHERE id = ?1", params![id])?;
        remove_blob_files(data_dir, kind, &rel_path, &id);
        report.blobs += 1;
        report.bytes += bytes.max(0) as u64;
    }
    Ok(report)
}

/// 收藏文件实体总占用（blobs.kind = 2）。
pub fn favorite_file_bytes(conn: &Connection) -> StoreResult<u64> {
    let sum: i64 = conn.query_row(
        "SELECT COALESCE(SUM(byte_len), 0) FROM blobs WHERE kind = ?1",
        params![KIND_FILES],
        |row| row.get(0),
    )?;
    Ok(sum.max(0) as u64)
}

/// 删除所有 blob 物理目录（清空历史 + 删除副本时使用）。
pub fn remove_all_blob_dirs(data_dir: &Path) -> std::io::Result<()> {
    for dir in [
        images_dir(data_dir),
        thumbs_dir(data_dir),
        files_dir(data_dir),
    ] {
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 收藏文件实体复制
// ---------------------------------------------------------------------------

/// 收藏文件复制结果。
#[derive(Debug, Clone, Default)]
pub struct FavoriteCopy {
    /// 收藏文件实体目录 id（即使没有复制成功也可能已存在）。
    pub blob_id: Option<String>,
    /// 更新后的文件清单（stored 标记与 copy_state 已刷新）。
    pub files: Vec<FileEntry>,
    pub copied: usize,
    pub copied_bytes: u64,
    pub skipped_missing: usize,
    pub skipped_too_large: usize,
    /// 复制过程中出错的文件数（权限/磁盘等，可重试）。
    pub failed: usize,
    /// 因总量超限而未复制（spec §10.4：只停止新增，绝不删已有）。
    pub limit_hit: bool,
    /// 当前收藏文件实体总占用。
    pub total_bytes: u64,
}

impl FavoriteCopy {
    /// 汇总为单个状态，供 UI 徽标/提示使用。
    pub fn overall_state(&self) -> CopyState {
        if self.failed > 0 {
            CopyState::Failed
        } else if self.limit_hit || self.skipped_too_large > 0 {
            CopyState::TooLarge
        } else if self.skipped_missing > 0 {
            // 部分文件缺失也要显式提示，避免用户以为副本已完整保存。
            CopyState::Missing
        } else if self.copied > 0 || self.files.iter().any(|f| f.stored) {
            CopyState::Stored
        } else if self.files.is_empty() {
            CopyState::NotNeeded
        } else {
            CopyState::Failed
        }
    }
}

/// 把文件清单中的文件复制进 `blobs/files/<blob_id>/`（收藏时调用）。
///
/// - `max_file_bytes`：单文件上限，0 表示不限制；
/// - `max_total_bytes`：收藏实体总量上限，0 表示禁止复制实体（只保留路径）。
pub fn copy_favorite_files(
    conn: &Connection,
    data_dir: &Path,
    clip_hash: &str,
    existing_blob_id: Option<&str>,
    files: &[FileEntry],
    max_file_bytes: u64,
    max_total_bytes: u64,
) -> StoreResult<FavoriteCopy> {
    let mut report = FavoriteCopy {
        files: files.to_vec(),
        total_bytes: favorite_file_bytes(conn)?,
        ..Default::default()
    };
    if max_total_bytes == 0 {
        report.limit_hit = true;
        for file in report.files.iter_mut() {
            if !file.stored {
                file.copy_state = Some(CopyState::TooLarge);
            } else {
                file.copy_state = Some(CopyState::Stored);
            }
        }
        return Ok(report);
    }
    // 已收藏过则沿用原目录 id，保证幂等
    let blob_id = existing_blob_id
        .map(str::to_string)
        .unwrap_or_else(|| favorite_blob_id(clip_hash, files));
    report.blob_id = Some(blob_id.clone());
    let dir = favorite_files_dir(data_dir, &blob_id);

    for file in report.files.iter_mut() {
        if file.stored && find_stored_copy(&dir, &file.name).is_some() {
            file.copy_state = Some(CopyState::Stored);
            continue; // 已保存过
        }
        // 上次复制成功但 files_json 里的 stored 标记丢失：按同名副本复用
        if let Some(existing) = find_stored_copy(&dir, &file.name) {
            let same_size = fs::metadata(&existing)
                .map(|meta| meta.len() == file.size)
                .unwrap_or(false);
            if same_size || file.size == 0 {
                file.stored = true;
                file.exists = true;
                file.copy_state = Some(CopyState::Stored);
                continue;
            }
        }
        let src = Path::new(&file.path);
        let Ok(meta) = fs::metadata(src) else {
            report.skipped_missing += 1;
            file.copy_state = Some(CopyState::Missing);
            continue;
        };
        if !meta.is_file() {
            report.skipped_missing += 1;
            file.copy_state = Some(CopyState::Missing);
            continue;
        }
        let size = meta.len();
        if max_file_bytes > 0 && size > max_file_bytes {
            report.skipped_too_large += 1;
            file.copy_state = Some(CopyState::TooLarge);
            continue;
        }
        if report.total_bytes.saturating_add(size) > max_total_bytes {
            report.limit_hit = true;
            file.copy_state = Some(CopyState::TooLarge);
            continue;
        }
        fs::create_dir_all(&dir)?;
        let dest = unique_dest(&dir, &file.name);
        match fs::copy(src, &dest) {
            Ok(copied_bytes) => {
                file.stored = true;
                file.exists = true;
                file.size = size;
                file.copy_state = Some(CopyState::Stored);
                report.copied += 1;
                report.copied_bytes += copied_bytes;
                report.total_bytes += copied_bytes;
            }
            Err(err) => {
                report.failed += 1;
                file.copy_state = Some(CopyState::Failed);
                crate::logging::warn(&format!(
                    "复制收藏文件失败（{}）：{err}",
                    short_name(&file.name)
                ));
            }
        }
    }

    // 登记/更新收藏文件实体 blob 行（refs 只在首次创建时为 1，超限时绝不删除已有实体）
    if report.copied > 0 || dir.is_dir() {
        let byte_len: u64 = collect_files(&dir)
            .iter()
            .filter_map(|path| fs::metadata(path).ok())
            .map(|meta| meta.len())
            .sum();
        conn.execute(
            "INSERT INTO blobs (id, kind, rel_path, byte_len, sha256, refs, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
             ON CONFLICT(id) DO UPDATE SET byte_len = excluded.byte_len",
            params![
                blob_id,
                KIND_FILES,
                format!("files/{blob_id}"),
                byte_len as i64,
                blob_id,
                now_ms()
            ],
        )?;
    }
    Ok(report)
}

/// 在 `blobs/files/<blob_id>/` 中查找原始文件名对应的副本。
pub fn find_stored_copy(dir: &Path, name: &str) -> Option<PathBuf> {
    let safe_name = sanitize_file_name(name);
    let exact = dir.join(&safe_name);
    if exact.is_file() {
        return Some(exact);
    }
    let all = collect_files(dir);
    if let Some(found) = all.iter().find(|path| {
        path.file_name()
            .map(|n| n == safe_name.as_str())
            .unwrap_or(false)
    }) {
        return Some(found.clone());
    }
    // 重名时复制名形如 `原名~2.ext`
    let prefix = format!("{safe_name}~");
    all.iter()
        .find(|path| {
            path.file_name()
                .map(|n| n.to_string_lossy().starts_with(&prefix))
                .unwrap_or(false)
        })
        .cloned()
}

/// 递归收集目录下所有文件。
pub(crate) fn collect_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 收藏文件恢复
// ---------------------------------------------------------------------------

/// 原文件失效时，把收藏实体复制到 `restored/<时间戳>/` 并更新清单中的路径。
/// 返回本次恢复的文件数。
pub fn restore_favorite_files(
    data_dir: &Path,
    blob_id: &str,
    files: &mut [FileEntry],
) -> StoreResult<usize> {
    let dir = favorite_files_dir(data_dir, blob_id);
    let mut restored = 0usize;
    let mut dest_root: Option<PathBuf> = None;
    for file in files.iter_mut() {
        if !file.stored {
            continue;
        }
        if Path::new(&file.path).exists() {
            file.exists = true;
            continue;
        }
        let Some(src) = find_stored_copy(&dir, &file.name) else {
            crate::logging::warn(&format!(
                "未找到收藏文件实体 blob={}… name={}",
                short_id(blob_id),
                short_name(&file.name)
            ));
            file.exists = false;
            continue;
        };
        if dest_root.is_none() {
            let root = restored_dir(data_dir)
                .join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
            fs::create_dir_all(&root)?;
            dest_root = Some(root);
        }
        let Some(root) = dest_root.as_ref() else {
            continue;
        };
        let dest = unique_dest(root, &file.name);
        fs::copy(&src, &dest)?;
        file.path = dest.to_string_lossy().to_string();
        file.exists = true;
        restored += 1;
    }
    if restored > 0 {
        crate::logging::log(&format!("收藏文件恢复 {restored} 个到恢复目录"));
    }
    Ok(restored)
}

// ---------------------------------------------------------------------------
// 文件名与路径辅助
// ---------------------------------------------------------------------------

/// 日志用短 id（前 8 位，避免日志过长）。
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// 日志用截断文件名（只记录文件名，不输出完整路径）。
fn short_name(name: &str) -> String {
    crate::model::truncate_chars(name, 48)
}

/// 过滤 Windows 文件名非法字符。
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| if "\\/:*?\"<>|".contains(ch) { '_' } else { ch })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').trim();
    if trimmed.is_empty() {
        format!("file-{}", now_ms())
    } else {
        trimmed.to_string()
    }
}

/// 目标目录下的唯一文件名（重名追加 `~N`）。
pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let safe_name = sanitize_file_name(name);
    let candidate = dir.join(&safe_name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(&safe_name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for index in 2..1000 {
        let next = dir.join(format!("{stem}~{index}{ext}"));
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!("{stem}~{}{ext}", now_ms()))
}

/// 计算收藏文件实体 id（blob 目录名，由内容 hash + 路径清单决定）。
pub fn favorite_blob_id(clip_hash: &str, files: &[FileEntry]) -> String {
    let mut seed = String::with_capacity(clip_hash.len() + 64);
    seed.push_str(clip_hash);
    for file in files {
        seed.push('|');
        seed.push_str(&file.path);
    }
    sha256_hex(seed.as_bytes())
}

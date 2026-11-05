//! JSON 导出 / 导入（spec §8.1 第 14 条、CONTRACT §2.2 `ExportJson`）。
//!
//! 导出格式（v1）：
//! ```json
//! { "version": 1, "exported_at": 1730000000000,
//!   "clips": [ { "kind": 0, "hash": "…", "preview": "…", "text": "…",
//!                "files": [], "blob_id": null, "byte_len": 12,
//!                "created_at": 1730000000000, "last_used_at": …,
//!                "use_count": 3, "pinned": true, "source_app": "chrome.exe" } ] }
//! ```
//! 图片/文件实体不内嵌（只保留路径与 blob 引用）；导入时图片要求目标数据目录
//! 已有对应 blob 文件，否则跳过并计入 `skipped`（升级为内嵌备份留待 v1.1）。

#![allow(dead_code)] // 导入接口按 Task 5 要求先留好，v1 只有单测走导入路径

use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{now_ms, ClipInput, ClipKind, FileEntry};
use crate::store::blob;
use crate::store::db::{Db, UpsertOutcome};
use crate::store::{StoreError, StoreResult};

/// 当前导出文件格式版本。
pub const EXPORT_VERSION: u32 = 2;

/// 导出文档根。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportDoc {
    pub version: u32,
    pub exported_at: i64,
    #[serde(default)]
    pub clips: Vec<ExportClip>,
}

/// 单条记录（不含图片/文件实体）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportClip {
    pub kind: i32,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub text: Option<String>,
    /// 富文本 HTML fragment（可选字段，旧导出文件缺失时为 None）。
    #[serde(default)]
    pub html: Option<String>,
    /// RTF 原文（可选字段，旧导出文件缺失时为 None）。
    #[serde(default)]
    pub rtf: Option<String>,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub byte_len: u64,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    pub created_at: i64,
    #[serde(default)]
    pub last_used_at: i64,
    #[serde(default)]
    pub use_count: u32,
    /// 收藏（v2 起独立于 pinned）。
    #[serde(default)]
    pub favorited: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub source_app: Option<String>,
    #[serde(default)]
    pub source_title: Option<String>,
}

/// 导出结果。
#[derive(Debug, Clone)]
pub struct ExportReport {
    pub path: PathBuf,
    pub clips: usize,
    pub bytes: u64,
}

/// 导入结果。
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub inserted: usize,
    pub touched: usize,
    pub skipped: usize,
}

/// 导出全部条目为 JSON 文件（原子写：先 tmp 再 rename）。
pub fn export_json(db: &Db, path: &Path) -> StoreResult<ExportReport> {
    let clips = collect_clips(db)?;
    let doc = ExportDoc {
        version: EXPORT_VERSION,
        exported_at: now_ms(),
        clips,
    };
    let json = serde_json::to_string_pretty(&doc)
        .map_err(|err| StoreError::Msg(format!("导出序列化失败：{err}")))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json.as_bytes())?;
    fs::rename(&tmp, path)?;
    let report = ExportReport {
        path: path.to_path_buf(),
        clips: doc.clips.len(),
        bytes: json.len() as u64,
    };
    crate::logging::log(&format!(
        "Store: 导出 {} 条记录到 {}（{} 字节）",
        report.clips,
        path.display(),
        report.bytes
    ));
    Ok(report)
}

/// 从 JSON 文件导入（按 hash 去重，幂等；图片缺失实体时跳过）。
pub fn import_json(db: &Db, path: &Path) -> StoreResult<ImportReport> {
    let raw = fs::read_to_string(path)?;
    let doc: ExportDoc = serde_json::from_str(&raw)
        .map_err(|err| StoreError::Msg(format!("导入文件格式错误：{err}")))?;
    if doc.version > EXPORT_VERSION {
        return Err(StoreError::Msg(format!(
            "导入文件版本 {} 高于本程序支持的 {EXPORT_VERSION}",
            doc.version
        )));
    }
    let mut report = ImportReport::default();
    for clip in doc.clips {
        let kind = ClipKind::from_i32(clip.kind).unwrap_or(ClipKind::Text);
        let hash = if clip.hash.is_empty() {
            import_hash(&clip)
        } else {
            clip.hash.clone()
        };
        // 图片/文件实体不内嵌：目标目录里没有实体时跳过
        let blob_id = match kind {
            ClipKind::Image => match clip.blob_id.as_deref() {
                Some(id) if blob::image_path(&db.data_dir, id).exists() => Some(id.to_string()),
                _ => {
                    report.skipped += 1;
                    continue;
                }
            },
            ClipKind::Files => clip
                .blob_id
                .clone()
                .filter(|id| blob::favorite_files_dir(&db.data_dir, id).exists()),
            ClipKind::Text => None,
        };
        let input = ClipInput {
            kind,
            hash,
            preview: clip.preview,
            text: clip.text,
            html: clip.html,
            rtf: clip.rtf,
            image_png: None,
            files: clip.files,
            byte_len: clip.byte_len,
            width: clip.width,
            height: clip.height,
            source_app: clip.source_app,
            source_title: clip.source_title,
            captured_at: clip.created_at,
        };
        match db.import_upsert(&input, blob_id)? {
            UpsertOutcome::Inserted(id) => {
                report.inserted += 1;
                // v1 导出的 pinned 实际是收藏；v2 起 favorited 与 pinned 独立。
                let legacy_favorite = doc.version < 2 && clip.pinned;
                let favorited = clip.favorited || legacy_favorite;
                let pinned = clip.pinned && favorited && !legacy_favorite;
                // 保留导出时的收藏/置顶 / 最近使用时间（导入是新行，需回写）
                db.conn.execute(
                    "UPDATE clips SET last_used_at = ?1, use_count = ?2, favorited = ?3, pinned = ?4 WHERE id = ?5",
                    params![
                        if clip.last_used_at > 0 {
                            clip.last_used_at
                        } else {
                            clip.created_at
                        },
                        i64::from(clip.use_count.max(1)),
                        i32::from(favorited),
                        i32::from(pinned),
                        id
                    ],
                )?;
            }
            UpsertOutcome::Touched(_) => report.touched += 1,
        }
    }
    crate::logging::log(&format!(
        "Store: 导入完成，新增 {} 条 / 已存在 {} 条 / 跳过 {} 条",
        report.inserted, report.touched, report.skipped
    ));
    Ok(report)
}

/// 收集所有条目（按创建时间正序，导出文件可读且稳定）。
fn collect_clips(db: &Db) -> StoreResult<Vec<ExportClip>> {
    let mut stmt = db.conn.prepare(
        "SELECT id, kind, hash, preview, text, files_json, blob_id, byte_len, width, height,
                created_at, last_used_at, use_count, favorited, pinned, source_app, source_title,
                html, rtf
         FROM clips ORDER BY created_at ASC, id ASC",
    )?;
    let iter = stmt.query_map([], |row| {
        let files_json: Option<String> = row.get(5)?;
        let files: Vec<FileEntry> = files_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        let width: Option<i64> = row.get(8)?;
        let height: Option<i64> = row.get(9)?;
        let use_count: i64 = row.get(12)?;
        let favorited: i64 = row.get(13)?;
        let pinned: i64 = row.get(14)?;
        Ok(ExportClip {
            kind: row.get(1)?,
            hash: row.get(2)?,
            preview: row.get(3)?,
            text: row.get(4)?,
            files,
            blob_id: row.get(6)?,
            byte_len: row.get::<_, i64>(7)?.max(0) as u64,
            width: width.map(|v| v.max(0) as u32),
            height: height.map(|v| v.max(0) as u32),
            created_at: row.get(10)?,
            last_used_at: row.get(11)?,
            use_count: use_count.max(0) as u32,
            favorited: favorited != 0,
            pinned: pinned != 0,
            source_app: row.get(15)?,
            source_title: row.get(16)?,
            html: row.get(17)?,
            rtf: row.get(18)?,
        })
    })?;
    let clips = iter.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(clips)
}

/// 为缺失 hash 的旧导出文件补算 hash（kind + 内容摘要）。
fn import_hash(clip: &ExportClip) -> String {
    let mut seed = format!("{}|{}", clip.kind, clip.preview);
    seed.push('|');
    if let Some(text) = clip.text.as_deref() {
        seed.push_str(text);
    }
    for file in &clip.files {
        seed.push('|');
        seed.push_str(&file.path);
    }
    blob::sha256_hex(seed.as_bytes())
}

/// 读取导出文件（测试用：验证往返内容）。
#[cfg(test)]
pub fn read_export(path: &Path) -> StoreResult<ExportDoc> {
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw).map_err(|err| StoreError::Msg(format!("导出文件解析失败：{err}")))
}

/// 判断导出文件中是否存在某 hash（测试用）。
#[cfg(test)]
pub fn contains_hash(doc: &ExportDoc, hash: &str) -> bool {
    doc.clips.iter().any(|clip| clip.hash == hash)
}

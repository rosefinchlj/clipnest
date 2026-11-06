//! 单文件完整备份 / 恢复。
//!
//! 备份是一个 zip 文件（默认扩展名 `.cnbak`），内容：
//! - `manifest.json`：格式版本、程序版本、创建时间、条目/图片/收藏文件数量、
//!   每个文件的大小与 sha256、总大小；
//! - `data.db`：SQLite 快照（优先 `VACUUM INTO`，失败时回退 `wal_checkpoint` + 复制）；
//! - `blobs/images/**`、`blobs/thumbs/**`：图片与缩略图；
//! - `blobs/files/**`：收藏时保存的实体文件副本。
//!
//! 恢复语义为「合并导入」：按内容 hash 去重，已存在的条目跳过；恢复前先完整
//! 校验 manifest 与每个文件的 sha256。所有数据先解压到临时 staging 目录，
//! 再在单个 SQLite 事务中合并；任何失败都会回滚事务并删除本次新建的文件，
//! 现有数据库与 blobs 不受影响。备份写入同样使用临时文件 + 原子替换。

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::model::{
    now_ms, BackupPreview, BackupReport, RestoreReport, BACKUP_DB_ENTRY, BACKUP_FORMAT_VERSION,
    BACKUP_MANIFEST, SCHEMA_VERSION,
};
use crate::store::blob;
use crate::store::db::Db;
use crate::store::{StoreError, StoreResult};

/// manifest 的格式标识（用于拒绝非 ClipNest 备份）。
const BACKUP_KIND: &str = "clipnest-backup";

/// 备份/恢复 staging 目录前缀。
const STAGING_PREFIX: &str = "clipnest-backup";

/// 备份文件内的文件条目类别。
const KIND_DB: &str = "db";
const KIND_IMAGE: &str = "image";
const KIND_THUMB: &str = "thumb";
const KIND_FILE: &str = "file";

// ---------------------------------------------------------------------------
// manifest
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    format: String,
    format_version: u32,
    app_version: String,
    created_at: i64,
    clips: usize,
    images: usize,
    favorite_files: usize,
    file_count: usize,
    total_bytes: u64,
    files: Vec<ManifestFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ManifestFile {
    path: String,
    size: u64,
    sha256: String,
    kind: String,
}

impl Manifest {
    fn has_db(&self) -> bool {
        self.files
            .iter()
            .any(|f| f.path == BACKUP_DB_ENTRY && f.kind == KIND_DB)
    }
}

/// 备份数据库中的 blob 行（只读取恢复所需字段）。
struct BackupBlob {
    id: String,
    kind: i32,
    rel_path: String,
    byte_len: i64,
    sha256: String,
    created_at: i64,
}

/// 备份数据库中的 clip 行（保留收藏/置顶/时间等状态）。
struct BackupClip {
    kind: i32,
    hash: String,
    text: Option<String>,
    preview: String,
    blob_id: Option<String>,
    files_json: Option<String>,
    byte_len: i64,
    width: Option<i64>,
    height: Option<i64>,
    created_at: i64,
    last_used_at: i64,
    use_count: i64,
    favorited: i64,
    pinned: i64,
    source_app: Option<String>,
    source_title: Option<String>,
}

// ---------------------------------------------------------------------------
// 创建备份
// ---------------------------------------------------------------------------

/// 创建单文件完整备份（先写临时文件，成功后原子替换目标）。
pub fn create_backup(db: &Db, path: &Path) -> StoreResult<BackupReport> {
    if path.as_os_str().is_empty() {
        return Err(StoreError::Msg("备份路径为空".to_string()));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let staging = staging_dir("create")?;
    let tmp_path = temp_sibling(path);
    if tmp_path.exists() {
        let _ = fs::remove_file(&tmp_path);
    }
    let result = create_backup_inner(db, path, &staging, &tmp_path);
    if result.is_err() {
        // 失败时不留下半截备份文件。
        let _ = fs::remove_file(&tmp_path);
    }
    let _ = fs::remove_dir_all(&staging);
    result
}

fn create_backup_inner(
    db: &Db,
    path: &Path,
    staging: &Path,
    tmp_path: &Path,
) -> StoreResult<BackupReport> {
    // 1) 数据库快照（与实际写入解耦，避免把 WAL 中途状态打进备份）。
    let snapshot = staging.join(BACKUP_DB_ENTRY);
    snapshot_database(db, &snapshot)?;

    // 2) 只备份仍被未删除条目引用的 blob（图片/缩略图/收藏文件副本）。
    let blobs = referenced_blobs(db)?;
    let clips = clip_count(db)?;

    let file = File::create(tmp_path)?;
    let mut writer = ZipWriter::new(BufWriter::new(file));
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    let mut manifest = Manifest {
        format: BACKUP_KIND.to_string(),
        format_version: BACKUP_FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: now_ms(),
        clips,
        images: 0,
        favorite_files: 0,
        file_count: 0,
        total_bytes: 0,
        files: Vec::new(),
    };

    // 3) 数据库快照。
    let entry = add_file_entry(&mut writer, BACKUP_DB_ENTRY, &snapshot, KIND_DB, deflated)?;
    push_manifest_file(&mut manifest, entry)?;

    // 4) blobs：图片 + 缩略图 + 收藏文件副本。
    let mut images = 0usize;
    let mut favorite_files = 0usize;
    for item in &blobs {
        match item.kind {
            blob::KIND_IMAGE => {
                let abs = blob::blob_path(&db.data_dir, &item.rel_path);
                if abs.is_file() {
                    let name = blob_entry_name(&item.rel_path);
                    let entry = add_file_entry(&mut writer, &name, &abs, KIND_IMAGE, stored)?;
                    push_manifest_file(&mut manifest, entry)?;
                    images += 1;
                } else {
                    crate::logging::warn(&format!(
                        "备份跳过缺失的图片文件 blob={}",
                        short(&item.id)
                    ));
                }
                let thumb_rel = blob::thumb_rel_path(&item.id);
                let thumb_abs = blob::blob_path(&db.data_dir, &thumb_rel);
                if thumb_abs.is_file() {
                    let name = blob_entry_name(&thumb_rel);
                    let entry = add_file_entry(&mut writer, &name, &thumb_abs, KIND_THUMB, stored)?;
                    push_manifest_file(&mut manifest, entry)?;
                }
            }
            blob::KIND_FILES => {
                let dir = blob::blob_path(&db.data_dir, &item.rel_path);
                if !dir.is_dir() {
                    crate::logging::warn(&format!(
                        "备份跳过缺失的收藏文件目录 blob={}",
                        short(&item.id)
                    ));
                    continue;
                }
                let mut paths = Vec::new();
                collect_files(&dir, &mut paths);
                paths.sort();
                for abs in paths {
                    let rel = match abs.strip_prefix(&dir) {
                        Ok(rel) => rel,
                        Err(_) => continue,
                    };
                    let inner = rel_to_entry(rel);
                    let name = format!("{}/{}", blob_entry_name(&item.rel_path), inner);
                    let entry = add_file_entry(&mut writer, &name, &abs, KIND_FILE, stored)?;
                    push_manifest_file(&mut manifest, entry)?;
                    favorite_files += 1;
                }
            }
            _ => {}
        }
    }
    manifest.images = images;
    manifest.favorite_files = favorite_files;

    // 5) manifest（放在最后写入，便于流式实现；zip 中央目录不要求顺序）。
    let json = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| StoreError::Msg(format!("manifest 序列化失败：{err}")))?;
    writer
        .start_file(BACKUP_MANIFEST, deflated)
        .map_err(|err| zip_error("写入 manifest", err))?;
    writer.write_all(&json)?;

    // 6) 收尾并原子替换目标文件。
    let buffered = writer
        .finish()
        .map_err(|err| zip_error("完成备份写入", err))?;
    let file = buffered
        .into_inner()
        .map_err(|err| StoreError::Msg(format!("刷新备份文件失败：{err}")))?;
    file.sync_all()?;
    drop(file);
    replace_file(tmp_path, path)?;
    crate::logging::log(&format!(
        "完整备份完成：{} 条，图片 {}，收藏文件副本 {}，{}",
        manifest.clips,
        manifest.images,
        manifest.favorite_files,
        path.display()
    ));
    Ok(BackupReport {
        clips: manifest.clips,
        images: manifest.images,
        favorite_files: manifest.favorite_files,
        bytes: manifest.total_bytes,
    })
}

fn push_manifest_file(manifest: &mut Manifest, file: ManifestFile) -> StoreResult<()> {
    if manifest.files.iter().any(|f| f.path == file.path) {
        return Err(StoreError::Msg(format!(
            "备份内部错误：重复文件 {}",
            file.path
        )));
    }
    manifest.total_bytes += file.size;
    manifest.files.push(file);
    Ok(())
}

/// 写入一个 zip 条目并同时计算 sha256。
fn add_file_entry<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entry_name: &str,
    src: &Path,
    kind: &str,
    options: SimpleFileOptions,
) -> StoreResult<ManifestFile> {
    let mut source = BufReader::new(File::open(src)?);
    writer
        .start_file(entry_name, options)
        .map_err(|err| zip_error("写入备份条目", err))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        writer.write_all(&buf[..read])?;
        size += read as u64;
    }
    Ok(ManifestFile {
        path: entry_name.to_string(),
        size,
        sha256: hex_digest(hasher.finalize().as_slice()),
        kind: kind.to_string(),
    })
}

/// 数据库快照：优先 `VACUUM INTO`（一致且紧凑），失败回退 checkpoint + 复制。
fn snapshot_database(db: &Db, dest: &Path) -> StoreResult<()> {
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    let dest_str = dest.to_string_lossy().to_string();
    match db.conn.execute("VACUUM INTO ?1", params![dest_str]) {
        Ok(_) => return Ok(()),
        Err(err) => {
            crate::logging::warn(&format!(
                "数据库快照 VACUUM INTO 失败（{err}），回退为 checkpoint + 复制"
            ));
        }
    }
    db.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    fs::copy(&db.db_path, dest)?;
    Ok(())
}

/// 查询仍被未删除条目引用的 blob 行。
fn referenced_blobs(db: &Db) -> StoreResult<Vec<BackupBlob>> {
    let mut stmt = db.conn.prepare(
        "SELECT b.id, b.kind, b.rel_path, b.byte_len, b.sha256, b.created_at
           FROM blobs b
          WHERE b.kind IN (?1, ?2)
            AND EXISTS (SELECT 1 FROM clips c
                         WHERE c.blob_id = b.id AND c.deleted_at IS NULL)
          ORDER BY b.id",
    )?;
    let rows = stmt.query_map(params![blob::KIND_IMAGE, blob::KIND_FILES], |row| {
        Ok(BackupBlob {
            id: row.get(0)?,
            kind: row.get(1)?,
            rel_path: row.get(2)?,
            byte_len: row.get(3)?,
            sha256: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn clip_count(db: &Db) -> StoreResult<usize> {
    let count: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM clips WHERE deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

fn blob_entry_name(rel_path: &str) -> String {
    format!("blobs/{}", rel_path.replace('\\', "/"))
}

fn rel_to_entry(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------------
// 检查备份（恢复前预览）
// ---------------------------------------------------------------------------

/// 打开备份并展示摘要；同时校验 manifest 与每个文件的 sha256。
pub fn inspect_backup(path: &Path, key_hex: Option<&str>) -> StoreResult<BackupPreview> {
    let staging = staging_dir("inspect")?;
    let result = inspect_inner(path, &staging, key_hex);
    let _ = fs::remove_dir_all(&staging);
    result
}

fn inspect_inner(path: &Path, staging: &Path, key_hex: Option<&str>) -> StoreResult<BackupPreview> {
    let mut archive = open_archive(path)?;
    let manifest = load_manifest(&mut archive)?;
    let (checksum_errors, checksum_note) = verify_archive_members(&mut archive, &manifest)?;

    // 解出数据库快照，核对条目数与结构（预览里显示真实可恢复数量）。
    let snapshot = staging.join(BACKUP_DB_ENTRY);
    extract_member(&mut archive, BACKUP_DB_ENTRY, &snapshot)?;
    let conn = open_snapshot(&snapshot, key_hex)?;
    let clips = snapshot_clip_count(&conn)?;
    drop(conn);

    let mut note = checksum_note;
    let mut errors = checksum_errors;
    if clips != manifest.clips {
        errors += 1;
        if note.is_empty() {
            note = format!(
                "manifest 记录 {} 条，数据库实际 {} 条",
                manifest.clips, clips
            );
        }
    }

    Ok(BackupPreview {
        path: path.to_path_buf(),
        format_version: manifest.format_version,
        app_version: manifest.app_version,
        created_at: manifest.created_at,
        clips,
        images: manifest.images,
        favorite_files: manifest.favorite_files,
        file_count: manifest.file_count,
        total_bytes: manifest.total_bytes,
        checksum_ok: errors == 0,
        checksum_errors: errors,
        checksum_note: note,
    })
}

fn open_archive(path: &Path) -> StoreResult<ZipArchive<BufReader<File>>> {
    let file =
        File::open(path).map_err(|err| StoreError::Msg(format!("无法打开备份文件：{err}")))?;
    ZipArchive::new(BufReader::new(file)).map_err(|err| zip_error("备份文件不是有效的 zip", err))
}

fn load_manifest(archive: &mut ZipArchive<BufReader<File>>) -> StoreResult<Manifest> {
    let mut member = archive
        .by_name(BACKUP_MANIFEST)
        .map_err(|err| zip_error("备份缺少 manifest.json", err))?;
    let mut buf = String::new();
    member
        .read_to_string(&mut buf)
        .map_err(|err| StoreError::Msg(format!("读取 manifest 失败：{err}")))?;
    drop(member);
    let manifest: Manifest = serde_json::from_str(&buf)
        .map_err(|err| StoreError::Msg(format!("manifest 解析失败：{err}")))?;
    if manifest.format != BACKUP_KIND {
        return Err(StoreError::Msg(
            "该文件不是 ClipNest 备份（manifest 格式标识不匹配）".to_string(),
        ));
    }
    if manifest.format_version > BACKUP_FORMAT_VERSION {
        return Err(StoreError::Msg(format!(
            "备份格式版本 v{} 高于当前程序支持的 v{}，请升级 ClipNest 后再恢复",
            manifest.format_version, BACKUP_FORMAT_VERSION
        )));
    }
    if !manifest.has_db() {
        return Err(StoreError::Msg(
            "备份缺少数据库快照（data.db），无法恢复".to_string(),
        ));
    }
    let mut seen = HashSet::new();
    for file in &manifest.files {
        if !seen.insert(file.path.as_str()) {
            return Err(StoreError::Msg(format!(
                "manifest 中存在重复文件：{}",
                file.path
            )));
        }
        safe_rel(&file.path)?;
    }
    Ok(manifest)
}

/// 流式校验每个条目的存在性、大小与 sha256；返回（失败数，首个失败原因）。
fn verify_archive_members(
    archive: &mut ZipArchive<BufReader<File>>,
    manifest: &Manifest,
) -> StoreResult<(usize, String)> {
    let mut errors = 0usize;
    let mut note = String::new();
    for file in &manifest.files {
        let result = verify_member(archive, file);
        if let Err(reason) = result {
            errors += 1;
            if note.is_empty() {
                note = format!("{}：{reason}", file.path);
            }
        }
    }
    Ok((errors, note))
}

fn verify_member(
    archive: &mut ZipArchive<BufReader<File>>,
    file: &ManifestFile,
) -> Result<(), String> {
    let mut member = archive
        .by_name(&file.path)
        .map_err(|_| "备份中缺少该文件".to_string())?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = member
            .read(&mut buf)
            .map_err(|err| format!("读取失败：{err}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        size += read as u64;
    }
    if size != file.size {
        return Err(format!("大小不符（期望 {}，实际 {}）", file.size, size));
    }
    if hex_digest(hasher.finalize().as_slice()) != file.sha256 {
        return Err("校验和不符".to_string());
    }
    Ok(())
}

fn extract_member(
    archive: &mut ZipArchive<BufReader<File>>,
    name: &str,
    dest: &Path,
) -> StoreResult<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut member = archive
        .by_name(name)
        .map_err(|err| zip_error("备份缺少所需文件", err))?;
    let mut out = BufWriter::new(File::create(dest)?);
    std::io::copy(&mut member, &mut out)?;
    out.flush()?;
    Ok(())
}

/// 解压全部内容到 staging，并在解压过程中复核大小与 sha256。
fn extract_all_verified(
    archive: &mut ZipArchive<BufReader<File>>,
    manifest: &Manifest,
    work: &Path,
) -> StoreResult<()> {
    for file in &manifest.files {
        let rel = safe_rel(&file.path)?;
        let dest = work.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut member = archive
            .by_name(&file.path)
            .map_err(|err| zip_error("备份中缺少文件", err))?;
        let mut out = BufWriter::new(File::create(&dest)?);
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let read = member
                .read(&mut buf)
                .map_err(|err| StoreError::Msg(format!("读取备份条目失败：{err}")))?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
            out.write_all(&buf[..read])?;
            size += read as u64;
        }
        out.flush()?;
        if size != file.size {
            return Err(StoreError::Msg(format!(
                "备份文件「{}」大小不符（期望 {}，实际 {}），已取消恢复",
                file.path, file.size, size
            )));
        }
        if hex_digest(hasher.finalize().as_slice()) != file.sha256 {
            return Err(StoreError::Msg(format!(
                "备份文件「{}」校验和不符，已取消恢复",
                file.path
            )));
        }
    }
    Ok(())
}

/// 打开备份里的数据库快照（只读）并做结构与版本检查。
fn open_snapshot(path: &Path, key_hex: Option<&str>) -> StoreResult<Connection> {
    let encrypted = crate::db_crypto::is_encrypted_file(path);
    if encrypted && key_hex.is_none() {
        return Err(StoreError::Msg(
            "备份中的数据库已加密，需要当前 Windows 用户密钥才能检查/恢复".to_string(),
        ));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // 明文备份不能带 key 打开：SQLCipher 对明文库执行 PRAGMA key 会报
    // `file is not a database`。是否加密以快照文件头为准，而不是当前库状态。
    if encrypted {
        if let Some(key) = key_hex {
            conn.execute_batch(&format!("PRAGMA key = \"x'{key}'\";"))?;
        }
    }
    let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(StoreError::Msg(
            "备份中的数据库快照损坏，无法恢复".to_string(),
        ));
    }
    let version: i64 = match conn.query_row(
        "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(_) => 0,
    };
    if version > SCHEMA_VERSION {
        return Err(StoreError::Msg(format!(
            "备份数据库版本 v{version} 高于当前程序支持的 v{SCHEMA_VERSION}，请升级 ClipNest 后再恢复"
        )));
    }
    require_columns(
        &conn,
        "clips",
        &[
            "kind",
            "hash",
            "text",
            "preview",
            "blob_id",
            "files_json",
            "byte_len",
            "width",
            "height",
            "created_at",
            "last_used_at",
            "use_count",
            "favorited",
            "pinned",
            "source_app",
            "source_title",
            "deleted_at",
        ],
    )?;
    require_columns(
        &conn,
        "blobs",
        &["id", "kind", "rel_path", "byte_len", "sha256", "created_at"],
    )?;
    Ok(conn)
}

fn require_columns(conn: &Connection, table: &str, required: &[&str]) -> StoreResult<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols: HashSet<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .collect();
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|name| !cols.contains(*name))
        .collect();
    if !missing.is_empty() {
        return Err(StoreError::Msg(format!(
            "备份数据库结构过旧（{table} 缺少字段：{}），无法恢复",
            missing.join("、")
        )));
    }
    Ok(())
}

fn snapshot_clip_count(conn: &Connection) -> StoreResult<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM clips WHERE deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

// ---------------------------------------------------------------------------
// 恢复（合并导入）
// ---------------------------------------------------------------------------

/// 从备份合并恢复；校验/解压/合并任一环节失败都不会改动现有数据。
pub fn restore_backup(db: &Db, path: &Path, key_hex: Option<&str>) -> StoreResult<RestoreReport> {
    let staging = staging_dir("restore")?;
    let result = restore_inner(db, path, &staging, key_hex);
    let _ = fs::remove_dir_all(&staging);
    result
}

fn restore_inner(db: &Db, path: &Path, staging: &Path, key_hex: Option<&str>) -> StoreResult<RestoreReport> {
    let mut archive = open_archive(path)?;
    let manifest = load_manifest(&mut archive)?;
    // 先完整解压并逐文件校验；只有全部通过才允许触碰现有数据库。
    extract_all_verified(&mut archive, &manifest, staging)?;

    let snapshot_path = staging.join(BACKUP_DB_ENTRY);
    let source = open_snapshot(&snapshot_path, key_hex)?;
    let clips = read_clips(&source)?;
    let blobs = read_blobs(&source)?;
    drop(source);

    let existing = existing_hashes(db)?;
    let tx = db.conn.unchecked_transaction()?;
    let mut guard = CleanupGuard::new();
    let mut report = RestoreReport {
        bytes: manifest.total_bytes,
        ..Default::default()
    };
    let mut inserted_hashes: HashSet<String> = HashSet::new();

    for clip in &clips {
        if existing.contains(&clip.hash) || inserted_hashes.contains(&clip.hash) {
            report.skipped += 1;
            continue;
        }
        let mut blob_id_out: Option<String> = None;
        if let Some(blob_id) = clip.blob_id.as_deref() {
            let Some(item) = blobs.get(blob_id) else {
                crate::logging::warn(&format!(
                    "恢复跳过条目 {}：备份中缺少 blob 登记 {}",
                    short(&clip.hash),
                    short(blob_id)
                ));
                report.skipped += 1;
                continue;
            };
            match item.kind {
                blob::KIND_IMAGE => {
                    let stage_file = staging.join(blob_entry_name(&item.rel_path));
                    if !stage_file.is_file() {
                        crate::logging::warn(&format!(
                            "恢复跳过条目 {}：备份中缺少图片文件",
                            short(&clip.hash)
                        ));
                        report.skipped += 1;
                        continue;
                    }
                    let dest = blob::blob_path(&db.data_dir, &item.rel_path);
                    copy_file_tracked(&stage_file, &dest, &mut guard)?;
                    // 缩略图尽力恢复；缺失时 UI 会回退原图。
                    let thumb_rel = blob::thumb_rel_path(&item.id);
                    let stage_thumb = staging.join(blob_entry_name(&thumb_rel));
                    if stage_thumb.is_file() {
                        let thumb_dest = blob::thumb_path(&db.data_dir, &item.id);
                        copy_file_tracked(&stage_thumb, &thumb_dest, &mut guard)?;
                    }
                    register_blob(&tx, item)?;
                    blob_id_out = Some(item.id.clone());
                    report.images += 1;
                }
                blob::KIND_FILES => {
                    let stage_dir = staging.join(blob_entry_name(&item.rel_path));
                    if !stage_dir.is_dir() {
                        crate::logging::warn(&format!(
                            "恢复跳过条目 {}：备份中缺少收藏文件副本",
                            short(&clip.hash)
                        ));
                        report.skipped += 1;
                        continue;
                    }
                    let dest_dir = blob::blob_path(&db.data_dir, &item.rel_path);
                    report.favorite_files += copy_dir_tracked(&stage_dir, &dest_dir, &mut guard)?;
                    register_blob(&tx, item)?;
                    blob_id_out = Some(item.id.clone());
                }
                _ => {}
            }
        }
        insert_clip(&tx, clip, blob_id_out.as_deref())?;
        inserted_hashes.insert(clip.hash.clone());
        report.inserted += 1;
    }

    tx.commit()?;
    guard.disarm();
    crate::logging::log(&format!(
        "从备份合并恢复完成：新增 {} 条，跳过重复/缺失 {} 条，图片 {}，收藏文件副本 {}，{}",
        report.inserted,
        report.skipped,
        report.images,
        report.favorite_files,
        path.display()
    ));
    Ok(report)
}

fn read_clips(conn: &Connection) -> StoreResult<Vec<BackupClip>> {
    let mut stmt = conn.prepare(
        "SELECT kind, hash, text, preview, blob_id, files_json, byte_len, width, height,
                created_at, last_used_at, use_count, favorited, pinned, source_app, source_title
           FROM clips
          WHERE deleted_at IS NULL
          ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(BackupClip {
            kind: row.get(0)?,
            hash: row.get(1)?,
            text: row.get(2)?,
            preview: row.get(3)?,
            blob_id: row.get(4)?,
            files_json: row.get(5)?,
            byte_len: row.get(6)?,
            width: row.get(7)?,
            height: row.get(8)?,
            created_at: row.get(9)?,
            last_used_at: row.get(10)?,
            use_count: row.get(11)?,
            favorited: row.get(12)?,
            pinned: row.get(13)?,
            source_app: row.get(14)?,
            source_title: row.get(15)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn read_blobs(conn: &Connection) -> StoreResult<HashMap<String, BackupBlob>> {
    let mut stmt =
        conn.prepare("SELECT id, kind, rel_path, byte_len, sha256, created_at FROM blobs")?;
    let rows = stmt.query_map([], |row| {
        Ok(BackupBlob {
            id: row.get(0)?,
            kind: row.get(1)?,
            rel_path: row.get(2)?,
            byte_len: row.get(3)?,
            sha256: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let item = row?;
        out.insert(item.id.clone(), item);
    }
    Ok(out)
}

fn existing_hashes(db: &Db) -> StoreResult<HashSet<String>> {
    let mut stmt = db.conn.prepare("SELECT hash FROM clips")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

/// 登记（或增加引用计数）blob，随恢复事务一起提交/回滚。
fn register_blob(conn: &Connection, item: &BackupBlob) -> StoreResult<()> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT refs FROM blobs WHERE id = ?1",
            params![item.id],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(refs) => {
            conn.execute(
                "UPDATE blobs SET refs = ?2 WHERE id = ?1",
                params![item.id, refs + 1],
            )?;
        }
        None => {
            conn.execute(
                "INSERT INTO blobs (id, kind, rel_path, byte_len, sha256, refs, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
                params![
                    item.id,
                    item.kind,
                    item.rel_path,
                    item.byte_len,
                    item.sha256,
                    item.created_at
                ],
            )?;
        }
    }
    Ok(())
}

/// 插入恢复的条目（保留原 created_at/last_used_at/use_count/收藏/置顶）。
fn insert_clip(conn: &Connection, clip: &BackupClip, blob_id: Option<&str>) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO clips
           (kind, hash, text, preview, blob_id, files_json, byte_len, width, height,
            created_at, last_used_at, use_count, favorited, pinned, source_app, source_title)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            clip.kind,
            clip.hash,
            clip.text,
            clip.preview,
            blob_id,
            clip.files_json,
            clip.byte_len,
            clip.width,
            clip.height,
            clip.created_at,
            clip.last_used_at,
            clip.use_count,
            clip.favorited,
            clip.pinned,
            clip.source_app,
            clip.source_title,
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 文件操作辅助
// ---------------------------------------------------------------------------

/// 失败时回滚本次新建文件/目录的守卫。
struct CleanupGuard {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    armed: bool,
}

impl CleanupGuard {
    fn new() -> Self {
        Self {
            files: Vec::new(),
            dirs: Vec::new(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for file in &self.files {
            let _ = fs::remove_file(file);
        }
        // 目录按创建顺序记录，逆序删除（先深后浅）。
        for dir in self.dirs.iter().rev() {
            let _ = fs::remove_dir(dir);
        }
    }
}

fn copy_file_tracked(src: &Path, dest: &Path, guard: &mut CleanupGuard) -> StoreResult<()> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        ensure_dir_tracked(parent, guard)?;
    }
    // 先登记再复制：即使复制中途失败，也会在恢复失败时清掉半截文件。
    guard.files.push(dest.to_path_buf());
    fs::copy(src, dest)?;
    Ok(())
}

fn ensure_dir_tracked(dir: &Path, guard: &mut CleanupGuard) -> StoreResult<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() {
            ensure_dir_tracked(parent, guard)?;
        }
    }
    fs::create_dir(dir)?;
    guard.dirs.push(dir.to_path_buf());
    Ok(())
}

/// 递归复制目录；只复制目标缺失的文件，返回文件数。
fn copy_dir_tracked(src: &Path, dest: &Path, guard: &mut CleanupGuard) -> StoreResult<usize> {
    let mut count = 0usize;
    ensure_dir_tracked(dest, guard)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            count += copy_dir_tracked(&entry.path(), &target, guard)?;
        } else if file_type.is_file() {
            copy_file_tracked(&entry.path(), &target, guard)?;
            count += 1;
        }
    }
    Ok(count)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_files(&path, out),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}

/// 校验 zip 内的相对路径（禁止绝对路径、`..`、盘符等越界路径）。
fn safe_rel(name: &str) -> StoreResult<PathBuf> {
    if name.is_empty() || name.contains('\0') {
        return Err(StoreError::Msg("备份中存在非法文件路径".to_string()));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(StoreError::Msg(format!("备份中存在绝对路径：{name}")));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err(StoreError::Msg(format!("备份中存在越界路径：{name}"))),
        }
    }
    Ok(path.to_path_buf())
}

fn staging_dir(tag: &str) -> StoreResult<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "{STAGING_PREFIX}-{}-{tag}-{}-{}",
        std::process::id(),
        now_ms(),
        next_seq()
    ));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn next_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::SeqCst)
}

/// 备份输出路径的临时兄弟文件（同目录保证 rename 原子性）。
fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "clipnest-backup.cnbak".to_string());
    name.push_str(&format!(".tmp-{}", std::process::id()));
    path.with_file_name(name)
}

/// Windows 上原子替换目标文件（`MoveFileExW` + `MOVEFILE_REPLACE_EXISTING`）。
fn replace_file(src: &Path, dest: &Path) -> StoreResult<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let to_wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let src_w = to_wide(src);
    let dest_w = to_wide(dest);
    let ok = unsafe {
        MoveFileExW(
            src_w.as_ptr(),
            dest_w.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(StoreError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn short(value: &str) -> String {
    value.chars().take(10).collect()
}

fn zip_error(context: &str, err: ZipError) -> StoreError {
    StoreError::Msg(format!("{context}：{err}"))
}

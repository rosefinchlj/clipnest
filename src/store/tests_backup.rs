//! 完整备份/恢复测试：往返一致性、损坏拒绝、校验和篡改检测。
//!
//! 与其它存储测试一样，只使用 `std::env::temp_dir()` 下的隔离目录。

use super::tests::*;
use super::{backup, blob};
use crate::model::*;
use rusqlite::params;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// 统计未删除条目数。
fn clip_count(db: &super::db::Db) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM clips WHERE deleted_at IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("统计条目失败")
}

/// 构造图片 ClipInput。
fn image_input(png: &[u8], seed: u8) -> ClipInput {
    ClipInput {
        kind: ClipKind::Image,
        hash: sha(&format!("image-key-{seed}")),
        preview: format!("图片 {seed}"),
        text: None,
        html: None,
        rtf: None,
        image_png: Some(png.to_vec()),
        files: Vec::new(),
        byte_len: png.len() as u64,
        width: Some(4),
        height: Some(3),
        source_app: Some("test.exe".to_string()),
        source_title: Some("图片测试".to_string()),
        captured_at: now_ms(),
    }
}

/// 用同样的条目名重写 zip，并把第一张图片的内容替换成垃圾数据（manifest 保持原样）。
fn tamper_first_image(original: &Path, tampered: &Path) {
    let file = File::open(original).expect("打开原备份失败");
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).expect("读取原备份 zip 失败");
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for index in 0..archive.len() {
        let mut member = archive.by_index(index).expect("读取 zip 条目失败");
        let name = member.name().to_string();
        let mut buf = Vec::new();
        member.read_to_end(&mut buf).expect("读取 zip 内容失败");
        entries.push((name, buf));
    }

    let out = File::create(tampered).expect("创建篡改备份失败");
    let mut writer = zip::ZipWriter::new(BufWriter::new(out));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut tampered_one = false;
    for (name, buf) in entries {
        writer
            .start_file(&name, options)
            .expect("写入 zip 条目失败");
        if !tampered_one && name.starts_with("blobs/images/") {
            writer
                .write_all(b"tampered-not-a-png")
                .expect("写入篡改内容失败");
            tampered_one = true;
            continue;
        }
        writer.write_all(&buf).expect("复制 zip 内容失败");
    }
    assert!(tampered_one, "备份中应包含至少一张图片");
    writer.finish().expect("完成篡改备份失败");
}

// ---------------------------------------------------------------------------
// 往返一致性
// ---------------------------------------------------------------------------

#[test]
fn backup_roundtrip_restores_text_image_and_favorite_files() {
    let (dir_a, db_a) = open_db("backup-roundtrip-a");

    // 1) 文本条目（收藏）。
    let text = "备份往返文本 Alpha";
    let text_hash = sha(text);
    let text_id = db_a.upsert(&text_input(text)).expect("写入文本失败").id();
    db_a.set_favorited(text_id, true).expect("收藏文本失败");

    // 2) 图片条目（含收藏/置顶）。
    let png = tiny_png(7);
    let image_hash = sha("image-key-7");
    let image_outcome = db_a.upsert(&image_input(&png, 7)).expect("写入图片失败");
    let image_id = image_outcome.id();
    db_a.set_favorited(image_id, true).expect("收藏图片失败");
    db_a.set_pinned(image_id, true).expect("置顶图片失败");
    let image_blob_id: String = db_a
        .conn
        .query_row(
            "SELECT blob_id FROM clips WHERE id = ?1",
            params![image_id],
            |row| row.get(0),
        )
        .expect("读取图片 blob_id 失败");

    // 3) 文件条目 + 收藏文件副本（stored=true）。
    let source_file = dir_a.join("收藏笔记.txt");
    std::fs::write(&source_file, b"favorite file payload").expect("写源文件失败");
    let files_key = source_file.to_string_lossy().to_string();
    let files_hash = sha(&files_key);
    let files_id = db_a
        .upsert(&files_input(&[&source_file], now_ms()))
        .expect("写入文件条目失败")
        .id();
    db_a.set_favorited(files_id, true)
        .expect("收藏文件条目失败");
    let copy = db_a
        .save_favorite_files(files_id, &Settings::default())
        .expect("保存收藏文件失败")
        .expect("应返回收藏文件副本信息");
    assert_eq!(copy.copied, 1, "收藏文件应被复制");
    let files_blob_id = copy.blob_id.clone().expect("收藏文件 blob_id 缺失");
    db_a.set_pinned(files_id, true).expect("置顶文件条目失败");

    let expected = clip_count(&db_a);
    assert_eq!(expected, 3);

    // 4) 创建备份并检查摘要。
    let backup_path = dir_a.join("roundtrip.cnbak");
    let report = backup::create_backup(&db_a, &backup_path).expect("创建备份失败");
    assert_eq!(report.clips, 3);
    assert_eq!(report.images, 1);
    assert_eq!(report.favorite_files, 1);
    assert!(backup_path.is_file(), "备份文件应已落盘");
    assert!(report.bytes > 0);

    let preview = backup::inspect_backup(&backup_path, None).expect("检查备份失败");
    assert!(preview.checksum_ok, "校验应通过：{}", preview.checksum_note);
    assert_eq!(preview.clips, 3);
    assert_eq!(preview.images, 1);
    assert_eq!(preview.favorite_files, 1);
    assert_eq!(preview.format_version, BACKUP_FORMAT_VERSION);
    assert!(!preview.app_version.is_empty());

    // 5) 恢复到另一个隔离库。
    let (dir_b, db_b) = open_db("backup-roundtrip-b");
    let restored = backup::restore_backup(&db_b, &backup_path, None).expect("恢复失败");
    assert_eq!(restored.inserted, 3);
    assert_eq!(restored.skipped, 0);
    assert_eq!(restored.images, 1);
    assert_eq!(restored.favorite_files, 1);
    assert_eq!(clip_count(&db_b), 3);

    // 文本内容与收藏状态。
    let (restored_text, favorited): (Option<String>, i64) = db_b
        .conn
        .query_row(
            "SELECT text, favorited FROM clips WHERE hash = ?1",
            params![text_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("恢复后的文本条目缺失");
    assert_eq!(restored_text.as_deref(), Some(text));
    assert_eq!(favorited, 1, "收藏状态应保留");

    // 图片文件与缩略图、收藏/置顶状态。
    let (restored_blob_id, image_favorited, image_pinned): (String, i64, i64) = db_b
        .conn
        .query_row(
            "SELECT blob_id, favorited, pinned FROM clips WHERE hash = ?1",
            params![image_hash],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("恢复后的图片条目缺失");
    assert_eq!(
        restored_blob_id, image_blob_id,
        "图片 blob id 应保持内容寻址一致"
    );
    assert_eq!(image_favorited, 1);
    assert_eq!(image_pinned, 1);
    let image_path = blob::image_path(&dir_b, &restored_blob_id);
    assert!(image_path.is_file(), "恢复后的图片文件缺失");
    assert_eq!(
        std::fs::read(&image_path).expect("读取恢复图片失败"),
        png,
        "恢复后的图片内容应一致"
    );
    let thumb_path = blob::thumb_path(&dir_b, &restored_blob_id);
    assert!(thumb_path.is_file(), "恢复后的缩略图缺失");

    // 收藏文件副本实体与清单。
    let (restored_files_json, restored_files_blob): (String, String) = db_b
        .conn
        .query_row(
            "SELECT files_json, blob_id FROM clips WHERE hash = ?1",
            params![files_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("恢复后的文件条目缺失");
    assert_eq!(restored_files_blob, files_blob_id);
    let entries: Vec<FileEntry> =
        serde_json::from_str(&restored_files_json).expect("解析 files_json 失败");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].stored, "恢复后 stored 标记应为 true");
    let stored_dir = blob::favorite_files_dir(&dir_b, &restored_files_blob);
    let stored_copy =
        blob::find_stored_copy(&stored_dir, &entries[0].name).expect("恢复后的收藏文件副本缺失");
    assert_eq!(
        std::fs::read(&stored_copy).expect("读取收藏文件副本失败"),
        b"favorite file payload"
    );

    // 再次恢复：全部按 hash 去重跳过，不重复插入。
    let again = backup::restore_backup(&db_b, &backup_path, None).expect("二次恢复失败");
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, 3);
    assert_eq!(clip_count(&db_b), 3);
}

// ---------------------------------------------------------------------------
// 损坏与篡改
// ---------------------------------------------------------------------------

#[test]
fn corrupted_zip_is_rejected_and_existing_db_untouched() {
    let (dir, db) = open_db("backup-corrupt");
    db.upsert(&text_input("现有条目应保持"))
        .expect("写入现有条目失败");
    let before = clip_count(&db);

    let bad_path = dir.join("broken.cnbak");
    std::fs::write(&bad_path, b"this is definitely not a zip archive").expect("写损坏文件失败");

    let inspect_err = backup::inspect_backup(&bad_path, None).expect_err("损坏 zip 应检查失败");
    assert!(
        inspect_err.to_string().contains("zip") || inspect_err.to_string().contains("备份"),
        "错误信息应可读：{inspect_err}"
    );
    let restore_err =
        backup::restore_backup(&db, &bad_path, None).expect_err("损坏 zip 应恢复失败");
    let _ = restore_err;
    assert_eq!(clip_count(&db), before, "恢复失败不得改动现有数据库");
    let text: String = db
        .conn
        .query_row("SELECT text FROM clips LIMIT 1", [], |row| row.get(0))
        .expect("读取现有条目失败");
    assert_eq!(text, "现有条目应保持");
}

#[test]
fn tampered_backup_fails_checksum_without_touching_db() {
    let (dir_a, db_a) = open_db("backup-tamper-a");
    db_a.upsert(&text_input("篡改测试条目"))
        .expect("写入条目失败");
    db_a.upsert(&image_input(&tiny_png(9), 9))
        .expect("写入图片失败");
    let backup_path = dir_a.join("original.cnbak");
    backup::create_backup(&db_a, &backup_path).expect("创建备份失败");

    let tampered_path = dir_a.join("tampered.cnbak");
    tamper_first_image(&backup_path, &tampered_path);

    // 检查阶段不报错，但会把校验失败标记返回给预览 UI。
    let preview = backup::inspect_backup(&tampered_path, None).expect("检查篡改备份失败");
    assert!(!preview.checksum_ok, "篡改后的备份不应通过校验");
    assert!(preview.checksum_errors >= 1);
    assert!(!preview.checksum_note.is_empty());

    // 恢复阶段必须整体拒绝，且目标库保持不变。
    let (_dir_b, db_b) = open_db("backup-tamper-b");
    db_b.upsert(&text_input("目标库原有条目"))
        .expect("写入目标条目失败");
    let before = clip_count(&db_b);
    let err = backup::restore_backup(&db_b, &tampered_path, None).expect_err("篡改备份应恢复失败");
    let message = err.to_string();
    assert!(
        message.contains("校验") || message.contains("大小"),
        "错误信息应说明校验失败：{message}"
    );
    assert_eq!(clip_count(&db_b), before, "恢复失败不得改动现有数据库");
}

#[test]
fn encrypted_database_backup_can_be_inspected_and_restored() {
    let (src_dir, mut src) = open_db("enc-backup-src");
    src.upsert(&text_input("加密备份内容")).unwrap();
    src.set_encryption(true).unwrap();
    let key = crate::db_crypto::load_key_hex(&src_dir).unwrap();
    assert!(crate::db_crypto::is_encrypted_file(
        &src_dir.join("data.db")
    ));
    let backup_path = src_dir.join("enc.cnbak");
    backup::create_backup(&src, &backup_path).expect("加密库备份失败");
    drop(src);

    let preview = backup::inspect_backup(&backup_path, key.as_deref()).expect("检查加密备份失败");
    assert_eq!(preview.clips, 1);
    let (_dst_dir, dst) = open_db("enc-backup-dst");
    let report =
        backup::restore_backup(&dst, &backup_path, key.as_deref()).expect("恢复加密备份失败");
    assert_eq!(report.inserted, 1);
    assert_eq!(clip_count(&dst), 1);
}

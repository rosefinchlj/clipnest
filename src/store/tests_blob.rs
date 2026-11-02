//! 存储域单元测试（第二部分）：blob 内容寻址 / 收藏文件副本与恢复 / 导出导入 / 损坏库恢复。

use super::tests::*;
use super::{blob, export, retention};
use crate::model::*;
use rusqlite::params;
use std::path::Path;

/// 递归统计目录下文件数。
fn count_files(dir: &Path) -> usize {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                total += 1;
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// 图片 blob
// ---------------------------------------------------------------------------

#[test]
fn image_blob_content_addressed_with_refcount() {
    let (dir, db) = open_db("blob-image");
    let png = tiny_png(5);
    // 两个不同 hash 的条目引用同一张图片 → 共享同一 blob
    let first = db.upsert(&image_input(png.clone(), 1_000)).unwrap().id();
    let second = db
        .upsert(&image_input_variant(png.clone(), 2_000, "变体"))
        .unwrap()
        .id();

    let blob_id = db.item(first).unwrap().unwrap().blob_id.unwrap();
    let blob_id_2 = db.item(second).unwrap().unwrap().blob_id.unwrap();
    assert_eq!(blob_id, blob_id_2, "相同图片应共享内容寻址 blob");
    assert_eq!(blob_id.len(), 64);

    // 同一 sha 的 PNG 目录下只有一份文件，内容字节一致
    let png_path = blob::image_path(&dir, &blob_id);
    assert!(png_path.exists());
    assert_eq!(std::fs::read(&png_path).unwrap(), png);
    assert_eq!(
        count_files(&blob::images_dir(&dir)),
        1,
        "相同图片只落盘一次"
    );

    // refs = 2
    let refs: i64 = db
        .conn
        .query_row(
            "SELECT refs FROM blobs WHERE id = ?1",
            params![blob_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(refs, 2);

    // 缩略图生成且可解码，最长边 ≤96
    let thumb_path = blob::thumb_path(&dir, &blob_id);
    assert!(thumb_path.exists(), "应生成缩略图");
    let thumb_bytes = std::fs::read(&thumb_path).unwrap();
    let thumb = image::load_from_memory_with_format(&thumb_bytes, image::ImageFormat::Jpeg)
        .expect("缩略图应为合法 JPEG");
    use image::GenericImageView;
    let (width, height) = thumb.dimensions();
    assert!(width <= blob::THUMB_MAX_EDGE && height <= blob::THUMB_MAX_EDGE);

    // 删除一个引用：refs=1，文件保留
    db.delete(&[first]).unwrap();
    assert!(png_path.exists());
    let refs: i64 = db
        .conn
        .query_row(
            "SELECT refs FROM blobs WHERE id = ?1",
            params![blob_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(refs, 1);
    // 删除最后一个引用：blob 行与文件都删除
    db.delete(&[second]).unwrap();
    assert!(!png_path.exists());
    assert!(!thumb_path.exists());
    let rows: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM blobs WHERE id = ?1",
            params![blob_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn image_paste_payload_has_png_path() {
    let (dir, db) = open_db("paste-image");
    let png = tiny_png(8);
    let id = db.upsert(&image_input(png.clone(), 1_000)).unwrap().id();
    let payload = db.paste_payload(id).unwrap().expect("应返回粘贴载荷");
    assert_eq!(payload.clip_id, id);
    assert_eq!(payload.kind, ClipKind::Image);
    let path = payload.png_path.expect("图片载荷应有 png_path");
    assert!(path.starts_with(&dir));
    assert_eq!(std::fs::read(&path).unwrap(), png);
    // 不存在的 id → None
    assert!(db.paste_payload(999_999).unwrap().is_none());
}

#[test]
fn file_entries_json_roundtrip() {
    let (_dir, db) = open_db("files-json");
    let source_dir = temp_dir("files-json-src");
    let file = write_temp_file(&source_dir, "清单.txt", b"file body");
    let id = db
        .upsert(&files_input(&[file.as_path()], 1_000))
        .unwrap()
        .id();
    let item = db.item(id).unwrap().unwrap();
    assert_eq!(item.kind, ClipKind::Files);
    assert_eq!(item.files.len(), 1);
    assert_eq!(item.files[0].name, "清单.txt");
    assert_eq!(item.files[0].path, file.to_string_lossy().to_string());
    assert!(!item.files[0].stored);
    assert!(item.files[0].exists);
    assert!(item.preview.contains("清单.txt"));
    // 非文件类型不需要保存副本
    let text_id = db.upsert(&text_input("文本条目")).unwrap().id();
    assert!(db
        .save_favorite_files(text_id, &Settings::default())
        .unwrap()
        .is_none());
}

// ---------------------------------------------------------------------------
// 收藏文件复制 / 恢复
// ---------------------------------------------------------------------------

#[test]
fn favorite_files_copy_and_restore() {
    let (dir, db) = open_db("favorite");
    let source_dir = temp_dir("favorite-src");
    let first = write_temp_file(&source_dir, "报告 一.txt", b"alpha content");
    let second = write_temp_file(&source_dir, "b.txt", b"beta content");
    let id = db
        .upsert(&files_input(&[first.as_path(), second.as_path()], 1_000))
        .unwrap()
        .id();

    // 收藏 → 复制实体
    let copy = db
        .save_favorite_files(id, &Settings::default())
        .unwrap()
        .expect("文件条目应返回复制结果");
    assert_eq!(copy.copied, 2);
    assert!(!copy.limit_hit);
    assert_eq!(copy.skipped_missing, 0);
    let item = db.item(id).unwrap().unwrap();
    assert!(item.files.iter().all(|file| file.stored));
    let blob_id = item.blob_id.clone().expect("收藏后应有 blob_id");
    let stored_dir = blob::favorite_files_dir(&dir, &blob_id);
    assert!(stored_dir.is_dir());
    let stored = blob::find_stored_copy(&stored_dir, "b.txt").expect("应找到实体副本");
    assert_eq!(std::fs::read(&stored).unwrap(), b"beta content");

    // 幂等：再次保存不重复复制
    let again = db
        .save_favorite_files(id, &Settings::default())
        .unwrap()
        .unwrap();
    assert_eq!(again.copied, 0);
    assert_eq!(count_files(&stored_dir), 2);

    // 删除源文件 → GetPastePayload 自动恢复到 restored/<时间戳>/
    std::fs::remove_file(&first).unwrap();
    std::fs::remove_file(&second).unwrap();
    let payload = db.paste_payload(id).unwrap().expect("应返回文件载荷");
    assert_eq!(payload.kind, ClipKind::Files);
    assert_eq!(payload.files.len(), 2);
    for file in &payload.files {
        assert!(file.exists, "恢复后 exists=true");
        assert!(
            Path::new(&file.path).exists(),
            "恢复文件应存在：{}",
            file.path
        );
        assert!(
            file.path.contains("restored"),
            "应位于恢复目录：{}",
            file.path
        );
    }
    let mut contents: Vec<Vec<u8>> = payload
        .files
        .iter()
        .map(|file| std::fs::read(&file.path).unwrap())
        .collect();
    contents.sort();
    assert_eq!(
        contents,
        vec![b"alpha content".to_vec(), b"beta content".to_vec()]
    );

    // 恢复路径已持久化：再次取载荷不会重复复制（路径已存在）
    let payload_again = db.paste_payload(id).unwrap().unwrap();
    assert_eq!(payload_again.files.len(), 2);
    assert!(payload_again
        .files
        .iter()
        .all(|file| Path::new(&file.path).exists()));
}

#[test]
fn favorite_copy_respects_limits() {
    let (dir, db) = open_db("favorite-limit");
    let source_dir = temp_dir("favorite-limit-src");
    let big = write_temp_file(&source_dir, "big.bin", &vec![7u8; 2048]);
    let id = db
        .upsert(&files_input(&[big.as_path()], 1_000))
        .unwrap()
        .id();

    // 单文件上限：跳过复制
    let mut settings = Settings::default();
    settings.max_file_bytes = 1024;
    let copy = db.save_favorite_files(id, &settings).unwrap().unwrap();
    assert_eq!(copy.copied, 0);
    assert_eq!(copy.skipped_too_large, 1);

    // 总量上限 = 0：只保留路径
    settings.max_file_bytes = 0;
    settings.max_favorite_file_bytes = 0;
    let copy = db.save_favorite_files(id, &settings).unwrap().unwrap();
    assert!(copy.limit_hit);
    assert_eq!(copy.copied, 0);

    // 正常复制后把总量调小：retention 只标记超限，绝不删除已有实体
    settings.max_favorite_file_bytes = 10 * 1024 * 1024;
    let copy = db.save_favorite_files(id, &settings).unwrap().unwrap();
    assert_eq!(copy.copied, 1);
    let blob_id = db.item(id).unwrap().unwrap().blob_id.unwrap();
    let stored_dir = blob::favorite_files_dir(&dir, &blob_id);
    assert!(stored_dir.is_dir());
    settings.max_favorite_file_bytes = 1;
    let report = retention::prune(&db, &settings).unwrap();
    assert!(report.favorites_limit_hit);
    assert!(stored_dir.is_dir(), "超限不得删除已有收藏实体");
    assert!(blob::find_stored_copy(&stored_dir, "big.bin").is_some());
}

// ---------------------------------------------------------------------------
// 导出 / 导入
// ---------------------------------------------------------------------------

#[test]
fn export_import_roundtrip() {
    let (dir, db) = open_db("export-src");
    let source_dir = temp_dir("export-files");
    let file = write_temp_file(&source_dir, "附件.txt", b"file body");
    db.upsert(&text_input("导出文本一")).unwrap();
    db.upsert(&text_input_at("导出文本二", 2_000)).unwrap();
    let pinned_id = db
        .upsert(&text_input_at("导出收藏文本", 3_000))
        .unwrap()
        .id();
    db.set_pinned(pinned_id, true).unwrap();
    let file_id = db
        .upsert(&files_input(&[file.as_path()], 4_000))
        .unwrap()
        .id();
    db.save_favorite_files(file_id, &Settings::default())
        .unwrap()
        .unwrap();

    let export_path = dir.join("export.json");
    let report = export::export_json(&db, &export_path).unwrap();
    assert_eq!(report.clips, 4);
    assert!(export_path.exists());
    assert!(report.bytes > 0);
    let doc = export::read_export(&export_path).unwrap();
    assert_eq!(doc.version, export::EXPORT_VERSION);
    assert!(doc.clips.iter().any(|clip| clip.pinned));
    assert!(export::contains_hash(&doc, &sha("导出文本一")));

    // 导入到全新数据库
    let (dir2, db2) = open_db("export-dst");
    let imported = export::import_json(&db2, &export_path).unwrap();
    assert_eq!(imported.inserted, 4);
    assert_eq!(imported.skipped, 0);
    assert_eq!(total_rows(&db2), 4);
    assert_eq!(db2.counts().unwrap().pinned, 1);
    // 文本内容与收藏状态一致
    let page = db2
        .search(&SearchQuery {
            text: "导出".to_string(),
            filter: ClipFilter::All,
            offset: 0,
            limit: 10,
            trash: false,
            gen: 1,
        })
        .unwrap();
    assert_eq!(page.total, 3);
    assert!(page
        .items
        .iter()
        .any(|item| item.pinned && item.text.as_deref() == Some("导出收藏文本")));
    // 文件条目路径清单保留
    let files_page = db2
        .search(&SearchQuery {
            text: "附件".to_string(),
            filter: ClipFilter::All,
            offset: 0,
            limit: 10,
            trash: false,
            gen: 2,
        })
        .unwrap();
    assert_eq!(files_page.total, 1);
    assert_eq!(files_page.items[0].files[0].name, "附件.txt");

    // 二次导入幂等（全部命中 hash）
    let imported_again = export::import_json(&db2, &export_path).unwrap();
    assert_eq!(imported_again.inserted, 0);
    assert_eq!(imported_again.touched, 4);
    assert_eq!(total_rows(&db2), 4);

    // 导入文件格式错误 → 明确报错不 panic
    let bad = dir2.join("bad.json");
    std::fs::write(&bad, b"{not json").unwrap();
    assert!(export::import_json(&db2, &bad).is_err());
}

// ---------------------------------------------------------------------------
// 损坏库恢复
// ---------------------------------------------------------------------------

#[test]
fn corrupt_database_is_backed_up_and_rebuilt() {
    let dir = temp_dir("corrupt");
    std::fs::write(
        dir.join("data.db"),
        b"this is definitely not a sqlite database",
    )
    .unwrap();
    let db = db_open_recovered(&dir);
    assert_eq!(total_rows(&db), 0, "重建后应为空库");
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    // 原损坏文件被备份
    let backups: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".bad-"))
        .collect();
    assert!(!backups.is_empty(), "应保留损坏库备份");
    // 重建后的库可正常读写
    db.upsert(&text_input("重建后写入")).unwrap();
    assert_eq!(total_rows(&db), 1);
}

/// 打开（允许自动修复）指定数据目录的库。
fn db_open_recovered(dir: &Path) -> super::db::Db {
    super::db::Db::open_in_dir(dir, super::db::DbOptions::default()).expect("损坏库应自动备份重建")
}

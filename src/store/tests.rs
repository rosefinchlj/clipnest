//! 存储域单元测试：schema / 写入去重 / 查询分页过滤 / retention。
//!
//! 所有测试使用 `std::env::temp_dir()` 下的唯一目录，绝不读写 `%APPDATA%\ClipNest`。
//! 公共夹具（临时目录、`ClipInput` 构造、示例数据）由本模块提供，
//! `tests_blob`（blob/导出）与 `tests_thread`（线程/性能）复用。

use super::db::{Db, DbOptions, UpsertOutcome};
use super::schema;
use super::{blob, retention};
use crate::model::*;
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 唯一临时目录（不自动清理：避免 Windows 上数据库仍被占用时删除失败）。
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "clipnest-store-test-{}-{}-{seq}-{tag}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    dir
}

/// 打开临时新库（默认 FTS 探测）。
pub(crate) fn open_db(tag: &str) -> (PathBuf, Db) {
    let dir = temp_dir(tag);
    let db = Db::open_in_dir(&dir, DbOptions::default()).expect("打开测试库失败");
    (dir, db)
}

/// 打开强制 LIKE 降级的临时新库。
pub(crate) fn open_db_like(tag: &str) -> (PathBuf, Db) {
    let dir = temp_dir(tag);
    let db = Db::open_in_dir(
        &dir,
        DbOptions {
            disable_fts: true,
            ..Default::default()
        },
    )
    .expect("打开测试库失败");
    (dir, db)
}

pub(crate) fn sha(text: &str) -> String {
    blob::sha256_hex(text.as_bytes())
}

/// 构造文本 ClipInput（hash = sha(文本)，captured_at 可控）。
pub(crate) fn text_input_at(text: &str, captured_at: i64) -> ClipInput {
    ClipInput {
        kind: ClipKind::Text,
        hash: sha(text),
        preview: text.chars().take(200).collect(),
        text: Some(text.to_string()),
        html: None,
        rtf: None,
        image_png: None,
        files: Vec::new(),
        byte_len: text.len() as u64,
        width: None,
        height: None,
        source_app: Some("test.exe".to_string()),
        source_title: None,
        captured_at,
    }
}

pub(crate) fn text_input(text: &str) -> ClipInput {
    text_input_at(text, now_ms())
}

/// 构造文件条目 ClipInput。
pub(crate) fn files_input(paths: &[&Path], captured_at: i64) -> ClipInput {
    let files: Vec<FileEntry> = paths
        .iter()
        .map(|path| FileEntry::from_path(path))
        .collect();
    let key = paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("|");
    ClipInput {
        kind: ClipKind::Files,
        hash: sha(&key),
        preview: files
            .iter()
            .map(FileEntry::display_name)
            .collect::<Vec<_>>()
            .join("、"),
        text: None,
        html: None,
        rtf: None,
        image_png: None,
        files,
        byte_len: 0,
        width: None,
        height: None,
        source_app: Some("explorer.exe".to_string()),
        source_title: None,
        captured_at,
    }
}

/// 生成 4×3 的测试 PNG（不同 seed → 不同内容）。
pub(crate) fn tiny_png(seed: u8) -> Vec<u8> {
    const WIDTH: u32 = 4;
    const HEIGHT: u32 = 3;
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            pixels.extend_from_slice(&[
                seed.wrapping_add(x as u8),
                seed.wrapping_add(y as u8),
                seed.wrapping_mul(3),
                255,
            ]);
        }
    }
    let mut out = Vec::new();
    {
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&pixels, WIDTH, HEIGHT, image::ExtendedColorType::Rgba8)
            .expect("测试 PNG 编码失败");
    }
    out
}

/// 构造图片 ClipInput。
pub(crate) fn image_input(png: Vec<u8>, captured_at: i64) -> ClipInput {
    let (width, height) = png_size(&png);
    let byte_len = png.len() as u64;
    ClipInput {
        kind: ClipKind::Image,
        hash: sha(&format!("image:{}", blob::sha256_hex(&png))),
        preview: format!("图片 {width}×{height}"),
        text: None,
        html: None,
        rtf: None,
        image_png: Some(png),
        files: Vec::new(),
        byte_len,
        width: Some(width),
        height: Some(height),
        source_app: Some("snippingtool.exe".to_string()),
        source_title: None,
        captured_at,
    }
}

/// 同图片但不同 hash（验证 blob 内容寻址 + 引用计数）。
pub(crate) fn image_input_variant(png: Vec<u8>, captured_at: i64, variant: &str) -> ClipInput {
    let mut input = image_input(png, captured_at);
    input.hash = sha(&format!("{variant}:{}", input.hash));
    input
}

pub(crate) fn png_size(png: &[u8]) -> (u32, u32) {
    use image::GenericImageView;
    let image = image::load_from_memory(png).expect("测试 PNG 解码失败");
    image.dimensions()
}

pub(crate) fn write_temp_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("写临时文件失败");
    path
}

pub(crate) fn table_exists(db: &Db, name: &str) -> bool {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0
}

pub(crate) fn total_rows(db: &Db) -> usize {
    db.counts().expect("统计失败").total
}

pub(crate) fn search_query(
    text: &str,
    filter: ClipFilter,
    offset: usize,
    limit: usize,
    gen: u64,
) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        filter,
        time_range: crate::model::TimeRange::All,
        time_basis: crate::model::TimeBasis::LastUsed,
        custom_day: None,
        custom_end_day: None,
        offset,
        limit,
        gen,
        ..SearchQuery::default()
    }
}

// ---------------------------------------------------------------------------
// schema / 迁移
// ---------------------------------------------------------------------------

#[test]
fn schema_v1_wal_and_idempotent_migrate() {
    let (dir, mut db) = open_db("schema");
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    for name in ["clips", "blobs", "meta"] {
        assert!(table_exists(&db, name), "缺少表 {name}");
    }
    // WAL 生效
    let mode: String = db.conn.pragma_query_one(None, "journal_mode").unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
    // FTS 状态与运行时探测一致（不假设 FTS5 一定可用）
    if schema::trigram_available() {
        assert!(db.fts_enabled, "FTS5 trigram 可用时迁移应启用 FTS");
        assert!(table_exists(&db, "clips_fts"));
    } else {
        assert!(!db.fts_enabled);
        assert!(!table_exists(&db, "clips_fts"));
    }
    // 迁移幂等：重复调用不报错、版本不变
    db.migrate(DbOptions::default()).unwrap();
    db.migrate(DbOptions::default()).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    drop(db);
    // 重新打开同一目录：仍能打开且数据不丢
    let db = Db::open_in_dir(&dir, DbOptions::default()).unwrap();
    db.upsert(&text_input("重新打开后仍存在")).unwrap();
    drop(db);
    let db = Db::open_in_dir(&dir, DbOptions::default()).unwrap();
    assert_eq!(total_rows(&db), 1);
}

#[test]
fn meta_records_fts_flag_and_created_at() {
    let (_dir, db) = open_db("meta");
    assert!(db.meta_get("fts_enabled").unwrap().is_some());
    assert!(db.meta_get("created_at").unwrap().is_some());
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn enabling_fts_later_rebuilds_existing_rows() {
    // 先在禁用 FTS 的模式下建库写入历史数据，再以 FTS 模式打开：
    // 应创建 clips_fts 并 rebuild 回填已有数据（spec §10.2 创建顺序要求）。
    let dir = temp_dir("fts-rebuild");
    let db = Db::open_in_dir(
        &dir,
        DbOptions {
            disable_fts: true,
            ..Default::default()
        },
    )
    .unwrap();
    db.upsert(&text_input("稍后启用全文索引的内容")).unwrap();
    drop(db);

    let db = Db::open_in_dir(&dir, DbOptions::default()).unwrap();
    if db.fts_enabled {
        assert!(table_exists(&db, "clips_fts"));
        let page = db
            .search(&search_query("稍后启用", ClipFilter::All, 0, 10, 1))
            .unwrap();
        assert_eq!(page.total, 1, "重建 FTS 应能检索到历史数据");
    } else {
        // 构建不支持 trigram：LIKE 降级依然要能检索到
        let page = db
            .search(&search_query("稍后启用", ClipFilter::All, 0, 10, 1))
            .unwrap();
        assert_eq!(page.total, 1);
    }
}

// ---------------------------------------------------------------------------
// 写入 / 去重
// ---------------------------------------------------------------------------

#[test]
fn upsert_dedupes_and_bumps_usage() {
    let (_dir, db) = open_db("dedupe");
    let input = text_input("去重测试内容");
    let UpsertOutcome::Inserted(id) = db.upsert(&input).unwrap() else {
        panic!("首次写入应为 Inserted");
    };
    let item = db.item(id).unwrap().unwrap();
    assert_eq!(item.use_count, 1);
    assert!(!item.pinned);
    assert_eq!(total_rows(&db), 1);

    // 重复复制 → Touched：不新增行、use_count+1、last_used_at 更新
    let before = item.last_used_at;
    assert_eq!(db.upsert(&input).unwrap(), UpsertOutcome::Touched(id));
    let item = db.item(id).unwrap().unwrap();
    assert_eq!(item.use_count, 2);
    assert!(item.last_used_at >= before);
    assert_eq!(total_rows(&db), 1);

    // 收藏后再复制：收藏标记不被重置
    assert!(db.set_favorited(id, true).unwrap());
    assert!(db.set_pinned(id, true).unwrap());
    db.upsert(&input).unwrap();
    let item = db.item(id).unwrap().unwrap();
    assert!(item.favorited, "去重更新不得清除收藏标记");
    assert!(item.pinned, "去重更新不得清除置顶标记");
    assert_eq!(item.use_count, 3);
    assert_eq!(total_rows(&db), 1);
}

#[test]
fn upsert_hash_unique_across_kinds() {
    let (_dir, db) = open_db("dedupe-kind");
    let text = text_input("跨类型同 hash");
    let UpsertOutcome::Inserted(id) = db.upsert(&text).unwrap() else {
        panic!()
    };
    let mut weird = files_input(&[], now_ms());
    weird.hash = text.hash.clone();
    assert_eq!(db.upsert(&weird).unwrap(), UpsertOutcome::Touched(id));
    assert_eq!(total_rows(&db), 1);
}

// ---------------------------------------------------------------------------
// 查询 / 分页 / 过滤 / 排序
// ---------------------------------------------------------------------------

struct Sample {
    image_ids: [i64; 2],
    file_ids: [i64; 2],
}

/// 7 条示例数据：3 文本 / 2 图片 / 2 文件，时间递增，收藏最旧文本。
fn seed_sample(db: &Db) -> Sample {
    let text_ids = [
        db.upsert(&text_input_at("第一行中文内容\n第二行内容", 1_000))
            .unwrap()
            .id(),
        db.upsert(&text_input_at("rust code fn main", 2_000))
            .unwrap()
            .id(),
        db.upsert(&text_input_at("搜索关键词 alpha", 3_000))
            .unwrap()
            .id(),
    ];
    let image_ids = [
        db.upsert(&image_input(tiny_png(11), 4_000)).unwrap().id(),
        db.upsert(&image_input(tiny_png(12), 5_000)).unwrap().id(),
    ];
    let png_a = write_temp_file(&temp_dir("sample-a"), "a.txt", b"a");
    let png_b = write_temp_file(&temp_dir("sample-b"), "b.txt", b"b");
    let file_ids = [
        db.upsert(&files_input(&[png_a.as_path()], 6_000))
            .unwrap()
            .id(),
        db.upsert(&files_input(&[png_b.as_path()], 7_000))
            .unwrap()
            .id(),
    ];
    assert!(db.set_favorited(text_ids[0], true).unwrap());
    assert!(db.set_pinned(text_ids[0], true).unwrap());
    Sample {
        image_ids,
        file_ids,
    }
}

#[test]
fn search_empty_query_sorted_and_paged() {
    let (_dir, db) = open_db("search-sort");
    let sample = seed_sample(&db);
    let page = db
        .search(&search_query("", ClipFilter::All, 0, 3, 7))
        .unwrap();
    assert_eq!(page.gen, 7);
    assert_eq!(page.total, 7);
    assert_eq!(page.items.len(), 3);
    // 全部列表保持纯时间序：收藏/置顶不应把旧内容顶到最前
    assert_eq!(page.items[0].id, sample.file_ids[1]);
    assert_eq!(page.items[1].id, sample.file_ids[0]);
    assert_eq!(page.items[2].id, sample.image_ids[1]);

    // 分页不重叠、可覆盖全部
    let page2 = db
        .search(&search_query("", ClipFilter::All, 3, 4, 8))
        .unwrap();
    assert_eq!(page2.items.len(), 4);
    let mut ids: Vec<i64> = page
        .items
        .iter()
        .chain(page2.items.iter())
        .map(|item| item.id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 7);

    // 越界分页：total 不变、items 为空
    let page3 = db
        .search(&search_query("", ClipFilter::All, 100, 10, 9))
        .unwrap();
    assert_eq!(page3.total, 7);
    assert!(page3.items.is_empty());
}

#[test]
fn search_filters() {
    let (_dir, db) = open_db("search-filter");
    let sample = seed_sample(&db);
    for (filter, expected) in [
        (ClipFilter::All, 7usize),
        (ClipFilter::Text, 3),
        (ClipFilter::Image, 2),
        (ClipFilter::Files, 2),
        (ClipFilter::Favorites, 1),
    ] {
        let page = db.search(&search_query("", filter, 0, 100, 1)).unwrap();
        assert_eq!(page.total, expected, "过滤条件 {filter:?}");
        assert_eq!(page.items.len(), expected, "过滤条件 {filter:?}");
        match filter {
            ClipFilter::All => {}
            ClipFilter::Text => assert!(page.items.iter().all(|i| i.kind == ClipKind::Text)),
            ClipFilter::Image => assert!(page.items.iter().all(|i| i.kind == ClipKind::Image)),
            ClipFilter::Files => assert!(page.items.iter().all(|i| i.kind == ClipKind::Files)),
            ClipFilter::Favorites => {
                assert!(page.items.iter().all(|i| i.favorited));
                assert!(page.items[0].pinned, "收藏列表内置顶项应排在最前");
            }
        }
    }
    // 图片过滤应包含示例图片条目
    let image_page = db
        .search(&search_query("", ClipFilter::Image, 0, 10, 3))
        .unwrap();
    let image_ids: Vec<i64> = image_page.items.iter().map(|item| item.id).collect();
    assert!(image_ids.contains(&sample.image_ids[0]));
    assert!(image_ids.contains(&sample.image_ids[1]));
    // 文本 + 图片组合过滤 → 0
    let page = db
        .search(&search_query("关键词", ClipFilter::Image, 0, 100, 2))
        .unwrap();
    assert_eq!(page.total, 0);
}

#[test]
fn time_basis_switches_between_used_and_captured() {
    let (_dir, db) = open_db("time-basis");
    let now = now_ms();
    let old = now - 40 * 86_400_000;
    let UpsertOutcome::Inserted(used_now_id) =
        db.upsert(&text_input_at("旧复制但最近用过", old)).unwrap()
    else {
        panic!("应插入旧记录");
    };
    let UpsertOutcome::Inserted(captured_now_id) =
        db.upsert(&text_input_at("刚复制但很久没用", now)).unwrap()
    else {
        panic!("应插入新记录");
    };
    db.conn
        .execute(
            "UPDATE clips SET last_used_at = ?1 WHERE id = ?2",
            params![now, used_now_id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE clips SET last_used_at = ?1 WHERE id = ?2",
            params![old, captured_now_id],
        )
        .unwrap();

    let mut query = search_query("", ClipFilter::All, 0, 10, 1);
    query.time_range = TimeRange::Last7Days;

    // 「最近使用」：命中 40 天前复制、刚刚用过的记录。
    query.time_basis = TimeBasis::LastUsed;
    let page = db.search(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, used_now_id);

    // 「复制时间」：命中刚复制、很久没用的记录。
    query.time_basis = TimeBasis::Captured;
    let page = db.search(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, captured_now_id);

    // 时间范围为 All 时两种依据都返回全部。
    query.time_range = TimeRange::All;
    assert_eq!(db.search(&query).unwrap().total, 2);
    query.time_basis = TimeBasis::LastUsed;
    assert_eq!(db.search(&query).unwrap().total, 2);
}

#[test]
fn save_text_as_writes_utf8_file() {
    let (dir, db) = open_db("save-text-as");
    let UpsertOutcome::Inserted(id) = db.upsert(&text_input("另存为文本内容")).unwrap()
    else {
        panic!("应插入文本记录");
    };
    let dest = dir.join("exported.txt");
    assert!(db.save_text_as(id, &dest).unwrap(), "保存应成功");
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "另存为文本内容",
        "文件内容应为原文本"
    );
    // 不存在的记录 → 返回 false（UI toast 提示已被清理），不 panic。
    assert!(!db.save_text_as(id + 999, &dir.join("missing.txt")).unwrap());
}

#[test]
fn time_range_filters_by_last_used() {
    let (_dir, db) = open_db("time-range");
    let now = now_ms();
    let UpsertOutcome::Inserted(old_id) = db
        .upsert(&text_input_at("旧记录", now - 40 * 86_400_000))
        .unwrap()
    else {
        panic!("应插入旧记录");
    };
    let UpsertOutcome::Inserted(recent_id) = db.upsert(&text_input_at("新记录", now)).unwrap()
    else {
        panic!("应插入新记录");
    };
    db.conn
        .execute(
            "UPDATE clips SET last_used_at = ?1 WHERE id = ?2",
            params![now - 40 * 86_400_000, old_id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE clips SET last_used_at = ?1 WHERE id = ?2",
            params![now, recent_id],
        )
        .unwrap();

    let mut query = search_query("", ClipFilter::All, 0, 10, 1);
    query.time_range = TimeRange::All;
    assert_eq!(db.search(&query).unwrap().total, 2);

    query.time_range = TimeRange::Last7Days;
    let page = db.search(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, recent_id);

    query.time_range = TimeRange::Today;
    assert_eq!(db.search(&query).unwrap().total, 1);

    query.time_range = TimeRange::Last90Days;
    assert_eq!(db.search(&query).unwrap().total, 2);

    // 任意一天：只命中该本地自然日内的记录。
    let today = TimeRange::Today.cutoff_ms(now).unwrap();
    query.time_range = TimeRange::Custom;
    query.custom_day = Some(today);
    let page = db.search(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, recent_id);
    query.custom_day = Some(today - 86_400_000);
    query.custom_end_day = None;
    assert_eq!(db.search(&query).unwrap().total, 0);

    // 日期范围：起止都包含，且起点晚于终点时自动交换。
    query.custom_day = Some(today - 50 * 86_400_000);
    query.custom_end_day = Some(today - 30 * 86_400_000);
    let page = db.search(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, old_id);
    query.custom_day = Some(today - 30 * 86_400_000);
    query.custom_end_day = Some(today - 50 * 86_400_000);
    let page = db.search(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, old_id);
}

#[test]
fn time_range_cutoffs_are_ordered() {
    let now = 1_700_000_000_000i64;
    assert_eq!(TimeRange::All.cutoff_ms(now), None);
    let today = TimeRange::Today.cutoff_ms(now).unwrap();
    assert!(today <= now && today > now - 2 * 86_400_000);
    assert_eq!(
        TimeRange::Last7Days.cutoff_ms(now),
        Some(now - 7 * 86_400_000)
    );
    assert_eq!(
        TimeRange::Last30Days.cutoff_ms(now),
        Some(now - 30 * 86_400_000)
    );
    assert_eq!(
        TimeRange::Last90Days.cutoff_ms(now),
        Some(now - 90 * 86_400_000)
    );
}

#[test]
fn pin_requires_favorite_and_unfavorite_clears_pin() {
    let (_dir, db) = open_db("favorite-pin");
    let id = db.upsert(&text_input("收藏与置顶")).unwrap().id();
    assert!(!db.set_pinned(id, true).unwrap(), "非收藏项不应允许置顶");
    assert!(db.set_favorited(id, true).unwrap());
    assert!(db.set_pinned(id, true).unwrap());
    let item = db.item(id).unwrap().unwrap();
    assert!(item.favorited && item.pinned);
    // 取消收藏必须同时清除置顶，避免之后重新收藏时意外置顶
    assert!(db.set_favorited(id, false).unwrap());
    let item = db.item(id).unwrap().unwrap();
    assert!(!item.favorited);
    assert!(!item.pinned);
}

#[test]
fn search_text_hit_and_elapsed() {
    let (_dir, db) = open_db("search-text");
    seed_sample(&db);
    let page = db
        .search(&search_query("关键词", ClipFilter::All, 0, 10, 3))
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].text.as_deref(), Some("搜索关键词 alpha"));
    assert!(page.elapsed_ms < 1_000);
    // 2 字符查询（FTS 不满足 trigram 长度 → LIKE）也能命中
    let page = db
        .search(&search_query("关键", ClipFilter::All, 0, 10, 4))
        .unwrap();
    assert_eq!(page.total, 1);
    // 不存在的词
    let page = db
        .search(&search_query(
            "绝不存在的关键词xyz",
            ClipFilter::All,
            0,
            10,
            5,
        ))
        .unwrap();
    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
}

#[test]
fn search_fts_and_like_paths_agree() {
    let (dir, db) = open_db("search-dual");
    seed_sample(&db);
    let fts_enabled = db.fts_enabled;
    let fts_page = db
        .search(&search_query("中文内容", ClipFilter::All, 0, 10, 1))
        .unwrap();
    assert_eq!(fts_page.total, 1);
    let fts_ids: Vec<i64> = fts_page.items.iter().map(|i| i.id).collect();

    // 关闭 FTS 后在同一数据库文件上走 LIKE 路径，结果一致
    drop(db);
    let like_db = Db::open_in_dir(
        &dir,
        DbOptions {
            disable_fts: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!like_db.fts_enabled);
    let like_page = like_db
        .search(&search_query("中文内容", ClipFilter::All, 0, 10, 2))
        .unwrap();
    assert_eq!(like_page.total, fts_page.total);
    let like_ids: Vec<i64> = like_page.items.iter().map(|i| i.id).collect();
    assert_eq!(fts_ids, like_ids);
    // FTS 可用时截断的词（<3 字符）在 LIKE 下同样工作
    let short = like_db
        .search(&search_query("中文", ClipFilter::All, 0, 10, 3))
        .unwrap();
    assert_eq!(short.total, 1);
    if fts_enabled {
        assert!(fts_page.items[0].id > 0);
    }
}

#[test]
fn search_special_characters_are_safe() {
    let (_dir, db) = open_db("search-special");
    let samples = [
        "100% 完成",
        "a_b 下划线",
        "他说\"你好\"",
        "'单引号'",
        "反斜杠 C:\\Users\\foo.txt",
    ];
    for sample in samples {
        db.upsert(&text_input(sample)).unwrap();
    }
    // 特殊字符查询不 panic、不报错
    for query in [
        "%",
        "_",
        "'",
        "\"",
        "%_",
        "100%",
        "\\",
        "/",
        "'单引号'",
        "abc%_''",
    ] {
        let page = db
            .search(&search_query(query, ClipFilter::All, 0, 50, 1))
            .expect("特殊字符查询不应报错");
        assert!(page.total <= samples.len());
    }
    // 字面量 % 与 _ 精确匹配（不会被当通配符）
    let page = db
        .search(&search_query("%", ClipFilter::All, 0, 50, 2))
        .unwrap();
    assert_eq!(page.total, 1);
    assert!(page
        .items
        .iter()
        .all(|i| i.text.as_deref().unwrap_or("").contains('%')));
    let page = db
        .search(&search_query("_", ClipFilter::All, 0, 50, 3))
        .unwrap();
    assert_eq!(page.total, 1);
    // 路径分隔符 / 与 \ 等价（spec §11）
    let page = db
        .search(&search_query("C:/Users", ClipFilter::All, 0, 50, 4))
        .unwrap();
    assert_eq!(page.total, 1, "正斜杠应匹配反斜杠路径");
    let page = db
        .search(&search_query("C:\\Users", ClipFilter::All, 0, 50, 5))
        .unwrap();
    assert_eq!(page.total, 1);
    let page = db
        .search(&search_query("/Users/foo", ClipFilter::All, 0, 50, 6))
        .unwrap();
    assert_eq!(page.total, 1);
}

#[test]
fn search_special_characters_on_like_path() {
    let (_dir, db) = open_db_like("search-special-like");
    assert!(!db.fts_enabled);
    db.upsert(&text_input("100% 完成")).unwrap();
    db.upsert(&text_input("a_b 下划线")).unwrap();
    db.upsert(&text_input("他说\"你好\"")).unwrap();
    for query in ["%", "_", "'", "\"", "%_", "100%", "'单引号'"] {
        db.search(&search_query(query, ClipFilter::All, 0, 50, 1))
            .expect("LIKE 路径特殊字符查询不应报错");
    }
    let page = db
        .search(&search_query("%", ClipFilter::All, 0, 50, 2))
        .unwrap();
    assert_eq!(page.total, 1);
    let page = db
        .search(&search_query("_", ClipFilter::All, 0, 50, 3))
        .unwrap();
    assert_eq!(page.total, 1);
    // 中文 2 字符（<3）走 LIKE 也能命中
    let page = db
        .search(&search_query("下划", ClipFilter::All, 0, 50, 4))
        .unwrap();
    assert_eq!(page.total, 1);
}

// ---------------------------------------------------------------------------
// retention
// ---------------------------------------------------------------------------

#[test]
fn retention_prunes_oldest_non_favorite() {
    let (_dir, db) = open_db("retention-count");
    let mut ids = Vec::new();
    for index in 0..10 {
        let input = text_input_at(&format!("retention 文本 {index}"), 1_000 + index);
        ids.push(db.upsert(&input).unwrap().id());
    }
    assert!(db.set_favorited(ids[0], true).unwrap());
    assert!(db.set_pinned(ids[0], true).unwrap());
    let mut settings = Settings::default();
    settings.max_items = 5;
    let report = retention::prune(&db, &settings).unwrap();
    assert_eq!(report.deleted_clips, 5);
    assert_eq!(total_rows(&db), 5);
    assert!(db.item(ids[0]).unwrap().is_some(), "收藏条目不得被清理");
    for id in &ids[1..=5] {
        assert!(db.item(*id).unwrap().is_none(), "最旧非收藏应被删除");
    }
    for id in &ids[6..] {
        assert!(db.item(*id).unwrap().is_some(), "较新条目应保留");
    }
    assert!(db.meta_get("last_prune_at").unwrap().is_some());
    assert_eq!(db.meta_get("clip_count").unwrap(), Some("5".to_string()));
}

#[test]
fn retention_prunes_images_over_limit() {
    let (dir, db) = open_db("retention-image");
    let mut entries: Vec<(i64, PathBuf)> = Vec::new();
    for index in 0..3 {
        let png = tiny_png(20 + index as u8);
        let id = db
            .upsert(&image_input(png, 1_000 + index as i64))
            .unwrap()
            .id();
        let blob_id = db.item(id).unwrap().unwrap().blob_id.unwrap();
        entries.push((id, blob::image_path(&dir, &blob_id)));
    }
    assert!(db.set_favorited(entries[0].0, true).unwrap());
    assert!(db.set_pinned(entries[0].0, true).unwrap());
    let mut settings = Settings::default();
    settings.max_image_bytes = 1; // 除收藏外全部删除
    let report = retention::prune(&db, &settings).unwrap();
    assert!(report.deleted_clips >= 1);
    assert_eq!(db.counts().unwrap().image, 1, "只应保留收藏图片");
    assert!(db.item(entries[0].0).unwrap().is_some());
    assert!(entries[0].1.exists(), "收藏图片文件不得删除");
    assert!(db.item(entries[1].0).unwrap().is_none());
    assert!(db.item(entries[2].0).unwrap().is_none());
    assert!(!entries[1].1.exists(), "被删图片的 PNG 应物理删除");
}

#[test]
fn retention_cleans_orphan_blobs() {
    let (dir, db) = open_db("retention-orphan");
    let png = tiny_png(99);
    let blob_id = blob::put_image(&db.conn, &dir, &png).unwrap();
    let png_path = blob::image_path(&dir, &blob_id);
    assert!(png_path.exists());
    // 模拟崩溃残留 A：blob 行 refs=0 且无 clip 引用
    db.conn
        .execute("UPDATE blobs SET refs = 0 WHERE id = ?1", params![blob_id])
        .unwrap();
    // 模拟崩溃残留 B：refs=1 但没有任何 clips 引用（写入中断）
    let leaked_id = blob::put_image(&db.conn, &dir, &tiny_png(100)).unwrap();
    let leaked_path = blob::image_path(&dir, &leaked_id);
    assert!(leaked_path.exists(), "残留文件应已写入");
    let report = retention::prune(&db, &Settings::default()).unwrap();
    assert_eq!(report.deleted_blobs, 2, "两种孤儿 blob 都应被清理");
    assert!(report.freed_bytes > 0);
    assert!(!png_path.exists());
    assert!(!leaked_path.exists());
    let rows: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn retention_keeps_favorites_when_over_image_limit() {
    let (_dir, db) = open_db("retention-pinned");
    let mut ids = Vec::new();
    for index in 0..3 {
        let id = db
            .upsert(&image_input(tiny_png(30 + index), 1_000 + index as i64))
            .unwrap()
            .id();
        ids.push(id);
    }
    for id in &ids {
        assert!(db.set_favorited(*id, true).unwrap());
        assert!(db.set_pinned(*id, true).unwrap());
    }
    let mut settings = Settings::default();
    settings.max_image_bytes = 1;
    retention::prune(&db, &settings).unwrap();
    // 全部为收藏：一条都不能删
    assert_eq!(db.counts().unwrap().image, 3);
}

#[test]
fn delete_and_clear_behaviour() {
    let (dir, db) = open_db("delete-clear");
    let png = tiny_png(41);
    let image_id = db.upsert(&image_input(png, 1_000)).unwrap().id();
    let text_id = db.upsert(&text_input("删除测试文本")).unwrap().id();
    let blob_id = db.item(image_id).unwrap().unwrap().blob_id.unwrap();
    let png_path = blob::image_path(&dir, &blob_id);
    assert!(png_path.exists());

    assert_eq!(db.delete(&[image_id]).unwrap(), 1);
    assert!(db.item(image_id).unwrap().is_none());
    assert!(!png_path.exists(), "删除条目应释放 blob 引用并物理删除文件");
    assert!(db.item(text_id).unwrap().is_some());
    // 重复删除不报错
    assert_eq!(db.delete(&[image_id]).unwrap(), 0);

    assert_eq!(total_rows(&db), 1);
    db.clear(true, false).unwrap();
    assert_eq!(total_rows(&db), 0);
    assert!(!blob::images_dir(&dir).exists());
    // 清空后仍可写入（目录会自动重建）
    let new_id = db.upsert(&image_input(tiny_png(42), 2_000)).unwrap().id();
    assert!(db.item(new_id).unwrap().unwrap().blob_id.is_some());
    db.clear(false, false).unwrap();
    assert_eq!(total_rows(&db), 0);
    assert!(
        blob::images_dir(&dir).exists(),
        "delete_blobs=false 时保留物理文件"
    );
}

#[test]
fn schema_v1_migrates_trash_column() {
    let dir = temp_dir("schema-v1-trash");
    let path = dir.join("data.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE clips (
           id INTEGER PRIMARY KEY, kind INTEGER NOT NULL, hash TEXT NOT NULL UNIQUE,
           text TEXT, preview TEXT NOT NULL DEFAULT '', blob_id TEXT, files_json TEXT,
           byte_len INTEGER NOT NULL DEFAULT 0, width INTEGER, height INTEGER,
           created_at INTEGER NOT NULL, last_used_at INTEGER NOT NULL,
           use_count INTEGER NOT NULL DEFAULT 1, pinned INTEGER NOT NULL DEFAULT 0,
           source_app TEXT, source_title TEXT
         );
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta(key, value) VALUES ('schema_version', '1');
         INSERT INTO clips(kind, hash, preview, created_at, last_used_at, pinned)
         VALUES (0, 'legacy-pinned', '老收藏', 1, 1, 1);",
    )
    .unwrap();
    drop(conn);

    let db = Db::open_in_dir(&dir, DbOptions::default()).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    let has_column: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('clips') WHERE name = 'deleted_at'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(has_column, 1, "老库应自动补齐 deleted_at 列");
    let has_index: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_clips_trash'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(has_index, 1, "回收站索引应创建");
    let (favorited, pinned): (i64, i64) = db
        .conn
        .query_row(
            "SELECT favorited, pinned FROM clips WHERE hash = 'legacy-pinned'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(favorited, 1, "老收藏应迁移为 favorited");
    assert_eq!(pinned, 0, "迁移后不应继续自动置顶");
}

#[test]
fn trash_soft_delete_search_restore_and_purge() {
    let (_dir, db) = open_db("trash-lifecycle");
    let id = db.upsert(&text_input("回收站生命周期")).unwrap().id();
    let keep = db.upsert(&text_input("保留条目")).unwrap().id();

    assert_eq!(db.soft_delete(&[id]).unwrap(), 1);
    assert_eq!(db.soft_delete(&[id]).unwrap(), 0, "重复删除应为 0");
    assert!(
        db.item(id).unwrap().is_none(),
        "回收站条目不应出现在活动查询"
    );
    let stats = db.counts().unwrap();
    assert_eq!(stats.total, 1);

    let page = db
        .search(&SearchQuery {
            text: String::new(),
            filter: ClipFilter::All,
            time_range: crate::model::TimeRange::All,
            time_basis: crate::model::TimeBasis::LastUsed,
            custom_day: None,
            custom_end_day: None,
            offset: 0,
            limit: 10,
            trash: true,
            gen: 1,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, id);
    assert!(page.items[0].deleted_at.is_some());

    assert_eq!(db.restore(&[id]).unwrap(), 1);
    assert!(db.item(id).unwrap().is_some());
    assert_eq!(db.purge(&[keep]).unwrap(), 0, "活动条目不能被彻底删除");

    assert_eq!(db.soft_delete(&[id]).unwrap(), 1);
    assert_eq!(db.purge(&[id]).unwrap(), 1);
    assert_eq!(db.counts().unwrap().total, 1);
}

#[test]
fn trashed_hash_upsert_restores_and_empty_trash_works() {
    let (_dir, db) = open_db("trash-recopy");
    let input = text_input("复制命中回收站");
    let id = db.upsert(&input).unwrap().id();
    assert_eq!(db.soft_delete(&[id]).unwrap(), 1);
    let outcome = db.upsert(&input).unwrap();
    assert_eq!(outcome, UpsertOutcome::Touched(id));
    assert!(db.item(id).unwrap().is_some(), "重新复制应自动恢复");

    assert_eq!(db.soft_delete(&[id]).unwrap(), 1);
    assert_eq!(db.empty_trash().unwrap(), 1);
    assert_eq!(db.counts().unwrap().total, 0);
}

#[test]
fn expired_trash_is_purged_after_two_days() {
    let (_dir, db) = open_db("trash-expiry");
    let id = db.upsert(&text_input("过期回收站")).unwrap().id();
    assert_eq!(db.soft_delete(&[id]).unwrap(), 1);
    let now = now_ms();
    assert_eq!(
        db.purge_expired_trash(now, TRASH_RETENTION_MS).unwrap(),
        0,
        "未到期不应清理"
    );
    assert!(db.item(id).unwrap().is_none());
    assert_eq!(
        db.purge_expired_trash(now + TRASH_RETENTION_MS + 1, TRASH_RETENTION_MS)
            .unwrap(),
        1
    );
}

// ---------------------------------------------------------------------------
// A2：清空/退出清理
// ---------------------------------------------------------------------------

#[test]
fn clear_default_keeps_favorites_and_their_files() {
    let (dir, db) = open_db("clear-keep-favorites");
    let keep = db.upsert(&text_input("收藏保留")).unwrap().id();
    let plain = db.upsert(&text_input("未收藏删除")).unwrap().id();
    assert!(db.set_favorited(keep, true).unwrap());

    // 默认清空：只删未收藏，收藏保留。
    let outcome = db.clear(true, false).unwrap();
    assert_eq!(outcome.cleared, 1);
    assert_eq!(outcome.kept, 1);
    assert!(db.item(plain).unwrap().is_none());
    let kept = db.item(keep).unwrap().expect("收藏条目应保留");
    assert!(kept.favorited);

    // 含收藏：全部清空 + 删除实体目录。
    let img = db.upsert(&image_input(tiny_png(9), 1_000)).unwrap().id();
    assert!(db.set_favorited(img, true).unwrap());
    let outcome = db.clear(true, true).unwrap();
    assert_eq!(outcome.kept, 0);
    assert_eq!(db.counts().unwrap().total, 0);
    assert!(
        !blob::images_dir(&dir).exists(),
        "含收藏清空且删除实体后，图片目录应被整个清掉"
    );
}

// ---------------------------------------------------------------------------
// A4：收藏文件副本状态 + 重试
// ---------------------------------------------------------------------------

#[test]
fn favorite_file_copy_reports_state_and_can_be_retried() {
    let (dir, db) = open_db("favorite-copy-state");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let ok_path = src.join("ok.txt");
    std::fs::write(&ok_path, b"hello").unwrap();
    let missing_path = src.join("missing.txt");

    let input = files_input(&[&ok_path, &missing_path], now_ms());
    let id = db.upsert(&input).unwrap().id();
    assert!(db.set_favorited(id, true).unwrap());
    let settings = Settings {
        max_file_bytes: 1 << 20,
        max_favorite_file_bytes: 1 << 20,
        ..Default::default()
    };
    let copy = db
        .save_favorite_files(id, &settings)
        .unwrap()
        .expect("文件类型应返回复制结果");
    assert_eq!(copy.copied, 1);
    assert_eq!(copy.skipped_missing, 1);
    assert_eq!(copy.overall_state(), CopyState::Missing);

    let item = db.item(id).unwrap().expect("条目存在");
    let ok = item
        .files
        .iter()
        .find(|f| f.name == "ok.txt")
        .expect("ok.txt 存在");
    assert!(ok.stored);
    assert_eq!(ok.copy_state, Some(CopyState::Stored));
    let missing = item
        .files
        .iter()
        .find(|f| f.name == "missing.txt")
        .expect("missing.txt 存在");
    assert!(!missing.stored);
    assert_eq!(missing.copy_state, Some(CopyState::Missing));

    // 补齐源文件后重试：缺失文件应被保存，整体状态变回已保存。
    std::fs::write(&missing_path, b"world").unwrap();
    let retry = db
        .save_favorite_files(id, &settings)
        .unwrap()
        .expect("重试应返回复制结果");
    assert_eq!(retry.skipped_missing, 0);
    assert_eq!(retry.overall_state(), CopyState::Stored);
    let item = db.item(id).unwrap().unwrap();
    assert!(item.files.iter().all(|f| f.stored));
}

#[test]
fn rich_text_fields_roundtrip_through_store() {
    let (_dir, db) = open_db("rich-fields");
    let mut input = text_input("富文本正文");
    input.html = Some("<p><b>富文本正文</b></p>".to_string());
    input.rtf = Some("{\\rtf1 富文本正文}".to_string());

    let id = db.upsert(&input).unwrap().id();
    let item = db.item(id).unwrap().expect("条目应存在");
    assert_eq!(item.text.as_deref(), Some("富文本正文"));
    assert_eq!(item.html.as_deref(), Some("<p><b>富文本正文</b></p>"));
    assert_eq!(item.rtf.as_deref(), Some("{\\rtf1 富文本正文}"));

    let payload = db.paste_payload(id).unwrap().expect("粘贴载荷应存在");
    assert_eq!(payload.text.as_deref(), Some("富文本正文"));
    assert_eq!(payload.html.as_deref(), Some("<p><b>富文本正文</b></p>"));
    assert_eq!(payload.rtf.as_deref(), Some("{\\rtf1 富文本正文}"));
}

#[test]
fn plain_text_item_reads_back_without_rich_fields() {
    let (_dir, db) = open_db("rich-none");
    let id = db.upsert(&text_input("纯文本")).unwrap().id();

    let item = db.item(id).unwrap().expect("条目应存在");
    assert_eq!(item.text.as_deref(), Some("纯文本"));
    assert!(item.html.is_none(), "无富文本时 html 应为 None");
    assert!(item.rtf.is_none(), "无富文本时 rtf 应为 None");

    let payload = db.paste_payload(id).unwrap().expect("粘贴载荷应存在");
    assert_eq!(payload.text.as_deref(), Some("纯文本"));
    assert!(payload.html.is_none());
    assert!(payload.rtf.is_none());
}

#[test]
fn schema_v3_migrates_rich_text_columns() {
    let dir = temp_dir("schema-v3-rich");
    let path = dir.join("data.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE clips (
           id INTEGER PRIMARY KEY, kind INTEGER NOT NULL, hash TEXT NOT NULL UNIQUE,
           text TEXT, preview TEXT NOT NULL DEFAULT '', blob_id TEXT, files_json TEXT,
           byte_len INTEGER NOT NULL DEFAULT 0, width INTEGER, height INTEGER,
           created_at INTEGER NOT NULL, last_used_at INTEGER NOT NULL,
           use_count INTEGER NOT NULL DEFAULT 1,
           favorited INTEGER NOT NULL DEFAULT 0, pinned INTEGER NOT NULL DEFAULT 0,
           source_app TEXT, source_title TEXT, deleted_at INTEGER
         );
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta(key, value) VALUES ('schema_version', '3');
         INSERT INTO clips(kind, hash, text, preview, created_at, last_used_at, pinned)
         VALUES (0, 'legacy-rich', '旧文本', '旧文本', 1, 1, 1);",
    )
    .unwrap();
    drop(conn);

    let db = Db::open_in_dir(&dir, DbOptions::default()).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    for column in ["html", "rtf"] {
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('clips') WHERE name = ?1",
                params![column],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "v3 旧库应自动补齐 {column} 列");
    }

    let old_id: i64 = db
        .conn
        .query_row(
            "SELECT id FROM clips WHERE hash = 'legacy-rich'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let item = db.item(old_id).unwrap().expect("旧条目应保留");
    assert_eq!(item.text.as_deref(), Some("旧文本"));
    assert!(item.html.is_none(), "旧数据 html 应为 NULL");
    assert!(item.rtf.is_none(), "旧数据 rtf 应为 NULL");
    assert!(item.pinned, "v3→v4 不应再改写用户置顶");
    assert!(!item.favorited, "v3→v4 不应把置顶迁移成收藏");
    drop(db);

    // restart-safe：再次打开不应报错、也不应重复加列或重复迁移
    let db2 = Db::open_in_dir(&dir, DbOptions::default()).unwrap();
    assert_eq!(db2.schema_version().unwrap(), SCHEMA_VERSION);
    let pinned: i64 = db2
        .conn
        .query_row(
            "SELECT pinned FROM clips WHERE hash = 'legacy-rich'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pinned, 1, "重复打开后置顶状态应保持");
}

// ---------------------------------------------------------------------------
// B2/B3：多关键词 / 来源筛选 / 排序 / 回收站分页
// ---------------------------------------------------------------------------

fn bulk_query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        limit: 100,
        ..SearchQuery::default()
    }
}

#[test]
fn multi_keyword_any_and_all_on_fts_path() {
    let (_dir, db) = open_db("multi-keyword");
    let both = db.upsert(&text_input("alpha beta 同时出现")).unwrap().id();
    let alpha = db.upsert(&text_input("alpha 单独出现")).unwrap().id();
    let beta = db.upsert(&text_input("beta 单独出现")).unwrap().id();

    let any = db.search(&bulk_query("alpha beta")).unwrap();
    assert_eq!(any.total, 3, "默认 OR：任一关键词命中即可");
    let ids: Vec<ClipId> = any.items.iter().map(|item| item.id).collect();
    assert!(ids.contains(&both) && ids.contains(&alpha) && ids.contains(&beta));

    let mut all_query = bulk_query("alpha beta");
    all_query.match_mode = MatchMode::All;
    let all = db.search(&all_query).unwrap();
    assert_eq!(all.total, 1, "AND：要求两个关键词都命中");
    assert_eq!(all.items[0].id, both);
}

#[test]
fn multi_keyword_like_path_and_quoted_phrase() {
    let (_dir, db) = open_db_like("multi-keyword-like");
    db.upsert(&text_input("alpha beta 连在一起")).unwrap();
    db.upsert(&text_input("alpha 中间夹了别的 beta")).unwrap();
    db.upsert(&text_input("alpha 单独出现")).unwrap();

    // 逐词 AND：每个词都要在 text/preview/source_title 里命中
    let mut all_query = bulk_query("alpha beta");
    all_query.match_mode = MatchMode::All;
    let all = db.search(&all_query).unwrap();
    assert_eq!(all.total, 2, "非相邻的两个关键词也应 AND 命中");

    // 引号短语不拆：只有相邻出现的整段命中
    let quoted = db.search(&bulk_query("\"alpha beta\"")).unwrap();
    assert_eq!(quoted.total, 1);
    assert!(quoted.items[0]
        .text
        .as_deref()
        .unwrap()
        .contains("连在一起"));
}

#[test]
fn source_app_filter_and_distinct_list() {
    let (_dir, db) = open_db("source-filter");
    let mut chrome = text_input_at("网页内容", 1_000);
    chrome.source_app = Some("chrome.exe".to_string());
    let mut code = text_input_at("代码内容", 2_000);
    code.source_app = Some("Code.exe".to_string());
    let mut notepad = text_input_at("纯文本", 3_000);
    notepad.source_app = Some("notepad.exe".to_string());
    let chrome_id = db.upsert(&chrome).unwrap().id();
    db.upsert(&code).unwrap();
    db.upsert(&notepad).unwrap();

    let mut query = bulk_query("");
    query.source_app = Some("chrome.exe".to_string());
    let page = db.search(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, chrome_id);

    let mut query = bulk_query("");
    query.source_app = Some("Code.exe".to_string());
    assert_eq!(db.search(&query).unwrap().total, 1);
    // 不带来源 = 不过滤
    assert_eq!(db.search(&bulk_query("")).unwrap().total, 3);

    // distinct 来源列表：不含已删除记录，按应用名（小写）排序
    let mut old = text_input_at("旧来源", 4_000);
    old.source_app = Some("old.exe".to_string());
    let old_id = db.upsert(&old).unwrap().id();
    db.soft_delete(&[old_id]).unwrap();
    assert_eq!(
        db.source_apps().unwrap(),
        vec![
            "chrome.exe".to_string(),
            "Code.exe".to_string(),
            "notepad.exe".to_string()
        ]
    );
}

#[test]
fn sort_orders_match_sql() {
    let (_dir, db) = open_db("sort-orders");
    let a = db.upsert(&text_input_at("a", 1_000)).unwrap().id();
    let b = db.upsert(&text_input_at("b", 2_000)).unwrap().id();
    let c = db.upsert(&text_input_at("c", 3_000)).unwrap().id();
    let d = db.upsert(&image_input(tiny_png(21), 9_000)).unwrap().id();
    // 直接改字段，构造可区分的 last_used_at / byte_len / use_count
    for (id, byte_len, use_count, last_used_at) in [
        (a, 10_i64, 5_i64, 3_000_i64),
        (b, 200, 2, 2_000),
        (c, 1_000, 9, 1_000),
        (d, 1, 1, 9_000),
    ] {
        db.conn
            .execute(
                "UPDATE clips SET byte_len=?1, use_count=?2, last_used_at=?3 WHERE id=?4",
                params![byte_len, use_count, last_used_at, id],
            )
            .unwrap();
    }

    let expect = |sort: SortOrder| -> Vec<ClipId> {
        let mut query = bulk_query("");
        query.sort = sort;
        db.search(&query)
            .unwrap()
            .items
            .iter()
            .map(|item| item.id)
            .collect()
    };
    assert_eq!(expect(SortOrder::Recent), vec![d, a, b, c]);
    assert_eq!(expect(SortOrder::Oldest), vec![c, b, a, d]);
    assert_eq!(expect(SortOrder::Largest), vec![c, b, a, d]);
    assert_eq!(expect(SortOrder::MostUsed), vec![c, a, b, d]);
    // 类型排序：文本在前、图片在后；同类型内仍按最近使用
    assert_eq!(expect(SortOrder::Type), vec![a, b, c, d]);

    // 收藏页：置顶项永远优先，再按所选排序
    assert!(db.set_favorited(a, true).unwrap());
    assert!(db.set_pinned(a, true).unwrap());
    assert!(db.set_favorited(b, true).unwrap());
    let mut favorites = bulk_query("");
    favorites.filter = ClipFilter::Favorites;
    favorites.sort = SortOrder::Largest;
    let ids: Vec<ClipId> = db
        .search(&favorites)
        .unwrap()
        .items
        .iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(ids, vec![a, b], "置顶优先，其次才是最大文件");
}

#[test]
fn trash_pagination_covers_all_rows_without_duplicates() {
    let (_dir, db) = open_db("trash-paging");
    let ids: Vec<ClipId> = (0..6)
        .map(|index| {
            db.upsert(&text_input_at(
                &format!("回收站条目{index}"),
                1_000 + index * 100,
            ))
            .unwrap()
            .id()
        })
        .collect();
    assert_eq!(db.soft_delete(&ids[..5]).unwrap(), 5);

    let mut seen = Vec::new();
    let mut offset = 0;
    loop {
        let query = SearchQuery {
            text: String::new(),
            limit: 2,
            offset,
            trash: true,
            ..SearchQuery::default()
        };
        let page = db.search(&query).unwrap();
        assert_eq!(page.total, 5, "翻页过程中 total 保持稳定");
        if page.items.is_empty() {
            break;
        }
        seen.extend(page.items.iter().map(|item| item.id));
        offset += page.items.len();
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 5, "分页覆盖全部回收站记录且无重复");
}

#[test]
fn like_search_matches_source_title_and_file_paths() {
    let (_dir, db) = open_db_like("source-title-like");
    let mut note = text_input_at("会议纪要正文", 1_000);
    note.source_title = Some("季度规划.docx".to_string());
    let note_id = db.upsert(&note).unwrap().id();
    let mut other = text_input_at("别的内容", 2_000);
    other.source_title = Some("无关文档.txt".to_string());
    db.upsert(&other).unwrap();

    let page = db.search(&bulk_query("季度规划")).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, note_id);

    // 带路径分隔符的文件名走 LIKE 路径，仍可搜到
    let mut file = text_input_at("附件", 3_000);
    file.preview = "预算表.xlsx".to_string();
    file.files = vec![FileEntry::from_path(Path::new(
        "C:\\Users\\demo\\Documents\\预算表.xlsx",
    ))];
    let file_id = db.upsert(&file).unwrap().id();
    let page = db.search(&bulk_query("预算表")).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, file_id);
}

#[test]
fn database_encryption_roundtrip_and_disable() {
    let (dir, mut db) = open_db("db-encryption");
    let input = text_input_at("数据库加密测试", now_ms());
    db.upsert(&input).unwrap();
    assert!(db.set_encryption(true).unwrap());
    assert!(crate::db_crypto::key_exists(&dir));
    assert!(crate::db_crypto::is_encrypted_file(
        &dir.join(super::db::DB_FILE)
    ));
    drop(db);

    let key = crate::db_crypto::load_key_hex(&dir).unwrap();
    let mut db = Db::open_in_dir(
        &dir,
        DbOptions {
            key_hex: key.clone(),
            ..Default::default()
        },
    )
    .expect("加密库应能用本机密钥打开");
    assert_eq!(db.counts().unwrap().total, 1);
    assert!(db.set_encryption(false).unwrap());
    assert!(!crate::db_crypto::key_exists(&dir));
    assert!(!crate::db_crypto::is_encrypted_file(
        &dir.join(super::db::DB_FILE)
    ));
    drop(db);

    let db = Db::open_in_dir(&dir, DbOptions::default()).expect("关闭加密后应能用明文打开");
    assert_eq!(db.counts().unwrap().total, 1);
}

//! 命令行诊断：`--selftest`、`--diagnose`、`--demo-seed`。
//!
//! 原则：诊断只读、不写用户数据；自检/演示数据使用隔离临时目录或显式 `--yes` 确认，
//! 绝不悄悄修改用户真实历史。

use crate::model::{
    now_ms, ClipFilter, ClipInput, ClipKind, Event, FileEntry, PastePayload, SearchPage,
    SearchQuery, Settings, StoreCmd,
};
use crate::store::StoreHandle;
use eframe::egui;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 事件等待上限。
const WAIT: Duration = Duration::from_secs(6);

/// 无界面全链路自检（隔离临时目录，绝不碰真实数据）。
pub fn selftest(out_path: &str) -> Result<String, String> {
    let started = Instant::now();
    let temp_dir = std::env::temp_dir().join(format!(
        "clipnest-selftest-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        crate::logging::log(&format!(
            "selftest [{}] {name}：{detail}",
            if ok { "PASS" } else { "FAIL" }
        ));
        checks.push(json!({"name": name, "ok": ok, "detail": detail}));
    };

    let result = run_selftest_body(&temp_dir, &mut check);
    let ok = result.is_ok() && checks.iter().all(|c| c["ok"] == true);
    let report = json!({
        "ok": ok,
        "temp_dir": temp_dir.display().to_string(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "checks": checks,
        "error": result.err(),
    });
    write_json(out_path, &report)?;
    let _ = std::fs::remove_dir_all(&temp_dir);
    if ok {
        Ok(out_path.to_string())
    } else {
        Err(format!("自检失败，详见 {out_path}"))
    }
}

type CheckFn<'a> = dyn FnMut(&str, bool, String) + 'a;

fn run_selftest_body(dir: &Path, check: &mut CheckFn<'_>) -> Result<(), String> {
    let (events_tx, events) = mpsc::channel::<Event>();
    let settings = Arc::new(Mutex::new(Settings::default()));
    let ctx = egui::Context::default();
    let store = StoreHandle::spawn_at(dir.to_path_buf(), events_tx, settings, ctx)
        .map_err(|e| format!("Store 启动失败：{e}"))?;
    let tx = store.cmd_tx();

    // 1) 文本写入 + 搜索
    let text = "selftest 中文文本：ClipNest 自检样例\n第二行内容".to_string();
    tx.send(StoreCmd::Upsert(text_input(&text)))
        .map_err(|e| e.to_string())?;
    let changed = wait_clips_changed(&events, WAIT);
    check("文本写入事件", changed, "收到 ClipsChanged".into());

    let page = search(&events, &tx, "中文文本", ClipFilter::Text, 0, 20);
    let text_hit = page
        .as_ref()
        .map(|p| p.total >= 1 && p.items.iter().any(|i| i.kind == ClipKind::Text))
        .unwrap_or(false);
    check(
        "中文子串搜索（FTS/LIKE）",
        text_hit,
        format!(
            "total={} items={}",
            page.as_ref().map(|p| p.total).unwrap_or(0),
            page.as_ref().map(|p| p.items.len()).unwrap_or(0)
        ),
    );
    let text_id = page
        .as_ref()
        .and_then(|p| p.items.first())
        .map(|i| i.id)
        .unwrap_or(0);

    // 2) 去重置顶
    tx.send(StoreCmd::Upsert(text_input(&text)))
        .map_err(|e| e.to_string())?;
    let _ = wait_clips_changed(&events, WAIT);
    let page2 = search(&events, &tx, "中文文本", ClipFilter::Text, 0, 20);
    check(
        "重复复制去重",
        page2.as_ref().map(|p| p.total == 1).unwrap_or(false),
        format!(
            "total={}（期望 1）",
            page2.as_ref().map(|p| p.total).unwrap_or(0)
        ),
    );
    let use_count_ok = page2
        .as_ref()
        .and_then(|p| p.items.first())
        .map(|i| i.use_count >= 2)
        .unwrap_or(false);
    check("重复复制 use_count+1", use_count_ok, "use_count>=2".into());

    // 3) 收藏 + 置顶排序
    if text_id > 0 {
        tx.send(StoreCmd::SetFavorited {
            id: text_id,
            favorited: true,
        })
        .map_err(|e| e.to_string())?;
        let _ = wait_clips_changed(&events, WAIT);
        tx.send(StoreCmd::SetPinned {
            id: text_id,
            pinned: true,
        })
        .map_err(|e| e.to_string())?;
        let _ = wait_clips_changed(&events, WAIT);
        let page3 = search(&events, &tx, "", ClipFilter::Favorites, 0, 20);
        check(
            "收藏过滤",
            page3
                .as_ref()
                .map(|p| p.items.iter().any(|i| i.id == text_id && i.favorited))
                .unwrap_or(false),
            format!("total={}", page3.as_ref().map(|p| p.total).unwrap_or(0)),
        );
    }

    // 4) 图片写入 + 载荷路径
    let png = make_test_png(64, 48)?;
    let mut image_input = ClipInput {
        kind: ClipKind::Image,
        hash: hash_of(ClipKind::Image, &png),
        preview: "图片 64×48".into(),
        text: None,
        image_png: Some(png),
        files: Vec::new(),
        byte_len: 0,
        width: Some(64),
        height: Some(48),
        source_app: Some("selftest.exe".into()),
        source_title: None,
        captured_at: now_ms(),
    };
    image_input.byte_len = image_input
        .image_png
        .as_ref()
        .map(|p| p.len() as u64)
        .unwrap_or(0);
    tx.send(StoreCmd::Upsert(image_input))
        .map_err(|e| e.to_string())?;
    let _ = wait_clips_changed(&events, WAIT);
    let page_img = search(&events, &tx, "", ClipFilter::Image, 0, 20);
    let image_id = page_img
        .as_ref()
        .and_then(|p| p.items.first())
        .map(|i| i.id)
        .unwrap_or(0);
    check(
        "图片写入",
        image_id > 0,
        format!("total={}", page_img.as_ref().map(|p| p.total).unwrap_or(0)),
    );
    let payload = get_payload(&events, &tx, image_id);
    let image_payload_ok = payload
        .as_ref()
        .map(|p| p.png_path.as_ref().map(|p| p.exists()).unwrap_or(false))
        .unwrap_or(false);
    check(
        "图片载荷路径存在",
        image_payload_ok,
        payload
            .as_ref()
            .and_then(|p| p.png_path.as_ref())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "无载荷".into()),
    );

    // 5) 文件写入 + 收藏实体复制 + 源文件删除后恢复
    let src_file = dir.join("selftest-file.txt");
    std::fs::write(&src_file, b"clipnest selftest file content").map_err(|e| e.to_string())?;
    let files = vec![FileEntry::from_path(&src_file)];
    let hash = hash_of(
        ClipKind::Files,
        files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
    );
    tx.send(StoreCmd::Upsert(ClipInput {
        kind: ClipKind::Files,
        hash,
        preview: files[0].name.clone(),
        text: None,
        image_png: None,
        files: files.clone(),
        byte_len: 0,
        width: None,
        height: None,
        source_app: Some("selftest.exe".into()),
        source_title: None,
        captured_at: now_ms(),
    }))
    .map_err(|e| e.to_string())?;
    let _ = wait_clips_changed(&events, WAIT);
    let page_files = search(&events, &tx, "", ClipFilter::Files, 0, 20);
    let file_id = page_files
        .as_ref()
        .and_then(|p| p.items.first())
        .map(|i| i.id)
        .unwrap_or(0);
    check(
        "文件写入",
        file_id > 0,
        format!(
            "total={}",
            page_files.as_ref().map(|p| p.total).unwrap_or(0)
        ),
    );
    // 收藏触发实体复制
    tx.send(StoreCmd::SetFavorited {
        id: file_id,
        favorited: true,
    })
    .map_err(|e| e.to_string())?;
    let _ = wait_clips_changed(&events, WAIT);
    let payload = get_payload(&events, &tx, file_id);
    let stored = payload
        .as_ref()
        .map(|p| p.files.iter().any(|f| f.stored))
        .unwrap_or(false);
    check("收藏文件实体复制", stored, "files[].stored=true".into());
    // 删除源文件后再次取载荷 → 应从 restored/ 恢复
    let _ = std::fs::remove_file(&src_file);
    let payload2 = get_payload(&events, &tx, file_id);
    let restored_ok = payload2
        .as_ref()
        .map(|p| p.files.iter().all(|f| Path::new(&f.path).exists()))
        .unwrap_or(false);
    check(
        "删除源文件后恢复粘贴",
        restored_ok,
        payload2
            .as_ref()
            .map(|p| p.files.first().map(|f| f.path.clone()).unwrap_or_default())
            .unwrap_or_else(|| "无载荷".into()),
    );

    // 6) 统计
    tx.send(StoreCmd::RefreshStats).map_err(|e| e.to_string())?;
    let stats = wait_stats(&events, WAIT);
    check(
        "统计信息",
        stats.as_ref().map(|s| s.total >= 3).unwrap_or(false),
        stats
            .as_ref()
            .map(|s| {
                format!(
                    "total={} text={} image={} files={}",
                    s.total, s.text, s.image, s.files
                )
            })
            .unwrap_or_else(|| "未收到 Stats".into()),
    );

    // 7) 导出 JSON
    let export_path = dir.join("selftest-export.json");
    tx.send(StoreCmd::ExportJson {
        path: export_path.clone(),
    })
    .map_err(|e| e.to_string())?;
    let export_ok = wait_for_duration(&events, WAIT, || {
        std::fs::metadata(&export_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    });
    check("导出 JSON", export_ok, export_path.display().to_string());

    // 8) 删除 + 清空
    tx.send(StoreCmd::Delete(vec![text_id, image_id]))
        .map_err(|e| e.to_string())?;
    let _ = wait_clips_changed(&events, WAIT);
    let page_after_delete = search(&events, &tx, "", ClipFilter::All, 0, 50);
    check(
        "删除记录",
        page_after_delete
            .as_ref()
            .map(|p| !p.items.iter().any(|i| i.id == text_id || i.id == image_id))
            .unwrap_or(false),
        format!(
            "total={}",
            page_after_delete.as_ref().map(|p| p.total).unwrap_or(0)
        ),
    );
    tx.send(StoreCmd::Clear { delete_blobs: true })
        .map_err(|e| e.to_string())?;
    let _ = wait_clips_changed(&events, WAIT);
    let page_after_clear = search(&events, &tx, "", ClipFilter::All, 0, 50);
    check(
        "清空历史",
        page_after_clear
            .as_ref()
            .map(|p| p.total == 0)
            .unwrap_or(false),
        format!(
            "total={}",
            page_after_clear.as_ref().map(|p| p.total).unwrap_or(0)
        ),
    );

    drop(store);
    Ok(())
}

// ---------------------------------------------------------------------------
// 帮助函数
// ---------------------------------------------------------------------------

fn search(
    events: &Receiver<Event>,
    tx: &std::sync::mpsc::Sender<StoreCmd>,
    text: &str,
    filter: ClipFilter,
    offset: usize,
    limit: usize,
) -> Option<SearchPage> {
    let gen = now_ms() as u64;
    if tx
        .send(StoreCmd::Search(SearchQuery {
            text: text.to_string(),
            filter,
            offset,
            limit,
            trash: false,
            gen,
        }))
        .is_err()
    {
        return None;
    }
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::SearchPage(page)) if page.gen == gen => return Some(page),
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    None
}

fn get_payload(
    events: &Receiver<Event>,
    tx: &std::sync::mpsc::Sender<StoreCmd>,
    id: i64,
) -> Option<PastePayload> {
    if id <= 0 {
        return None;
    }
    tx.send(StoreCmd::GetPastePayload(id)).ok()?;
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if let Ok(Event::PastePayload(payload)) = events.recv_timeout(Duration::from_millis(200)) {
            if payload.clip_id == id {
                return Some(payload);
            }
        }
    }
    None
}

fn wait_clips_changed(events: &Receiver<Event>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Event::ClipsChanged) = events.recv_timeout(Duration::from_millis(200)) {
            return true;
        }
    }
    false
}

fn wait_stats(events: &Receiver<Event>, timeout: Duration) -> Option<crate::model::Stats> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Event::Stats(stats)) = events.recv_timeout(Duration::from_millis(200)) {
            return Some(stats);
        }
    }
    None
}

fn wait_for_duration(
    events: &Receiver<Event>,
    timeout: Duration,
    mut condition: impl FnMut() -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        let _ = events.recv_timeout(Duration::from_millis(100));
    }
    condition()
}

fn text_input(text: &str) -> ClipInput {
    ClipInput {
        kind: ClipKind::Text,
        hash: hash_of(ClipKind::Text, text.as_bytes()),
        preview: text.chars().take(120).collect(),
        text: Some(text.to_string()),
        image_png: None,
        files: Vec::new(),
        byte_len: text.len() as u64,
        width: None,
        height: None,
        source_app: Some("selftest.exe".into()),
        source_title: Some("ClipNest 自检".into()),
        captured_at: now_ms(),
    }
}

fn hash_of(kind: ClipKind, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update([kind.as_i32() as u8]);
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn make_test_png(width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut img = image::RgbaImage::new(width, height);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        *pixel = image::Rgba([
            (x * 255 / width.max(1)) as u8,
            (y * 255 / height.max(1)) as u8,
            180,
            255,
        ]);
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(|e| format!("PNG 编码失败：{e}"))?;
    Ok(cursor.into_inner())
}

// ---------------------------------------------------------------------------
// 诊断
// ---------------------------------------------------------------------------

/// 环境与能力诊断：OS/路径/SQLite FTS5/窗口材质/数据统计。
pub fn diagnose(out_path: &str) -> Result<String, String> {
    crate::logging::init(&crate::config::logs_dir(), "info");
    let settings = crate::config::load_settings();
    let data_dir = crate::config::data_dir();
    let db = crate::config::db_path();
    let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    let (major, build) = crate::window::os_version();

    let report = json!({
        "app": {
            "name": crate::model::APP_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "arch": std::env::consts::ARCH,
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "os": {
            "platform": std::env::consts::OS,
            "version_major": major,
            "build": build,
            "is_windows_11": build >= 22000,
        },
        "paths": {
            "data_dir": data_dir.display().to_string(),
            "db": db.display().to_string(),
            "logs": crate::config::logs_dir().display().to_string(),
            "db_bytes": db_bytes,
            "settings_exists": crate::config::settings_path().exists(),
        },
        "capabilities": {
            "sqlite_version": rusqlite::version(),
            "fts5_trigram": fts5_trigram_available(),
            "dwm_mica_tabbed": crate::window::supports_mica(),
        },
        "settings": settings,
        "log": crate::logging::path().map(|p| p.display().to_string()),
    });

    write_json(out_path, &report)?;
    Ok(out_path.to_string())
}

/// 写入演示数据（需要 `--yes` 确认）。
pub fn demo_seed(count: usize, confirmed: bool) -> Result<String, String> {
    if !confirmed {
        return Err("写入演示数据会修改真实数据库，请加 --yes 确认".to_string());
    }
    crate::logging::init(&crate::config::logs_dir(), "info");
    let dir = crate::config::data_dir();
    crate::config::ensure_dirs().map_err(|e| format!("创建数据目录失败：{e}"))?;
    let (events_tx, events) = mpsc::channel::<Event>();
    let settings = Arc::new(Mutex::new(Settings::default()));
    let ctx = egui::Context::default();
    let store = StoreHandle::spawn_at(dir.clone(), events_tx, settings, ctx)
        .map_err(|e| format!("Store 启动失败：{e}"))?;
    let tx = store.cmd_tx();

    let demo_files_dir = dir.join("demo-files");
    let _ = std::fs::create_dir_all(&demo_files_dir);
    let topics = [
        "项目周报：本周完成剪贴板监听与去重逻辑，下周开始 UI 联调。",
        "SELECT id, kind, preview FROM clips WHERE pinned = 1 ORDER BY last_used_at DESC;",
        "会议纪要：周三 14:00 评审 ClipNest 视觉稿，参与人：产品、设计、前端。",
        "// 这样就可以\nlet mut v = vec![1, 2];\nv.push(3);",
        "旅行清单：护照、充电器、转换插头、雨伞、相机电池。",
        "SQLite FTS5 trigram tokenizer 对中文子串搜索友好，短查询回退 LIKE。",
        "git commit -m \"feat: add clipboard history persistence\"",
        "英文 sample text for clipboard demo — quick brown fox jumps over the lazy dog.",
        "https://example.com/docs/clipnest/design?section=picker",
        "灵感：面板顶部放一行状态：已暂停记录 / 已收藏 N 条 / 图片占用 12.3 MB。",
    ];
    for i in 0..count {
        let topic = topics[i % topics.len()];
        let text = format!("#{i:04} {topic}");
        let input = if i % 10 == 3 {
            let png = make_test_png(96 + (i as u32 % 64), 64)?;
            ClipInput {
                kind: ClipKind::Image,
                hash: hash_of(ClipKind::Image, &png),
                preview: "演示图片".into(),
                text: None,
                image_png: Some(png),
                files: Vec::new(),
                byte_len: 0,
                width: Some(96),
                height: Some(64),
                source_app: Some("demo-seed.exe".into()),
                source_title: Some("ClipNest 演示".into()),
                captured_at: now_ms() - (count as i64 - i as i64) * 1000,
            }
        } else if i % 20 == 7 {
            let f = demo_files_dir.join(format!("演示文件-{i:04}.txt"));
            let _ = std::fs::write(&f, format!("演示文件内容 #{i}\n{topic}\n"));
            let files = vec![FileEntry::from_path(&f)];
            ClipInput {
                kind: ClipKind::Files,
                hash: hash_of(
                    ClipKind::Files,
                    files
                        .iter()
                        .map(|f| f.path.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .as_bytes(),
                ),
                preview: files[0].name.clone(),
                text: None,
                image_png: None,
                files,
                byte_len: 0,
                width: None,
                height: None,
                source_app: Some("demo-seed.exe".into()),
                source_title: Some("ClipNest 演示".into()),
                captured_at: now_ms() - (count as i64 - i as i64) * 1000,
            }
        } else {
            ClipInput {
                kind: ClipKind::Text,
                hash: hash_of(ClipKind::Text, text.as_bytes()),
                preview: text.chars().take(120).collect(),
                text: Some(text.clone()),
                image_png: None,
                files: Vec::new(),
                byte_len: text.len() as u64,
                width: None,
                height: None,
                source_app: Some("demo-seed.exe".into()),
                source_title: Some("ClipNest 演示".into()),
                captured_at: now_ms() - (count as i64 - i as i64) * 1000,
            }
        };
        tx.send(StoreCmd::Upsert(input))
            .map_err(|e| format!("发送演示数据失败：{e}"))?;
        if i % 200 == 199 {
            let _ = events.recv_timeout(Duration::from_millis(500));
        }
    }
    // 等最后一次写入落库：channel 是 FIFO，RefreshStats 返回时之前 Upsert 均已处理。
    tx.send(StoreCmd::RefreshStats).ok();
    let stats = wait_stats(&events, Duration::from_secs(15));
    let total = stats.as_ref().map(|s| s.total).unwrap_or(0);
    let stats_ok = stats.is_some();
    drop(store);
    Ok(format!(
        "已写入 {count} 条演示数据到 {}（当前总数 {total}{}）",
        dir.display(),
        if stats_ok { "" } else { "，统计超时" }
    ))
}

// ---------------------------------------------------------------------------
// 通用
// ---------------------------------------------------------------------------

fn write_json(path: &str, value: &serde_json::Value) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(value).map_err(|e| format!("序列化诊断结果失败: {e}"))?;
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    std::fs::write(path, text).map_err(|e| format!("写诊断文件失败: {e}"))?;
    Ok(())
}

/// 运行时探测 FTS5 + trigram tokenizer 是否可用（内存库，不碰用户数据）。
pub fn fts5_trigram_available() -> bool {
    let Ok(conn) = rusqlite::Connection::open_in_memory() else {
        return false;
    };
    conn.execute_batch(
        "CREATE VIRTUAL TABLE probe USING fts5(body, tokenize='trigram');
         INSERT INTO probe(body) VALUES ('clipnest 中文搜索 probe');
         SELECT * FROM probe WHERE probe MATCH '中文';",
    )
    .is_ok()
}

//! 存储域线程与性能测试：StoreHandle 命令/事件闭环、Shutdown、5000 条搜索延迟。

use super::db::Db;
use super::tests::*;
use super::StoreHandle;
use crate::model::*;
use rusqlite::params;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// 线程入口 / 事件
// ---------------------------------------------------------------------------

fn recv_matching<F>(rx: &std::sync::mpsc::Receiver<Event>, timeout: Duration, mut pred: F) -> Event
where
    F: FnMut(&Event) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "等待事件超时");
        match rx.recv_timeout(remaining) {
            Ok(event) => {
                if pred(&event) {
                    return event;
                }
            }
            Err(err) => panic!("等待事件失败：{err}"),
        }
    }
}

#[test]
fn store_thread_processes_commands_and_shuts_down() {
    let dir = temp_dir("thread");
    let export_dir = dir.clone();
    let (event_tx, event_rx) = mpsc::channel::<Event>();
    let settings = Arc::new(Mutex::new(Settings::default()));
    let ctx = eframe::egui::Context::default();
    let mut handle =
        StoreHandle::spawn_at(dir, event_tx, settings, ctx).expect("Store 线程启动失败");
    let tx = handle.cmd_tx();

    // Upsert → ClipsChanged
    tx.send(StoreCmd::Upsert(text_input("线程测试文本")))
        .unwrap();
    let event = recv_matching(&event_rx, Duration::from_secs(10), |event| {
        matches!(event, Event::ClipsChanged)
    });
    assert!(matches!(event, Event::ClipsChanged));

    tx.send(StoreCmd::Upsert(text_input("线程测试文本二")))
        .unwrap();
    recv_matching(&event_rx, Duration::from_secs(10), |event| {
        matches!(event, Event::ClipsChanged)
    });

    // Search → SearchPage（gen 原样带回，过期 gen 由 UI 丢弃）
    tx.send(StoreCmd::Search(search_query(
        "",
        ClipFilter::All,
        0,
        10,
        42,
    )))
    .unwrap();
    let page = match recv_matching(
        &event_rx,
        Duration::from_secs(10),
        |event| matches!(event, Event::SearchPage(page) if page.gen == 42),
    ) {
        Event::SearchPage(page) => page,
        _ => unreachable!(),
    };
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 2);
    let newest_id = page.items[0].id;
    let older_id = page.items[1].id;

    // GetPastePayload → PastePayload；默认 move_to_top_after_paste 应把较早条目置顶
    tx.send(StoreCmd::GetPastePayload(older_id)).unwrap();
    let payload = match recv_matching(&event_rx, Duration::from_secs(10), |event| {
        matches!(event, Event::PastePayload(_))
    }) {
        Event::PastePayload(payload) => payload,
        _ => unreachable!(),
    };
    assert_eq!(payload.kind, ClipKind::Text);
    assert!(payload.text.is_some());
    tx.send(StoreCmd::Search(search_query(
        "",
        ClipFilter::All,
        0,
        10,
        43,
    )))
    .unwrap();
    let page = match recv_matching(
        &event_rx,
        Duration::from_secs(10),
        |event| matches!(event, Event::SearchPage(page) if page.gen == 43),
    ) {
        Event::SearchPage(page) => page,
        _ => unreachable!(),
    };
    assert_eq!(
        page.items[0].id, older_id,
        "取载荷后应置顶（粘贴后置顶语义）"
    );
    assert!(page.items[0].use_count >= 2);

    // RefreshStats → Stats
    tx.send(StoreCmd::RefreshStats).unwrap();
    let stats = match recv_matching(&event_rx, Duration::from_secs(10), |event| {
        matches!(event, Event::Stats(_))
    }) {
        Event::Stats(stats) => stats,
        _ => unreachable!(),
    };
    assert_eq!(stats.total, 2);

    // 收藏 → 再显式置顶（两次 ClipsChanged）
    tx.send(StoreCmd::SetFavorited {
        id: newest_id,
        favorited: true,
    })
    .unwrap();
    recv_matching(&event_rx, Duration::from_secs(10), |event| {
        matches!(event, Event::ClipsChanged)
    });
    tx.send(StoreCmd::SetPinned {
        id: newest_id,
        pinned: true,
    })
    .unwrap();
    recv_matching(&event_rx, Duration::from_secs(10), |event| {
        matches!(event, Event::ClipsChanged)
    });

    // ExportJson → Toast("已导出…") + 文件落盘
    let export_path = export_dir.join("thread-export.json");
    tx.send(StoreCmd::ExportJson {
        path: export_path.clone(),
    })
    .unwrap();
    let toast = match recv_matching(&event_rx, Duration::from_secs(10), |event| {
        matches!(event, Event::Toast(_))
    }) {
        Event::Toast(message) => message,
        _ => unreachable!(),
    };
    assert!(toast.contains("已导出"), "导出成功应发 Toast：{toast}");
    assert!(export_path.exists(), "导出文件应落盘");

    // Shutdown → 线程退出，通道关闭
    tx.send(StoreCmd::Shutdown).unwrap();
    let join = handle.join.take().expect("join handle 应存在");
    join.join().expect("Store 线程应正常退出");
    assert!(
        tx.send(StoreCmd::RefreshStats).is_err(),
        "线程退出后命令通道应关闭"
    );
    drop(handle);
}

// ---------------------------------------------------------------------------
// 性能
// ---------------------------------------------------------------------------

fn seed_text_rows(db: &Db, count: usize) {
    let tx = db.conn.unchecked_transaction().unwrap();
    for index in 0..count {
        let text = format!("性能测试文本 第{index}条 关键字{}", index % 97);
        let hash = sha(&text);
        let preview = format!("性能测试文本 第{index}条");
        tx.execute(
            "INSERT INTO clips
               (kind, hash, text, preview, byte_len, created_at, last_used_at, use_count, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1, 0)",
            params![
                ClipKind::Text.as_i32(),
                hash,
                text,
                preview,
                text.len() as i64,
                index as i64
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

#[test]
fn search_5000_rows_under_300ms() {
    let (_dir, db) = open_db("perf");
    seed_text_rows(&db, 5_000);
    assert_eq!(total_rows(&db), 5_000);
    // 预热（首次查询含语句准备）
    let _ = db
        .search(&search_query("性能测试", ClipFilter::All, 0, 200, 0))
        .unwrap();
    let page = db
        .search(&search_query("性能测试", ClipFilter::All, 0, 200, 1))
        .unwrap();
    assert_eq!(page.total, 5_000);
    assert_eq!(page.items.len(), 200);
    assert!(
        page.elapsed_ms < 300,
        "5000 条搜索耗时 {}ms，超过 300ms 宽松上限",
        page.elapsed_ms
    );
    // 空查询分页
    let page = db
        .search(&search_query("", ClipFilter::All, 0, 200, 2))
        .unwrap();
    assert_eq!(page.items.len(), 200);
    assert!(page.elapsed_ms < 300, "空查询耗时 {}ms", page.elapsed_ms);
    // 过滤 + 查询组合
    let page = db
        .search(&search_query("关键字42", ClipFilter::Text, 0, 200, 3))
        .unwrap();
    assert!(page.elapsed_ms < 300);
    assert!(page.total > 0);
}

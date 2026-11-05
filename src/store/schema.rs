//! schema v4：建表、索引、FTS5（trigram 分词）、回收站/收藏/富文本列与同步触发器。
//!
//! 设计要点：
//! - 所有 DDL 都带 `IF NOT EXISTS`，`migrate` 幂等（spec §10.2）；老库缺失的列
//!   （v2 `deleted_at`、v3 `favorited`、v4 `html/rtf`）由 `pragma_table_info` 探测后
//!   `ALTER TABLE` 补齐，重启任意次都安全；
//! - FTS5 trigram 并非所有 SQLite 构建都可用：创建失败时整体回滚并按
//!   `meta.fts_enabled=0` 记降级标记，搜索走 LIKE（spec §11 / CONTRACT §2.2）；
//! - `clips_fts` 是外部内容表，靠 `clips` 上的三个触发器同步。UPDATE 触发器只跟踪
//!   `preview/text` 两列：去重时高频更新的 `last_used_at` 不会引发 FTS 重建。
//!   富文本列不进入全文索引（搜索只用 text/preview）。

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{now_ms, SCHEMA_VERSION};
use crate::store::db::DbOptions;
use crate::store::{StoreError, StoreResult};

/// 引入 `favorited` 的 schema 版本：只有低于它的老库才执行 pinned→favorited 迁移。
/// 注意不能写成 `< SCHEMA_VERSION`，否则 v3→v4 升级会再次改写用户的置顶状态。
const FAVORITE_SCHEMA_VERSION: i64 = 3;

/// 核心表结构（spec §10.2 原文）。
const CORE_SCHEMA: &str = r#"
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS clips (
  id           INTEGER PRIMARY KEY,
  kind         INTEGER NOT NULL,
  hash         TEXT    NOT NULL UNIQUE,
  text         TEXT,
  html         TEXT,
  rtf          TEXT,
  preview      TEXT    NOT NULL DEFAULT '',
  blob_id      TEXT,
  files_json   TEXT,
  byte_len     INTEGER NOT NULL DEFAULT 0,
  width        INTEGER,
  height       INTEGER,
  created_at   INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL,
  use_count    INTEGER NOT NULL DEFAULT 1,
  favorited    INTEGER NOT NULL DEFAULT 0,
  pinned       INTEGER NOT NULL DEFAULT 0,
  source_app   TEXT,
  source_title TEXT,
  deleted_at   INTEGER
);

CREATE INDEX IF NOT EXISTS idx_clips_order  ON clips(pinned DESC, last_used_at DESC);
CREATE INDEX IF NOT EXISTS idx_clips_kind   ON clips(kind, last_used_at DESC);
CREATE INDEX IF NOT EXISTS idx_clips_pinned ON clips(pinned, last_used_at DESC);

CREATE TABLE IF NOT EXISTS blobs (
  id         TEXT    PRIMARY KEY,
  kind       INTEGER NOT NULL,
  rel_path   TEXT    NOT NULL,
  byte_len   INTEGER NOT NULL,
  sha256     TEXT    NOT NULL,
  refs       INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

/// FTS5 外部内容表（trigram 分词：任意子串匹配，含中文）。
const FTS_SCHEMA: &str = "CREATE VIRTUAL TABLE clips_fts USING fts5(\n  preview, text, content='clips', content_rowid='id', tokenize='trigram'\n);";

/// 与 `clips` 同步的 FTS 触发器。`UPDATE OF preview, text` 保证只在这些列变化时重建索引。
const FTS_TRIGGERS: &str = r#"
CREATE TRIGGER IF NOT EXISTS clips_fts_ai AFTER INSERT ON clips BEGIN
  INSERT INTO clips_fts(rowid, preview, text) VALUES (new.id, new.preview, new.text);
END;
CREATE TRIGGER IF NOT EXISTS clips_fts_ad AFTER DELETE ON clips BEGIN
  INSERT INTO clips_fts(clips_fts, rowid, preview, text) VALUES ('delete', old.id, old.preview, old.text);
END;
CREATE TRIGGER IF NOT EXISTS clips_fts_au AFTER UPDATE OF preview, text ON clips BEGIN
  INSERT INTO clips_fts(clips_fts, rowid, preview, text) VALUES ('delete', old.id, old.preview, old.text);
  INSERT INTO clips_fts(rowid, preview, text) VALUES (new.id, new.preview, new.text);
END;
"#;

/// 迁移结果（FTS 状态用于诊断与测试断言）。
#[derive(Debug, Clone)]
pub struct MigrateOutcome {
    /// 本次连接是否启用 FTS5 搜索。
    pub fts_enabled: bool,
    /// 降级原因（启用时为 None）。
    pub fts_error: Option<String>,
}

/// 执行 schema 迁移（幂等）。
pub fn migrate(conn: &mut Connection, options: DbOptions) -> StoreResult<MigrateOutcome> {
    conn.execute_batch(CORE_SCHEMA)?;

    let current = read_schema_version(conn)?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::Msg(format!(
            "数据库 schema 版本 {current} 高于本程序支持的 {SCHEMA_VERSION}，请升级 ClipNest"
        )));
    }

    ensure_trash_schema(conn)?;
    ensure_favorite_schema(conn, current < FAVORITE_SCHEMA_VERSION)?;
    ensure_rich_schema(conn)?;
    let outcome = ensure_fts(conn, options)?;

    if current < SCHEMA_VERSION {
        write_meta(conn, "schema_version", &SCHEMA_VERSION.to_string())?;
    }
    let now = now_ms().to_string();
    write_meta_if_missing(conn, "created_at", &now)?;
    Ok(outcome)
}

/// 探测当前 SQLite 构建是否支持 FTS5 + trigram（内存库探测，不碰真实数据）。
/// 供测试与 `--diagnose` 诊断使用。
#[allow(dead_code)]
pub fn trigram_available() -> bool {
    let Ok(conn) = Connection::open_in_memory() else {
        return false;
    };
    conn.execute_batch("CREATE VIRTUAL TABLE probe_fts USING fts5(x, tokenize='trigram');")
        .is_ok()
}

/// v2 回收站列：老库通过 `ALTER TABLE` 补齐，并补建回收站索引。
fn ensure_trash_schema(conn: &Connection) -> StoreResult<()> {
    let has_column: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('clips') WHERE name = 'deleted_at'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !has_column {
        conn.execute("ALTER TABLE clips ADD COLUMN deleted_at INTEGER", [])?;
        crate::logging::log("Store: schema v2 已为 clips 添加 deleted_at 列");
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_clips_trash ON clips(deleted_at) WHERE deleted_at IS NOT NULL;",
    )?;
    Ok(())
}

/// v3 收藏列：老库把原来的 pinned 语义迁移为 favorited，并清除旧的自动置顶。
/// 转换以迁移前版本号为准：仅当版本 < [`FAVORITE_SCHEMA_VERSION`] 时执行，
/// 成功后写入 v3，之后（包括 v3→v4 升级）不再影响用户置顶。
fn ensure_favorite_schema(conn: &Connection, legacy: bool) -> StoreResult<()> {
    let has_column: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('clips') WHERE name = 'favorited'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !has_column {
        conn.execute(
            "ALTER TABLE clips ADD COLUMN favorited INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        crate::logging::log("Store: schema v3 已添加 favorited 列");
    }
    if legacy {
        // 迁移失败可重试：版本号只在全部迁移成功后写入，故以版本为准而不是只看列。
        conn.execute_batch(
            "UPDATE clips SET favorited = pinned WHERE pinned = 1;
             UPDATE clips SET pinned = 0;",
        )?;
        crate::logging::log("Store: schema v3 原收藏标记已迁移为收藏并取消自动置顶");
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_clips_favorite ON clips(favorited, pinned DESC, last_used_at DESC);",
    )?;
    Ok(())
}

/// v4 富文本列：`html` / `rtf`。
///
/// 与 v2/v3 一样以"列是否存在"为准（而不是版本号），保证迁移中途崩溃后重启可续。
fn ensure_rich_schema(conn: &Connection) -> StoreResult<()> {
    for (column, ddl) in [
        ("html", "ALTER TABLE clips ADD COLUMN html TEXT"),
        ("rtf", "ALTER TABLE clips ADD COLUMN rtf TEXT"),
    ] {
        let has_column: bool = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('clips') WHERE name = ?1",
            [column],
            |row| row.get::<_, i64>(0),
        )? > 0;
        if !has_column {
            conn.execute(ddl, [])?;
            crate::logging::log(&format!(
                "Store: schema v4 已为 clips 添加 {column} 列"
            ));
        }
    }
    Ok(())
}

/// 建/校验 FTS 索引；失败时回退 LIKE 并写 `meta.fts_enabled=0`。
fn ensure_fts(conn: &mut Connection, options: DbOptions) -> StoreResult<MigrateOutcome> {
    if options.disable_fts {
        write_meta(conn, "fts_enabled", "0")?;
        crate::logging::log("Store: FTS5 已按配置禁用，搜索使用 LIKE 降级");
        return Ok(MigrateOutcome {
            fts_enabled: false,
            fts_error: Some("已通过 CLIPNEST_DISABLE_FTS 禁用".to_string()),
        });
    }

    let exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'clips_fts'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if !exists {
        let tx = conn.transaction()?;
        match tx.execute_batch(FTS_SCHEMA) {
            Ok(()) => {
                tx.execute_batch(FTS_TRIGGERS)?;
                // 外部内容表创建后回填索引（表可能是新建的，也可能历史数据已存在）
                tx.execute_batch("INSERT INTO clips_fts(clips_fts) VALUES('rebuild');")?;
                tx.commit()?;
                write_meta(conn, "fts_enabled", "1")?;
                crate::logging::log("Store: FTS5 trigram 索引已启用");
                return Ok(MigrateOutcome {
                    fts_enabled: true,
                    fts_error: None,
                });
            }
            Err(err) => {
                // 回滚虚拟表创建，降级 LIKE（spec §10.2 / 风险 #6）
                drop(tx);
                let msg = err.to_string();
                write_meta(conn, "fts_enabled", "0")?;
                crate::logging::warn(&format!(
                    "Store: FTS5 trigram 不可用（{msg}），搜索降级为 LIKE"
                ));
                return Ok(MigrateOutcome {
                    fts_enabled: false,
                    fts_error: Some(msg),
                });
            }
        }
    }

    // 表已存在：补齐缺失的触发器；若触发器曾丢失则重建索引避免脏数据。
    let triggers: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'
           AND name IN ('clips_fts_ai', 'clips_fts_ad', 'clips_fts_au')",
        [],
        |row| row.get(0),
    )?;
    conn.execute_batch(FTS_TRIGGERS)?;
    if triggers < 3 {
        conn.execute_batch("INSERT INTO clips_fts(clips_fts) VALUES('rebuild');")?;
        crate::logging::warn("Store: FTS 触发器缺失，已重建全文索引");
    }
    write_meta(conn, "fts_enabled", "1")?;
    Ok(MigrateOutcome {
        fts_enabled: true,
        fts_error: None,
    })
}

// ---------------------------------------------------------------------------
// meta 读写
// ---------------------------------------------------------------------------

pub(crate) fn read_meta(conn: &Connection, key: &str) -> StoreResult<Option<String>> {
    let value = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value)
}

pub(crate) fn write_meta(conn: &Connection, key: &str, value: &str) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub(crate) fn write_meta_if_missing(conn: &Connection, key: &str, value: &str) -> StoreResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

fn read_schema_version(conn: &Connection) -> StoreResult<i64> {
    let raw = read_meta(conn, "schema_version")?;
    Ok(raw.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0))
}

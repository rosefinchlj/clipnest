# ClipNest 模块契约（CONTRACT）

> 本文件是并行开发的边界与接口约定。**修改契约必须先通知 Lead**（Lead 拥有本文件与
> `src/model.rs` 的写入权）。实现内部的私有模块可自由拆分，但跨域公开 API 必须与此一致。
>
> 阅读顺序：`docs/superpowers/specs/2026-10-31-clipnest-design.md`（行为规格）→ 本文件
> （接口）→ `docs/superpowers/plans/2026-10-31-clipnest-implementation.md`（任务拆分）。

## 0. 环境与约定

- 工程根：`E:\use\ai\deepseek\Rust\clipnest`；构建：`cargo build` / `cargo test`（Windows MSVC，Rust 1.99）。
- Cargo 已配置 rsproxy 镜像，依赖已锁定在 `Cargo.toml`；**teammate 不得修改 Cargo.toml**，
  需要新依赖/feature 先发消息给 Lead。
- 禁止运行 `git` 命令（Lead 统一提交）；禁止修改其他域的目录；禁止修改 `src/model.rs`、
  `src/main.rs`、`src/app.rs`、`Cargo.toml`、`docs/CONTRACT.md`（需要变更 → 消息 Lead）。
- 每个任务完成后必须：`cargo test` 通过（只跑自己域相关测试也可）→ 给 Lead 发完成消息
  （含：改了哪些文件、测试结果、遗留问题）。
- 日志用 `crate::logging::{log,warn,error,debug}`；**不得记录剪贴板内容全文**。
- 跨线程只允许 `std::sync::mpsc` 通道与 `Arc<Mutex<Settings>>`；禁止 `static mut`、全局可变状态。
  （实现中允许有界的 `Arc<Atomic*>` 计数/诊断快照、`Arc<Mutex<Settings>>` 只读共享与日志内部锁；
  这些只能用于计数/诊断/日志，不得用于业务数据同步——业务数据仍必须走 mpsc。）
- 中文注释/中文 UI 文案；任何线程内 panic 必须捕获并写日志，不得传播到 UI 线程。
- 文件建议 ≤800 行；公共函数要有简短中文文档注释。
- 真实数据目录 `%APPDATA%\ClipNest`：测试/自检必须用 `CLIPNEST_DATA_DIR` 指向临时目录，
  绝不读写用户真实库。

## 1. 公共模型（`src/model.rs`，Lead 维护）

已实现（节选，以源码为准）：

```rust
pub const APP_NAME: &str = "ClipNest";
pub const APP_TITLE: &str = "ClipNest · 剪贴匣";
pub const SCHEMA_VERSION: i64 = 1;
pub type ClipId = i64;
pub fn now_ms() -> i64;

pub enum ClipKind { Text = 0, Image = 1, Files = 2 }   // as_i32 / from_i32 / label / icon
pub struct FileEntry { name, path, size: u64, exists: bool, stored: bool }
pub struct ClipItem { id, kind, preview, text: Option<String>, files: Vec<FileEntry>,
                      blob_id: Option<String>, byte_len, width, height, created_at,
                      last_used_at, use_count, pinned, source_app, source_title }
pub struct ClipInput { kind, hash, preview, text: Option<String>, image_png: Option<Vec<u8>>,
                       files: Vec<FileEntry>, byte_len, width, height,
                       source_app, source_title, captured_at }
pub struct PastePayload { clip_id, kind, text: Option<String>, png_path: Option<PathBuf>,
                          files: Vec<FileEntry> }
pub enum ClipFilter { All, Text, Image, Files, Pinned }  // label/all()/next()/prev()
pub struct SearchQuery { text, filter, offset, limit, gen: u64 }
pub struct SearchPage  { gen, total, items: Vec<ClipItem>, elapsed_ms }
pub struct Stats { total, text, image, files, pinned, image_bytes, favorite_file_bytes, db_bytes }
pub struct HotkeyChord { ctrl, alt, shift, win, vk: u32 }   // label()/is_set()/is_win_v()
pub enum WinVMode { Auto, RegisterOnly, HookOnly, Off }
pub enum WinVStatus { Unknown, Registered, HookActive { compensated: bool },
                      Unavailable { reason: String } }      // label()/is_ok()
pub enum PasteKey { CtrlV, ShiftInsert }
pub struct Settings { … }   // 默认值 = spec §20 附录 A
pub enum Event { PickerRequested{target_hwnd:isize}, ManagerRequested, QuitRequested,
                 ClipsChanged{total}, SearchPage(SearchPage), PastePayload(PastePayload),
                 Stats(Stats), PasteResult{ok,reason}, Toast(String),
                 WinVStatus(WinVStatus), HotkeyStatus{ok,reason}, Captured(ClipInput) }
pub enum StoreCmd { Upsert(ClipInput), Search(SearchQuery), SetPinned{id,pinned},
                    Delete(Vec<ClipId>), Clear{delete_blobs}, GetPastePayload(ClipId),
                    SaveFavoriteFiles(ClipId), RefreshStats, ExportJson{path}, Shutdown }
pub enum SysCmd { Paste{payload,target_hwnd:isize}, WriteClipboard(PastePayload),
                  SetHotkeys{picker,winv_mode}, SetPause(bool), SetAutostart(bool), Shutdown }
```

## 2. Store 域（`src/store/**`，owner: store teammate）

### 2.1 对外 API

```rust
pub struct StoreHandle { /* private */ }
impl StoreHandle {
    /// 正常启动：使用 config::data_dir()。阻塞到 DB 打开/迁移完成。
    pub fn spawn(events: Sender<Event>, settings: Arc<Mutex<Settings>>, ctx: egui::Context)
        -> Result<Self, StoreError>;
    /// 指定数据目录（--selftest / 测试用）。
    pub fn spawn_at(data_dir: PathBuf, events: Sender<Event>, settings: Arc<Mutex<Settings>>,
                    ctx: egui::Context) -> Result<Self, StoreError>;
    pub fn cmd_tx(&self) -> Sender<StoreCmd>;
}
pub enum StoreError { Db(#[from] rusqlite::Error), Io(#[from] std::io::Error), Msg(String) }
pub type StoreResult<T> = Result<T, StoreError>;
```

### 2.2 行为约定

- 内部模块建议：`db.rs`（schema/CRUD/search/retention）、`blob.rs`（内容寻址文件库）、
  `export.rs`、`tests.rs`；`store.rs` 只做线程与命令分发。
- 线程启动：打开 `data_dir/data.db`（WAL、synchronous=NORMAL、cache_size=-2048、foreign_keys=ON），
  `migrate()`；失败返回 Err（UI 弹错误对话框）。FTS5 创建失败 → `meta.fts_enabled=0` 走 LIKE。
- 收到 `StoreCmd` 处理顺序：`Upsert` 去重/写库后发 `Event::ClipsChanged{total}`（每条最多一次，
  同一帧多条 Upsert 可合并成一个事件）；`Search` 返回 `Event::SearchPage(SearchPage{gen,…})`；
  `GetPastePayload` 返回 `Event::PastePayload`；`RefreshStats` 发 `Event::Stats`。
- 每次命令处理后 `ctx.request_repaint()`。
- `Upsert` 规则：`hash` 唯一；命中已存在 → `last_used_at=now, use_count+=1`（`pinned` 不变），
  返回 `Touched`；否则插入新行、分配 blob、更新统计；写库后按设置执行 `retention::prune`。
- `Search`：空文本按 `pinned DESC, last_used_at DESC` 分页；`ClipFilter::Pinned` 只查 `pinned=1`；
  文本查询 ≥3 字符优先 FTS5 trigram（`MATCH '"…"'` 转义双引号），<3 字符或 FTS 关闭时
  `LIKE '%'||?||'%' ESCAPE '\'`（同时转义 `% _ \`）；返回 `total` 与 `elapsed_ms`。
- `Delete`：删除行（FTS 触发器同步）、`blobs.refs-=1`，refs=0 的 blob 文件可立即删除；
  `Clear{delete_blobs}`：清空 `clips` 与 FTS（保留 `pinned`？**不，清空全部**，UI 二次确认），
  `delete_blobs=true` 时清空 `blobs/images` 与 `files` 目录。
- `SaveFavoriteFiles(id)`：把该条 `files[].path` 复制到 `data_dir/blobs/files/<blob_id>/<name>`，
  更新 `files[].stored/exists`，受 `max_file_bytes`、`max_favorite_file_bytes` 限制；
  超限 → 不发事件，只写日志与 `Event::Toast`。
- `GetPastePayload`：图片拼 `data_dir/blobs/images/…` 绝对路径；收藏文件若 `stored` 且原路径
  不存在 → 还原到 `data_dir/restored/<时间戳>/` 并返回新路径（`files[].path` 更新）。
- Blob 布局（与 spec §10.1 一致）：`blobs/images/<xx>/<yy>/<sha>.png`、
  `blobs/files/<blob_id>/<原文件名>`、`blobs/thumbs/…`。
- retention（启动 + 每 100 次写入 + `RefreshStats` 时）：条数超限删最旧非收藏；孤儿 blob 清理；
  收藏文件实体超限**停止新增**、不删已有（见 spec §10.4）。
- `ExportJson{path}`：导出 `{version, exported_at, settings?（不含敏感）, clips:[{kind,text,created_at,pinned,files}]}`；
  图片/文件不内嵌（记录路径与 blob 引用）；成功 `Event::Toast`。

### 2.3 测试要求

- 单元测试直接测 `db.rs`/`blob.rs` 的内部 API（临时目录/内存库）；`StoreHandle` 线程测试用
  `spawn_at(temp_dir, …)` + 通道断言。
- 必测：schema 幂等、去重置顶、分页/过滤/排序、FTS 与 LIKE 双路径、特殊字符、保留策略、
  blob 去重与 refs、收藏文件复制/恢复、导出导入往返、5000 条搜索 <100ms（宽松 <300ms）。

## 3. Sys 域（`src/sys/**` + `src/clipboard/**`，owner: win32 teammate）

### 3.1 对外 API

```rust
pub struct SysHandle { /* private */ }
impl SysHandle {
    pub fn spawn(events: Sender<Event>, settings: Arc<Mutex<Settings>>, ctx: egui::Context)
        -> Result<Self, SysError>;
    pub fn cmd_tx(&self) -> Sender<SysCmd>;
}
pub enum SysError { Msg(String), Win32(String) }
```

启动参数中**没有** Store 命令发送端：采集到的 `ClipInput` 以 `Event::Captured(ClipInput)` 发给
UI 线程，由 Lead 在 `app.rs` 转发给 Store（保持 Sys 不依赖 Store）。

### 3.2 线程与职责

- Sys 线程（唯一 Win32 消息循环）：注册窗口类 `ClipNestHidden`（`HWND_MESSAGE` 隐藏窗口），
  `AddClipboardFormatListener`、`RegisterHotKey`（备用键 + Win+V L1）、`WH_KEYBOARD_LL`（Win+V L2）、
  单实例 `WM_COPYDATA`（`FindWindowW("ClipNestHidden")`）。所有 Win32 句柄只在该线程使用。
- 采集：`WM_CLIPBOARDUPDATE` → `clipboard::read_clipboard()` → `clipboard::normalize()` →
  `Event::Captured(ClipInput)`。跳过：`Clipboard Viewer Ignore` 格式、忽略名单进程、暂停状态、
  自身回写（内部序列号/标记）。
- 热键：`WM_HOTKEY` / Win+V 钩子命中 → 记录 `GetForegroundWindow()` → `Event::PickerRequested{target_hwnd}`。
- 托盘：`tray-icon` 创建在 UI 线程（`ClipNestApp::new` 内由 Lead 调用 Sys 暴露的
  `SysHandle::create_tray(...)`？**约定：托盘由 Sys 线程创建**，菜单事件转成对应 `Event`：
  打开面板→`PickerRequested`、管理历史→`ManagerRequested`、暂停→`Event::Toast`+设置由 UI 落盘
  （见下）、清空→`Event::Toast` 交由 UI 确认、设置→`ManagerRequested`、退出→`QuitRequested`。
  托盘图标使用 `assets/icon.png`（Lead 提供），加载失败用内置 16×16 纯色兜底。
- 命令处理：
  - `Paste{payload, target_hwnd}`：UI 已隐藏窗口 → Sys 写剪贴板 → `paste::restore_and_paste(target_hwnd, key)`
    （失败区分 `TargetInvalid/UipiBlocked/SendInputFailed`）→ `Event::PasteResult`。
  - `WriteClipboard`：只写剪贴板。
  - `SetHotkeys`：注销旧热键/钩子后按新配置注册，结果 `Event::HotkeyStatus`/`Event::WinVStatus`。
  - `SetPause`：更新 `Arc<Mutex<Settings>>.pause_capture`。
  - `SetAutostart`：写 `HKCU\...\Run` 值 `ClipNest` = `"<exe路径>"`，失败回 `Event::Toast`。
  - `Shutdown`：卸钩子、注销热键、移除托盘、`PostQuitMessage`、线程 join。
- Win+V 三级策略（spec §13）：
  1. L1 `RegisterHotKey(0, ID, MOD_WIN, 'V')` 成功 → `WinVStatus::Registered`；
  2. 失败且 `WinVMode != Off` → L2 `WH_KEYBOARD_LL`：自身跟踪 Win 键状态，`V` down/up 命中
     吞键（忽略 `LLKHF_INJECTED`）并发 `PickerRequested`；按 P0 实测决定是否补偿开始菜单
     （吞 Win up + 注入补偿等），状态 `HookActive{compensated}`；
  3. 都失败或 `Off` → `WinVStatus::Unavailable{reason}`，备用快捷键仍可用。
- `clipboard` 域只被 Sys 调用，API 由 win32 teammate 自定，但必须满足：
  `read_clipboard() -> Result<RawClipboard, ClipError>`、`normalize(RawClipboard, &CaptureLimits) -> Option<ClipInput>`、
  `write_clipboard(&PastePayload, data_dir: &Path) -> Result<(), ClipError>`；格式优先级
  HDROP > UNICODETEXT > PNG > DIBV5 > DIB；文本 ≤ `max_text_bytes`、图片 ≤ `max_image_pixels`、
  文件 ≤ `max_files_per_clip`；DIB 解析必须有单元测试（24/32bpp、top-down、BITFIELDS、V5）。
- **clipboard 域接口（clipboard-dev 实现，sys 调用）**，`src/clipboard.rs` 导出：

```rust
pub const SELF_MARKER_FORMAT: &str = "ClipNest.SelfWrite";

/// 原始剪贴板内容（读取阶段，未归一化）。
pub struct RawClipboard {
    pub text: Option<String>,
    /// 已是 PNG 编码的图片（CF_PNG 注册格式优先，其次由 DIB 转码；二者只保留一个）。
    pub png: Option<Vec<u8>>,
    /// 原始 CF_DIB/CF_DIBV5 字节（仅在无 PNG 时保留，供转码兜底/日志）。
    pub dib: Option<Vec<u8>>,
    pub files: Vec<std::path::PathBuf>,
    /// 含 Clipboard Viewer Ignore 或本程序自写标记 → 不应入库。
    pub ignore: bool,
}

pub struct CaptureLimits {
    pub max_text_bytes: u64,
    pub max_image_pixels: u32,
    pub max_files: usize,
}
impl CaptureLimits {
    pub fn from_settings(settings: &crate::model::Settings) -> Self; // 字段缺失时用默认
    pub fn defaults() -> Self;
}

#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    #[error("剪贴板被占用")] Busy,
    #[error("Win32 错误: {0}")] Win32(String),
    #[error("图片处理失败: {0}")] Image(String),
    #[error("IO 错误: {0}")] Io(#[from] std::io::Error),
}
pub type ClipResult<T> = Result<T, ClipError>;

/// 读取系统剪贴板（内部 5×20ms 重试 OpenClipboard；只在 Sys 线程调用）。
pub fn read_clipboard() -> ClipResult<RawClipboard>;

/// 归一化为 ClipInput（优先级 HDROP > 非空文本 > 图片；忽略空白文本/超限内容；
/// 内部计算 SHA-256 hash 并填 preview/byte_len/width/height）。
pub fn normalize(raw: RawClipboard, limits: &CaptureLimits) -> Option<ClipInput>;

/// 把 Store 提供的粘贴载荷写回系统剪贴板（文本/图片 DIB+PNG 注册格式/文件 CF_HDROP），
/// 并写入 SELF_MARKER_FORMAT 标记避免自身的 WM_CLIPBOARDUPDATE 被再次采集。
pub fn write_clipboard(payload: &crate::model::PastePayload, data_dir: &std::path::Path) -> ClipResult<()>;
```

> Sys 侧：采集到 `RawClipboard.ignore == true` 时直接丢弃；否则 `normalize` 后填
> `source_app/source_title/captured_at` 再发 `Event::Captured`。图片 PNG 转码与 DIB 解析、
> CF_HDROP 构造、文本全局内存读写全部封装在 clipboard 域，sys 不直接碰这些格式。
> DIB 解析必须有单元测试（24/32bpp、top-down、BITFIELDS、V5、损坏输入不 panic），
> `write_clipboard` 的 `build_dib`/`build_hdrop` 纯函数也要测试。
- 图像内存安全：`GlobalLock` 后必须 `GlobalUnlock`；`OpenClipboard` 失败 5×20ms 重试；
  所有错误路径不得泄漏句柄、不得 panic。

### 3.3 Sys 辅助工具（sys 提供，Lead 的 app.rs 调用）

```rust
/// 当前前台窗口所属进程名（小写、含 .exe，用于忽略名单/来源记录）。
pub fn foreground_app_name() -> Option<String>;
/// 当前进程是否提权（诊断用）。
pub fn is_elevated() -> bool;
```

> 窗口/亚克力/定位/HWND 相关工具由 **Lead 在 `src/window.rs` 实现**（Lead 写域），
> sys 域不要重复实现；P0 spike 中验证亚克力/托盘/热键的能力结论写进 `docs/P0-FINDINGS.md` 即可。

## 4. UI 域（`src/ui/**`，owner: ui teammate）

### 4.1 对外 API（Lead 在 app.rs 中接线）

```rust
pub enum WindowMode { Picker, Manager }
pub enum ManagerPage { History, Favorites, Settings, About }

pub enum UiAction {
    Search { text: String, filter: ClipFilter, offset: usize, gen: u64 }, // 查询/翻页
    Paste { id: ClipId, plain: bool },        // plain=true → 只复制不自动粘贴（Ctrl+Enter）
    RequestPastePayload(ClipId),              // 双击/Enter 前先要载荷（由 Lead 发 StoreCmd）
    Pin { id: ClipId, pinned: bool },
    Delete { ids: Vec<ClipId> },
    Clear { delete_blobs: bool },
    SaveFavoriteFiles(ClipId),
    OpenManager,
    OpenPicker,
    HideWindow,
    Quit,
    SetSettings(Box<Settings>),               // 设置页保存
    SetAutostart(bool),
    SetPause(bool),
    ExportJson,
    OpenDataDir,
    RefreshStats,
    SelectManagerPage(ManagerPage),
}

pub struct UiState { /* private */ }
impl UiState {
    pub fn new(ctx: &egui::Context, settings: &Settings) -> Self;
    /// 应用 Store 返回的搜索页（gen 过期由 UI 丢弃）。
    pub fn apply_search_page(&mut self, page: SearchPage);
    pub fn apply_stats(&mut self, stats: Stats);
    pub fn apply_winv(&mut self, status: WinVStatus);
    pub fn set_settings(&mut self, settings: &Settings);
    pub fn toast(&mut self, msg: impl Into<String>);
    pub fn mode(&self) -> WindowMode;
    pub fn enter_manager(&mut self, page: ManagerPage);
    /// 每帧渲染；返回本帧产生的动作（Lead 负责执行并清空）。
    pub fn show(&mut self, ctx: &egui::Context, thumbs: &mut ThumbCache) -> Vec<UiAction>;
    /// 当前是否在文本输入/录制快捷键（Lead 用于决定失焦隐藏）。
    pub fn is_capturing_input(&self) -> bool;
}
pub struct ThumbCache { /* private */ }
impl ThumbCache {
    pub fn new(ctx: &egui::Context) -> Self;   // ctx 用于创建纹理
    pub fn get(&mut self, id: &str) -> Option<egui::TextureHandle>;
    pub fn clear(&mut self);
}
pub mod preview { pub fn show(ctx: &egui::Context, mode: &str); } // --ui-preview 假数据
pub fn install_theme(ctx: &egui::Context);  // 色板/圆角/字号（spec §7），app.rs 启动时调用
pub fn install_fonts(ctx: &egui::Context);  // 中文字体回退
```

### 4.2 视觉与交互硬约束

- 严格用 spec §7 色板与组件规格；只做深色主题。
- Picker：620×560 内容区（窗口尺寸由 Lead 控制）；搜索框 40px、过滤胶囊、行高 64px、
  虚拟化列表（`ScrollArea::show_rows`）、底部提示条、toast、缩略图 40×40 圆角 8。
- Manager：左侧导航 184px + 内容区，自定义标题栏（Lead 提供 `ViewportCommand::StartDrag` 回调钩子）；
  页面：历史/收藏/设置/关于；历史页详情面板。
- 快捷键路由（spec §6.1）：↑↓/PgUp/PgDn/Home/End、Enter、Ctrl+Enter、Ctrl+1…9、Tab/Shift+Tab、
  Ctrl+P、Delete、Ctrl+,、Esc；鼠标单击选中/双击粘贴/右键菜单。
- 所有文本中文；不使用 emoji 当图标（可用简单几何符号/自绘）；字体缺失不得 panic。
- 缩略图：后台线程解码 `blobs/thumbs/<id>.jpg` 或原图 → 生成 `egui::ColorImage`；
  LRU 上限 64 张/16MB；`apply_search_page` 后不阻塞 UI。

## 5. App 集成（`src/app.rs`，Lead）

- 启动：单实例检查（Sys 提供）→ `logging::init` → `load_settings` → `config::ensure_dirs` →
  `install_fonts/install_theme` → `StoreHandle::spawn` → `SysHandle::spawn` → 建托盘 →
  根窗口隐藏（`ViewportBuilder::with_visible(false)`）。
- 事件泵（每帧 `try_recv`）：`PickerRequested` → 记住 `target_hwnd`、切换 Picker 模式、
  `Visible(true)`+`Focus`+按光标/屏幕定位；`PasteResult` → toast；`SearchPage` → `UiState`；
  `Captured` → 转发 `StoreCmd::Upsert`；`ManagerRequested` → Picker 模式切 Manager。
- 动作泵：`UiAction` → Store/Sys 命令映射；`Paste` 先 `HideWindow` 再发 `SysCmd::Paste`。
- 模式切换：`ViewportCommand::{Decorations,InnerSize,OuterPosition,WindowLevel,Title}`。
- 亚克力：首帧取 `Frame` 的 HWND → `sys::window::apply_acrylic`；失败记录日志并让 UI 画
  不透明深色底（禁止黑屏/透明穿透）。
- 退出：托盘退出/`QuitRequested` → `SysCmd::Shutdown` + `StoreCmd::Shutdown` → `ViewportCommand::Close`。

## 6. 团队与协作

| 成员 | 写入域 | 任务板 |
|---|---|---|
| lead | `Cargo.toml` `src/model.rs` `src/main.rs` `src/app.rs` `src/config.rs` `src/logging.rs` `src/diag.rs` `docs/**` `tools/**` `assets/**` | 集成/验收 |
| store | `src/store/**` | 任务 1 |
| win32 | `src/clipboard/**` `src/sys/**` `examples/**` | 任务 2 |
| ui | `src/ui/**` | 任务 3 |

- 有阻塞/接口冲突 → `send_message` 给 lead；不要空转等待。
- 每完成一个里程碑（可编译 + 测试通过）发消息：`MILESTONE <域> <一句话>` + 文件清单 + 剩余项。
- Lead 负责最终 `cargo test`/`cargo build --release`/截图/验收；teammate 不跑 git、不动 release。
- P0 优先级：win32 先做 Win+V/亚克力/托盘的能力 spike（`examples/p0_spike.rs`）并发结论；
  ui 先做 theme+widgets+picker 静态预览；store 先做 schema+CRUD 测试。

## 7. v1.1 进程模型（守护进程 + 按需 UI 子进程）

> 2026-10-31 晚修复轮新增。目的：把 eframe/OpenGL/CJK 字体等 GUI 依赖从常驻进程中剥离，
> 让空闲内存接近原生程序（本机实测专用工作集 7.6MB / Private 10.6MB）。

- `clipnest.exe`（默认，无参数）= **守护进程**：单实例互斥体；Sys 线程（托盘/热键/Win+V 钩子/剪贴板监听/
  自动粘贴）+ Store 线程（SQLite 采集/搜索/容量治理）；主线程事件循环，**不创建 GUI 窗口、不初始化 eframe**。
- 收到 `Event::PickerRequested{target_hwnd}` / `ManagerRequested` / `SettingsRequested` 时：
  `std::process::Command` spawn 自身 exe：`--ui-picker --target <hwnd>` 或 `--ui-manager --page <page>`，
  并按类型只保留一个 UI 子进程；同类型重复请求只聚焦已有窗口。
- UI 子进程（`src/app.rs`）：创建单窗口 eframe（候选窗 620×560 置顶/管理台 1060×700），
  自己打开同一个 SQLite（WAL 多进程安全）；粘贴时直接调用 `clipboard::write_clipboard` +
  `sys::paste::restore_and_paste`；关闭/粘贴完成后以原生 `WM_CLOSE` 退出进程。
- 跨进程设置同步：UI 保存 `settings.json`；守护进程每 250ms 检查 mtime，变化即重载并
  `SysCmd::SetHotkeys`/`SetPause` 应用（≤1s 生效）。托盘"暂停/自启"由 Sys 直接写设置文件。
- 退出：托盘"退出"→ `Event::QuitRequested` → 守护进程 kill 子进程、drop Sys/Store；UI 里的"退出"
  通过 `WM_COPYDATA("CLNQ")` 请求守护进程退出。
- 开发调试：`--ui-picker/--ui-manager` 可手工启动 UI 子进程；`CLIPNEST_SKIP_FONTS`/`CLIPNEST_SKIP_MICA`
  仅用于 UI 内存对比实验。

## 8. v1.2 进程模型修订（2026-11-02）

> 用户反馈"打开界面慢"，要求回到 v1.0 方式、内存暂不考虑。**§7 的守护进程 + UI 子进程设计已撤销**，
> 实现回到单进程：eframe 窗口启动即创建（屏幕外、不抢前台），Store/Sys 线程常驻；
> 快捷键/托盘唤起只做窗口移入屏幕 + 强制前台（实测 20–80ms）。
> Win+V 接管同步移除（不注册、不装钩子）。设置改动由 UI 直接下发 SysCmd 立即生效，
> settings.json 监听作为兜底。以本节为准，§7 仅保留为历史。

## 9. v1.4.0 补充（2026-11-02）

- 新增 `Settings.theme`（dark/light）与设置页「外观」分区；`theme` 模块改为运行时动态色板，
  `set_light(bool)` + `install_theme(ctx)` 可热切换；窗口 Acrylic 色调随主题重设。
- 候选窗顶部 16px 拖拽条、管理台标题栏支持 `ViewportCommand::StartDrag` 拖动窗口。
- exe 图标改为构建后由 `patch-icon` 写入资源（不依赖 rc.exe）；发布流程见 `tools/build-release.ps1`。
- Win+V 模块/类型/事件/字段已彻底删除（`src/sys/winv.rs` 移除），当前无编译警告。

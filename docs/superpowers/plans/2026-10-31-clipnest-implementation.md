# ClipNest 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 Rust + eframe/egui 从零实现 Windows 深色磨砂玻璃剪贴板管理器 ClipNest：托盘常驻、Win+V 呼出、文本/图片/文件持久化、可配置快捷键、自动粘贴。

**Architecture:** 单进程四线程（UI/egui 主线程、Sys Win32 消息线程、Store SQLite 线程、Thumb 缩略图线程），单根窗口在 Picker/Manager 两模式间切换；模块间只通过 `model.rs` 定义的 Event/Command 通道通信，实现顺序契约先行、文件级写入域隔离、各模块独立测试。

**Tech Stack:** Rust 2021/MSVC、eframe=0.32.3(glow)、rusqlite(bundled, WAL+FTS5 trigram)、image、sha2、windows-sys、tray-icon、window-vibrancy、serde/serde_json、chrono、thiserror。

**Spec:** `docs/superpowers/specs/2026-10-31-clipnest-design.md`（实现中所有行为以 spec 为准，本计划只细化任务与接口）

## Global Constraints

- 只支持 Windows 10 1809+ / Windows 11，x86_64-pc-windows-msvc；纯 Rust，无 WebView，无网络调用。
- 数据目录固定 `%APPDATA%\ClipNest\`；不写 HKLM；不申请管理员权限。
- 全中文 UI；只做深色磨砂玻璃主题（见 spec §7 色板，不可自创配色）。
- 默认值必须与 spec §20 附录 A 完全一致（5000 条 / 200MB 图片 / 200MB 收藏文件 / 2MB 文本 / 50MB 单文件 / 620×560 面板）。
- 所有跨线程通信必须走 `mpsc` 通道（Event/Command enum），禁止共享可变全局状态；唯一共享为 `Arc<Mutex<Settings>>`。
- 每个模块文件建议 ≤800 行；公开 API 变更必须先改 `docs/CONTRACT.md`。
- 每个任务必须：先写测试（能跑失败）→ 实现 → `cargo test`/`cargo build` 通过 → `git commit`。
- 日志不得记录剪贴板内容全文（只记录类型/大小/来源/哈希前 8 位）。
- 依赖版本以 P0 锁定后的 `Cargo.toml` 为准，不得随意升级/新增（新增需 Lead 同意）。

---

## 文件结构（写入域）

| 域 | 负责人 | 文件 |
|---|---|---|
| Lead | lead | `Cargo.toml` `build.rs` `src/main.rs` `src/app.rs` `src/model.rs` `src/config.rs` `src/logging.rs` `src/diag.rs` `src/win_single.rs` `docs/**` `tools/**` `assets/**` |
| A store | store | `src/store/**` |
| B win32 | win32 | `src/clipboard/**` `src/sys/**` |
| C ui | ui | `src/ui/**` |

契约文件 `docs/CONTRACT.md` 由 Lead 维护；模块内新增私有文件自由。

---

## Task 1: P0 工程骨架 + 依赖锁定

**Files:** Create `Cargo.toml`、`src/main.rs`、`src/logging.rs`、`src/config.rs`、`tools/screenshot.ps1`、`assets/icon.png`、`examples/p0_spike.rs`、`.gitignore`

**Interfaces:**
- `logging::init(data_dir: &Path)`；`logging::log(msg: &str)`；`logging::log_err(...)`
- `config::data_dir() -> PathBuf`（`%APPDATA%\ClipNest`，`CLIPNEST_DATA_DIR` 环境变量可覆盖，供测试）
- `config::settings_path() / db_path() / logs_dir() / blobs_dir() / thumbs_dir() / restored_dir()`
- `config::load_settings() -> Settings`；`config::save_settings(&Settings) -> Result<()>`（原子写：tmp+rename）
- `Settings::default()` 与 spec §20 附录 A 一致

- [ ] Step 1: `cargo build` 通过（依赖已由 Lead 在 P0 预装并锁定版本；`rusqlite` bundled 编译成功是关键验证点）
- [ ] Step 2: 写 `examples/p0_spike.rs`：打开 620×560 透明无边框 always-on-top 窗口（eframe），注册托盘图标、`RegisterHotKey(Ctrl+`)`、安装 Win+V 低层键盘钩子（吞键），日志输出每种能力成功/失败；用 `window-vibrancy::apply_acrylic` 尝试亚克力
- [ ] Step 3: 运行 `cargo run --example p0_spike 2>&1 | Tee-Object p0.log`，人工/截图验证：窗口圆角透明+亚克力、托盘出现、日志打印 RegisterHotKey 结果与 Win+V 钩子触发行
- [ ] Step 4: 用 `tools/screenshot.ps1` 截图 `target/p0-acrylic.png`；PowerShell 发 Win+V 后截图 `target/p0-winv.png` 检查开始菜单是否误弹；记录结论到 `docs/P0-FINDINGS.md`（含 L1/L2 决策）
- [ ] Step 5: `git add -A && git commit -m "chore: P0 skeleton, deps lock, winv/acrylic spikes"`

**验收：** 依赖可编译；亚克力有截图证据；Win+V 结论明确（L1 成功 / L2 可用 / 需补偿 / 不可用）。

---

## Task 2: 共享契约与模型

**Files:** Create `src/model.rs`、`docs/CONTRACT.md`；Modify `src/main.rs`

**Interfaces（生产方必须与此一致）:**

```rust
pub type ClipId = i64;
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[repr(i32)] pub enum ClipKind { Text = 0, Image = 1, Files = 2 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileEntry { pub name: String, pub path: String, pub size: u64,
                       pub exists: bool, pub stored: bool }

#[derive(Clone, Debug)]
pub struct ClipItem { pub id: ClipId, pub kind: ClipKind, pub preview: String,
    pub text: Option<String>, pub files: Vec<FileEntry>, pub blob_id: Option<String>,
    pub byte_len: u64, pub width: Option<u32>, pub height: Option<u32>,
    pub created_at: i64, pub last_used_at: i64, pub use_count: u32,
    pub pinned: bool, pub source_app: Option<String>, pub source_title: Option<String> }

/// Sys 线程采集 → Store 写入的归一化输入
#[derive(Clone, Debug)]
pub struct ClipInput { pub kind: ClipKind, pub hash: String,
    pub preview: String, pub text: Option<String>, pub image_png: Option<Vec<u8>>,
    pub files: Vec<FileEntry>, pub byte_len: u64,
    pub width: Option<u32>, pub height: Option<u32>,
    pub source_app: Option<String>, pub source_title: Option<String>,
    pub captured_at: i64 }

/// 回写剪贴板需要的载荷（Store 提供 blob 路径）
#[derive(Clone, Debug)]
pub struct PastePayload { pub kind: ClipKind, pub text: Option<String>,
    pub png_path: Option<PathBuf>, pub files: Vec<FileEntry>, pub clip_id: ClipId }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ClipFilter { #[default] All, Text, Image, Files, Pinned }

#[derive(Clone, Debug)]
pub struct SearchQuery { pub text: String, pub filter: ClipFilter,
                         pub offset: usize, pub limit: usize, pub gen: u64 }

#[derive(Clone, Debug)]
pub struct SearchPage { pub gen: u64, pub total: usize, pub items: Vec<ClipItem> }

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HotkeyChord { pub ctrl: bool, pub alt: bool, pub shift: bool,
                         pub win: bool, pub vk: u32 }

pub enum Event {
    PickerRequested,
    ClipsChanged { total: usize },
    SearchPage(SearchPage),
    Stats(Stats),
    PasteResult { ok: bool, reason: Option<String> },
    Toast(String),
    WinVStatus(WinVStatus),
    HotkeyStatus { ok: bool, reason: Option<String> },
    ManagerRequested,
    QuitRequested,
}

pub enum StoreCmd {
    Upsert(ClipInput),
    Search(SearchQuery),
    SetPinned { id: ClipId, pinned: bool },
    Delete(Vec<ClipId>),
    Clear { delete_blobs: bool },
    GetPastePayload(ClipId),
    SaveFavoriteFiles(ClipId),
    RefreshStats,
    Shutdown,
}

pub enum SysCmd {
    Paste { payload: PastePayload, target: isize },   // target = 呼出时前台 HWND
    WriteClipboard(PastePayload),
    SetHotkeys { picker: HotkeyChord, winv_mode: WinVMode },
    SetPause(bool),
    ShowManagerHint,
    Shutdown,
}
```

- [ ] Step 1: 先写 `src/model.rs` 单元测试：`ClipKind` serde 往返、`hotkey_label()`、`Settings::default()` 断言（5000/200/200/2MB/50MB/620×560）
- [ ] Step 2: 实现 `model.rs`（含 `Settings` 与全部默认值、`Stats`、`WinVMode/WinVStatus`）
- [ ] Step 3: 写 `docs/CONTRACT.md`：每模块公开 API 签名 + 事件/命令字段 + 生命周期 + 错误类型
- [ ] Step 4: `cargo test` 通过；`git commit -m "feat: shared model and module contract"`

**验收：** `cargo test` 绿；CONTRACT.md 覆盖 store/clipboard/sys/ui 四域。

---

## Task 3: Store —— schema/迁移/连接

**Files:** Create `src/store/mod.rs`、`src/store/db.rs`、`src/store/blob.rs`、`src/store/export.rs`、`src/store/tests.rs`

**Interfaces:**
- `pub struct StoreHandle`，`StoreHandle::spawn(events: Sender<Event>, settings: Arc<Mutex<Settings>>, ctx: egui::Context) -> Result<StoreHandle, StoreError>`
- `pub fn command_tx(&self) -> Sender<StoreCmd>`
- 内部 `Db { conn: rusqlite::Connection }`：`open(path) -> Result<Db>`、`migrate(&mut self)`、`schema_version()`

- [ ] Step 1: 测试：临时目录打开库 → 迁移 → 断言 `clips/blobs/meta/fts` 表存在；再次 `migrate` 幂等；`PRAGMA journal_mode` = wal
- [ ] Step 2: 实现 schema v1（spec §10.2 原文，含 FTS 触发器、索引）；FTS5 创建失败时 catch → `meta.fts_enabled=0` 并继续
- [ ] Step 3: 测试：`fts_enabled` 两种路径（可强制 `CLIPNEST_DISABLE_FTS=1` 模拟）；`cargo test` 绿；commit

---

## Task 4: Store —— 写入/去重/查询/分页

**Files:** Modify `src/store/db.rs`、`src/store/tests.rs`

**Interfaces:**
- `pub fn upsert(&mut self, input: ClipInput) -> Result<UpsertOutcome>`；`UpsertOutcome::{Inserted(ClipId), Touched(ClipId)}`
- `pub fn search(&self, q: &SearchQuery) -> Result<SearchPage>`
- `pub fn item(&self, id: ClipId) -> Result<Option<ClipItem>>`
- `pub fn set_pinned(&mut self, id, pinned) -> Result<()>`、`delete(&mut self, ids: &[ClipId]) -> Result<()>`
- `pub fn counts(&self) -> Result<Stats>`、`pub fn paste_payload(&self, id) -> Result<Option<PastePayload>>`

- [ ] Step 1: 测试（spec §17.1 对应项）：重复 hash → `Touched` 且 use_count+1、last_used_at 更新、不新增行；不同 kind 同 hash 也去重（hash 归一化含 kind）
- [ ] Step 2: 实现 upsert（事务 + FTS 触发器自动同步）
- [ ] Step 3: 测试：分页（offset/limit）、过滤（全部/文本/图片/文件/收藏）、排序（pinned 优先、last_used_at 降序）；搜索中文子串 ≥3 字符走 FTS、<3 字符走 LIKE、特殊字符 `%_'"` 不报错
- [ ] Step 4: 实现 search；`cargo test` 绿；commit

---

## Task 5: Store —— blob/retention/文件收藏/导出

**Files:** Create/modify `src/store/blob.rs`、`src/store/export.rs`、`src/store/mod.rs`、`src/store/tests.rs`

**Interfaces:**
- `blob::put_image(conn, png: &[u8]) -> Result<String /*blob_id*/>`（内容寻址、refs=1）
- `blob::image_path(data_dir, blob_id) -> PathBuf`；`blob::put_favorite_files(data_dir, id, &[FileEntry]) -> Result<Vec<FileEntry>>`
- `blob::restore_favorite_files(data_dir, id) -> Result<Vec<PathBuf>>`
- `retention::prune(conn, settings) -> Result<PruneReport>`
- `export::export_json(data_dir, conn) -> Result<String>`；`import_json(data_dir, conn, &str) -> Result<ImportReport>`

- [ ] Step 1: 测试：图片 blob 写入/读取字节一致；相同图片只落盘一次（hash 目录）；refs 计数正确
- [ ] Step 2: 测试 retention：插入 10 条/limit=5 → 删最旧 5 条且收藏保留；孤儿 blob 文件被删；图片超限按最旧删（只删非收藏条目引用的）
- [ ] Step 3: 实现 retention（启动 + 每 100 次写 + 设置变更触发；不删已有收藏文件实体，超限停止新增）
- [ ] Step 4: 测试文件收藏：临时文件 → `SaveFavoriteFiles` → `files[].stored=true` 且副本存在；删除源文件后 `restore_favorite_files` 得到新路径且内容一致
- [ ] Step 5: 测试导出/导入往返（文本+收藏）；`cargo test` 绿；commit

---

## Task 6: Store —— 线程入口与事件

**Files:** Modify `src/store/mod.rs`

- [ ] Step 1: 测试：spawn 后 `Upsert` 收到 `Event::ClipsChanged`；`Search` 返回带 gen 的 `SearchPage`（过期 gen 仍返回但 UI 丢弃）；`Shutdown` 后线程退出
- [ ] Step 2: 实现 StoreHandle 线程循环：单连接、批量事务（同一帧多条合并）、`ctx.request_repaint()`、panic 捕获写日志
- [ ] Step 3: 测试搜索延迟 <100ms（5000 条造数，CI 宽松断言 <300ms）；commit

---

## Task 7: Clipboard —— 读取与归一化

**Files:** Create `src/clipboard/mod.rs`、`reader.rs`、`writer.rs`、`dib.rs`

**Interfaces:**
- `pub struct RawClipboard { pub text: Option<String>, pub png: Option<Vec<u8>>, pub dib: Option<Vec<u8>>, pub files: Vec<PathBuf>, pub ignore: bool }`
- `pub fn read_clipboard() -> Result<RawClipboard, ClipError>`（重试 5×20ms）
- `pub fn normalize(raw: RawClipboard, cfg: &CaptureLimits) -> Option<ClipInput>`
- `pub fn hash_input(input: &ClipInput) -> String`

- [ ] Step 1: 测试 `dib.rs`：构造 24bpp bottom-up / 32bpp top-down / BI_BITFIELDS 的 `Vec<u8>` DIB，解析出 RGBA 尺寸与像素；PNG 编码后再解码像素一致；损坏输入返回 Err 不 panic
- [ ] Step 2: 实现 DIB 解析（spec §10.3，含 8/16/24/32bpp、top-down、BITFIELDS、V5）
- [ ] Step 3: 测试 `normalize` 优先级：HDROP > 文本 > PNG > DIB；纯空白文本忽略；图像超 40MP 忽略；文本超 2MB 截断；`Clipboard Viewer Ignore` → `ignore=true`
- [ ] Step 4: 实现 `read_clipboard`（CF_HDROP/CF_UNICODETEXT/CF_DIBV5/CF_DIB/PNG 注册格式、GlobalLock、重复打开重试）
- [ ] Step 5: `cargo test` 绿；commit

**验收：** 单元测试不依赖真实剪贴板；真实剪贴板冒烟在 Task 10 做。

---

## Task 8: Clipboard —— 回写

**Files:** Modify `src/clipboard/writer.rs`

**Interfaces:** `pub fn write_clipboard(payload: &PastePayload, data_dir: &Path) -> Result<(), ClipError>`

- [ ] Step 1: 测试：纯函数 `build_dib(png_path) -> Vec<u8>`（32bpp BI_RGB top-down）可被 `dib.rs` 反向解析；`build_hdrop(&[PathBuf]) -> Vec<u8>` 字节布局正确（DROPFILES 20 字节 + 宽字符串双 \0）
- [ ] Step 2: 实现写文本（CF_UNICODETEXT）、图片（CF_DIB + 注册格式 "PNG"）、文件（CF_HDROP）；加 `Clipboard Viewer Ignore` 防回环标记
- [ ] Step 3: 集成冒烟（手动）：`--selftest` 内置写→读回环校验；commit

---

## Task 9: Sys —— 粘贴引擎

**Files:** Create `src/sys/mod.rs`、`thread.rs`、`paste.rs`、`util.rs`；Modify `src/model.rs` 事件

**Interfaces:**
- `pub struct SysHandle`：`SysHandle::spawn(events: Sender<Event>, settings: Arc<Mutex<Settings>>, ctx: egui::Context) -> Result<Self>`；`pub fn cmd_tx(&self) -> Sender<SysCmd>`
- `paste::restore_and_paste(target: isize, paste_key: PasteKey) -> Result<(), PasteError>`

- [ ] Step 1: 测试 `util`：进程名提取（路径→exe 名小写）、`is_elevated()` 不 panic
- [ ] Step 2: 实现前台恢复（`SetForegroundWindow` + `AttachThreadInput` 重试）与 `SendInput`（Ctrl+V / Shift+Insert），返回可区分错误 `TargetInvalid/UipiBlocked/SendInputFailed`
- [ ] Step 3: 实现 Sys 线程骨架：消息窗口类 `ClipNestHidden`、`Event` 泵、`Shutdown`；`WM_COPYDATA` 单实例唤起（Task 12 完整）
- [ ] Step 4: commit

---

## Task 10: Sys —— 剪贴板监听与事件

**Files:** Modify `src/sys/thread.rs`

- [ ] Step 1: 实现 `AddClipboardFormatListener` + `WM_CLIPBOARDUPDATE` → `clipboard::read_clipboard` → `normalize` → 计算 hash → 发 `StoreCmd::Upsert`（Sys 线程不能直接持有 Store 命令发送端？通过 `Event::Captured(ClipInput)` 由 UI 线程转发，或 spawn 时注入 `Sender<StoreCmd>`；**决定：spawn 参数增加 `store_tx: Sender<StoreCmd>`，Sys 直接发 Store**，见 CONTRACT）
- [ ] Step 2: 忽略规则：`Clipboard Viewer Ignore`、设置忽略名单（进程名）、暂停状态 → 跳过并日志
- [ ] Step 3: 测试（可注入）：`should_capture(&Settings, app_name, raw_flags) -> bool` 纯函数测试
- [ ] Step 4: 手动冒烟：复制文本/截图/文件各一次，日志出现 hash 与 kind；commit

---

## Task 11: Sys —— 热键与 Win+V

**Files:** `src/sys/hotkey.rs`、`src/sys/winv.rs`、Modify `src/sys/thread.rs`、`src/sys/mod.rs`

**Interfaces:**
- `hotkey::parse_chord("Ctrl+Shift+V") -> Result<HotkeyChord>`；`hotkey::label(&HotkeyChord) -> String`
- `WinVMode { Auto, RegisterOnly, HookOnly, Off }`；`WinVStatus { Registered, HookActive{compensated: bool}, Unavailable{reason: String} }`
- `winv::install_hook(hwnd, tx) -> Result<HookState>`；`winv::uninstall(&mut HookState)`

- [ ] Step 1: 测试 `parse_chord` 往返：`Ctrl+Shift+V`、`Win+V`、`Alt+``、非法组合报错、重复修饰键去重
- [ ] Step 2: 实现 RegisterHotKey（全局备用快捷键）注册/注销/冲突诊断（`GetLastError=1409` → 已占用）
- [ ] Step 3: 实现 Win+V：先 L1 尝试 `RegisterHotKey(MOD_WIN, V)`；失败时 L2 安装 WH_KEYBOARD_LL（Win 状态跟踪、吞 V down/up、忽略 INJECTED、按 P0 结论决定是否补偿 Start 菜单）；状态发 `Event::WinVStatus`
- [ ] Step 4: 实测脚本（PowerShell SendInput Win+V）验证面板打开且开始菜单不弹；日志/截图证据；commit

---

## Task 12: Sys —— 托盘/自启/单实例/生命周期

**Files:** Create `src/sys/tray.rs`、`src/sys/autostart.rs`、`src/win_single.rs`；Modify `src/sys/thread.rs`、`src/main.rs`

- [ ] Step 1: 托盘菜单（打开面板/管理历史/暂停记录/清空历史/设置/开机自启勾选/退出），事件转 `Event`；用 `assets/icon.png` 生成图标
- [ ] Step 2: `autostart::set(enabled) / get()` 读写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 值 `ClipNest`；测试（可注入注册表根/或读回再还原）
- [ ] Step 3: 单实例：命名 Mutex `Global\ClipNestSingleInstance`；第二实例 `FindWindowW("ClipNestHidden")` + `WM_COPYDATA` 通知首个实例 → 退出；`--selftest` 不受单实例限制
- [ ] Step 4: 退出流程：`Shutdown` 命令 → 停止监听/卸钩子/注销热键/保存设置/join 线程；commit

---

## Task 13: UI —— 主题/字体/控件

**Files:** Create `src/ui/mod.rs`、`theme.rs`、`fonts.rs`、`widgets.rs`、`thumbs.rs`

**Interfaces:**
- `theme::install(ctx, mode: UiTheme)`（spec §7 色板 → egui Visuals/Style；圆角 12/10/9/8）
- `fonts::install_cjk(ctx)`（复用已有回退模式：Deng → simhei → msyh → simsun）
- `widgets::{search_box, filter_chip, clip_row, toast, hint_bar, switch, hotkey_recorder, confirm_dialog}`
- `thumbs::ThumbCache::get(&mut self, ctx, blob_path) -> Option<egui::TextureHandle>`（后台加载 + LRU 64/16MB）

- [ ] Step 1: `ui/mod.rs` 定义 `UiState`（mode、search text、selected、filter、page、toast、settings drafts）与 `UiAction` 枚举（Paste/PlainCopy/Pin/Delete/OpenManager/OpenSettings/Close…）
- [ ] Step 2: 实现 theme/fonts（颜色常量集中定义，可在测试中断言关键色值）
- [ ] Step 3: 实现静态预览页 `cargo run -- --ui-preview picker`（喂假数据），截图对照 spec §7；commit

---

## Task 14: UI —— Picker

**Files:** `src/ui/picker.rs`、Modify `src/ui/mod.rs`

- [ ] Step 1: 实现布局：搜索栏（40px）+ 过滤胶囊 + 虚拟化列表（`ScrollArea::show_rows`，行 64px）+ 底部提示条 + Toast
- [ ] Step 2: 键盘：↑↓/PgUp/PgDn/Home/End/Enter/Ctrl+Enter/Ctrl+1…9/Tab/Ctrl+P/Delete/Esc；鼠标：单击选中/双击粘贴/右键菜单/滚轮
- [ ] Step 3: 空状态、搜索无结果、图片缩略图懒加载、tooltip 截断详情
- [ ] Step 4: `--ui-preview picker --demo` 截图 `target/ui-picker.png`；commit

---

## Task 15: UI —— Manager

**Files:** `src/ui/manager.rs`、`src/ui/pages/history.rs`、`favorites.rs`、`settings.rs`、`about.rs`、Modify `src/ui/mod.rs`

- [ ] Step 1: 外壳：自定义标题栏（拖拽 `ViewportCommand::StartDrag`、关闭/隐藏）、左侧导航 184px、内容路由、边缘拖拽缩放（`BeginResize`）
- [ ] Step 2: 历史页：表格（类型/摘要/来源/时间/大小/收藏）、搜索过滤、详情面板（文本全文/图片大图/文件清单）、多选批量操作
- [ ] Step 3: 收藏页 + 设置页（快捷键录制、Win+V 开关+状态、容量、行为、忽略名单、自启、数据目录/导出/清空）+ 关于页
- [ ] Step 4: `--ui-preview manager --demo` 截图 4 张；commit

---

## Task 16: App 集成

**Files:** `src/app.rs`、Modify `src/main.rs`、`src/diag.rs`

- [ ] Step 1: 启动流程：单实例 → 日志 → 设置 → 字体/主题 → Store/Sys spawn → 托盘 → 窗口隐藏启动（`ViewportBuilder.with_visible(false)`）
- [ ] Step 2: 事件泵（每帧 `try_recv`）：PickerRequested → 记录 target hwnd、切 Picker 模式、`Visible(true)`+`Focus`+定位；Paste → 隐藏窗口 → `SysCmd::Paste`；Toast/Stats/SearchPage → UI
- [ ] Step 3: 模式切换（Decorations/WindowLevel/InnerSize/OuterPosition）、失焦自动隐藏、Esc 隐藏、托盘退出、设置持久化
- [ ] Step 4: 冒烟：真实复制→Win+V/备用热键→选择→记事本自动粘贴；commit

---

## Task 17: 诊断/自检/演示数据

**Files:** `src/diag.rs`、Modify `src/main.rs`

- [ ] Step 1: `--diagnose out.json`：OS 版本、FTS5、亚克力、托盘、热键、Win+V 探测状态、数据目录、统计
- [ ] Step 2: `--selftest out.json`：隔离临时目录跑 store 全链路 + 剪贴板写读回环 + 退出码；`cargo test` 之外的一键验收
- [ ] Step 3: `--demo-seed N --yes`：写入演示数据（文本/中文/代码/图片/文件条目），支持 `--ui-preview` 截图；commit

---

## Task 18: 性能/内存/稳定性验收

**Files:** `docs/ACCEPTANCE.md`、`tools/measure.ps1`

- [ ] Step 1: 造 5000 条 + 300 张图片（200MB）压测库；`cargo build --release`
- [ ] Step 2: 测启动/呼出/搜索/滚动 FPS；`Get-Process` 采样 10 次内存（隐藏态、面板态、管理态）
- [ ] Step 3: 达标（spec §15）或记录差距并修；`tools/measure.ps1` 一键复测；commit
- [ ] Step 4: 24h 常驻不崩溃（可缩短为 2h 观察内存曲线，记录）

---

## Task 19: 文档与发行

**Files:** `README.md`、`docs/ACCEPTANCE.md`、`docs/USAGE.md`、`assets/icon.ico`、`build.rs`

- [ ] Step 1: README：功能、截图 4 张、安装/绿色运行、快捷键表、Win+V 接管说明与降级、数据目录、隐私、已知限制、开发命令
- [ ] Step 2: exe 图标（embed-resource，可选；失败不影响验收）；`cargo build --release` 产出单文件
- [ ] Step 3: release 验收：干净目录拷 exe → 运行 → 全链路 → 重启系统验证持久化（或至少进程重启 + 登录后自启逻辑）
- [ ] Step 4: commit + tag `v1.0.0`

---

## Task 20: 独立复核（Verifier）

**Files:** 只读 + `docs/REVIEW.md`

- [ ] Step 1: 独立 subagent 按 spec §17.2 逐项验收，跑 `cargo test`、`--selftest`、`--diagnose`、截图对比、内存采样
- [ ] Step 2: 代码审查：线程安全、剪贴板泄漏（GlobalUnlock/CloseClipboard）、错误路径、日志隐私
- [ ] Step 3: 输出 `docs/REVIEW.md`（问题清单 + 严重度），Lead 修复后复核

---

## 执行方式

- 依赖关系：Task 1/2 阻塞全部；Task 3→4→5→6（store 串行）；Task 7→8→9→10→11→12（win32 串行）；Task 13→14→15（ui 串行）；Task 16 依赖 2/6/12/15；Task 17/18/19 依赖 16；Task 20 最后。
- 并行组：store(A) / win32(B) / ui(C) 三线并行，Lead 同时做 Task 1/2/16/17/18/19。
- 每任务完成即 `cargo test` + commit；Lead 每日集成检查 `cargo check`（cargo 锁会自动串行，可接受）。
- 每个 teammate 任务板上的任务必须标注写入域；跨域修改先找 Lead。

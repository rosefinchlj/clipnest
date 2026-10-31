# ClipNest（剪贴匣）设计文档

- 日期：2026-10-31
- 状态：设计已获用户批准（对话确认），进入实施
- 目标平台：Windows 10 1809+ / Windows 11（开发机：Windows 11 build 26300，x64）
- 项目路径：`E:\use\ai\deepseek\Rust\clipnest`

## 1. 背景与问题

用户当前使用第三方剪贴板工具（截图中的侧栏式历史面板），痛点：

1. UI 观感差、侧栏占用屏幕空间、交互不顺。
2. 内存占用约 136MB，偏高。
3. 希望 Ctrl+C 复制后自动出现在工具中，重启/关机后历史仍在，可长期保存大量内容。
4. 希望能自定义快捷键粘贴，并**用本工具接管 Win+V**（替代 Windows 自带剪贴板历史）。
5. 明确要求：不是重构/复刻现有工具，而是重新设计。

## 2. 目标与非目标

### 2.1 目标（v1 必须达成）

- 常驻托盘、低资源（空闲内存 ≤60MB，含 UI 面板进程）。
- 全局快捷键呼出悬浮候选窗，键盘优先，选中即粘贴回原窗口。
- 支持文本、图片、文件列表三类剪贴板内容，自动入库、去重、秒级可见。
- SQLite 持久化，重启/关机不丢；默认容量 5000 条 / 图片 200MB，可配置。
- 收藏置顶、搜索过滤、删除清空、暂停记录、忽略应用名单。
- Win+V 接管（三级降级策略，见 §13），可配置、可回退。
- 深色磨砂玻璃 UI，全中文，单文件绿色 exe，不联网。

### 2.2 非目标（v1 明确不做）

- 富文本 HTML/RTF 保真粘贴、Office 专有格式。
- 云同步、多设备、账号体系、OCR、AI 摘要。
- 脚本/插件系统、命令行高级 DSL。
- 亮色主题（用户明确只要深色）。
- 常驻置顶侧栏/悬浮磁贴。
- 非 Windows 平台适配（代码保持可移植分层，但 v1 只验收 Windows）。

## 3. 需求决策记录（用户确认）

| 议题 | 决策 |
|---|---|
| 交互形态 | 托盘常驻 + 全局快捷键呼出浮动候选窗；另有管理界面 |
| 内容类型 | 文本 + 图片 + 文件列表 |
| 文件记录 | 默认只存路径清单；**收藏某条时**才把文件实体复制进库（默认单文件 ≤50MB） |
| 容量 | 默认 5000 条 / 图片 200MB（可调、可改无限、收藏项不清理） |
| 视觉 | 只做深色磨砂玻璃（亚克力） |
| 快捷键 | 接管 Win+V 作为主快捷键（需求见 §13）；同时保留可配置的备用快捷键，默认 Ctrl+` |

## 4. 技术选型

| 方案 | 结论 |
|---|---|
| **Rust + eframe/egui 0.32.3 + glow** | **采用**。纯 Rust、无 WebView、单 exe；与已有同类项目技术栈一致；eframe 0.32.3 已在开发机 cargo 缓存中并验证可编译 |
| Tauri 2 + Web 前端 | 淘汰：WebView2 常驻内存 100MB+，与低内存目标冲突；构建链重 |
| 纯 Win32/D2D 原生 UI | 淘汰为首版方案：内存最低但深色亚克力/圆角/动画全部手写，工期不可控 |

关键依赖（版本在 P0 阶段锁定，均需实际编译验证）：

- `eframe = "=0.32.3"`（glow 后端，无 wgpu；features 仅 default 精简后用到的部分）
- `rusqlite`（bundled，WAL）+ FTS5（trigram tokenizer；运行时探测，不可用则 LIKE 降级）
- `image`（PNG/JPEG 编解码、缩略图）
- `sha2`（内容寻址）、`serde`/`serde_json`、`chrono`、`thiserror`
- `windows-sys`（剪贴板、热键、键盘钩子、SendInput、DWM、注册表、单实例）
- `tray-icon`（托盘图标与菜单）
- `window-vibrancy`（亚克力；失败回退自绘半透明）
- `embed-resource`（可选，exe 图标）

## 5. 产品形态与窗口模型

### 5.1 两种模式，一个原生窗口

为降低复杂度与内存，使用**单个 eframe 根窗口 + 运行时切换模式**（已确认 egui 0.32 支持
`Decorations/WindowLevel/OuterPosition/InnerSize/Focus/Transparent` 运行期命令）：

| | Picker 模式（候选窗） | Manager 模式（管理台） |
|---|---|---|
| 尺寸 | 620×560（可配置 3 档） | 1060×700（可拉伸，最小 880×560） |
| 装饰 | 无边框、自定义标题区 | 无边框 + 自定义标题栏（拖拽/关闭/最小化到托盘） |
| 层级 | 始终置顶 | 普通层级 |
| 任务栏 | 不出现 | 不出现（托盘应用约定；Alt+Tab 不干扰） |
| 背景 | 深色亚克力磨砂 | 不透明深色面板 |
| 位置 | 光标所在屏幕居中（可设置"记住上次位置"） | 上次位置/屏幕居中 |
| 呼出 | 全局快捷键 / 托盘"打开面板" | 托盘"管理历史" / 面板齿轮 / 首次运行 |

切换方式：同一窗口发 `ViewportCommand::Decorations(false/true)`、`InnerSize`、`OuterPosition`、
`WindowLevel`，egui 立即换布局；不销毁重建窗口（避免闪烁与延迟）。

Picker 模式隐藏/显示：`ViewportCommand::Visible(false/true)` + `Focus`；关闭（Esc/失焦/粘贴后）
一律隐藏而非退出。失去焦点自动隐藏为默认行为（可配置）。

### 5.2 状态机

```
启动 → 托盘常驻
  ├─ 全局快捷键 / 托盘菜单 → Picker（记录当时前台窗口句柄）
  │    ├─ Enter/点击/快选 → 写剪贴板 → 恢复前台窗口 → SendInput Ctrl+V → 隐藏
  │    ├─ Ctrl+Enter → 仅写剪贴板 → 隐藏
  │    ├─ Esc/失焦 → 隐藏（不粘贴）
  │    └─ 齿轮/设置 → Manager（Picker 隐藏）
  ├─ 托盘"管理历史" → Manager
  └─ 托盘"退出" → 停止监听 → 保存 → 退出
```

## 6. 交互规格

### 6.1 候选窗键盘

| 按键 | 行为 |
|---|---|
| 输入 | 实时搜索（防抖 80ms，中文/代码/路径） |
| ↑ / ↓ | 选择上/下一条（跨分页自动滚动） |
| PageUp/PageDown/Home/End | 快速翻页/首尾 |
| Enter | 粘贴选中项（写剪贴板 + 恢复窗口 + 模拟 Ctrl+V） |
| Ctrl+Enter | 仅复制到剪贴板，不自动粘贴 |
| Ctrl+1…9 | 直接粘贴当前列表第 1–9 项 |
| Tab / Shift+Tab | 切换过滤：全部→文本→图片→文件→收藏 |
| Ctrl+P | 收藏/取消收藏选中项 |
| Delete | 删除选中项（收藏项需再次确认） |
| Ctrl+, | 打开设置 |
| Esc | 隐藏面板 |

鼠标：单击选中、双击粘贴；右键菜单（粘贴 / 仅复制 / 收藏 / 删除 / 复制文本）；滚轮滚动；
悬停显示截断内容的 tooltip 或右侧预览浮层（图片放大）。

### 6.2 列表项

- 类型图标（文本/图片/文件）或 40×40 缩略图（圆角 8px）。
- 标题：文本取首行前 80 字符；图片为 `图片 W×H`；文件为 `文件名（+N）`。
- 预览：文本取第二行/后续内容拼一行；文件显示路径；图片显示尺寸与大小。
- 右侧：相对时间（刚刚/3 分钟前/昨天 14:22/10-30）、收藏星标、来源应用小图标（可选）。
- 选中态：强调色 12% 背景 + 左侧 2px 竖条；悬停 6% 背景。
- 显示条数：单页虚拟化渲染 200 条，滚动加载更多（数据库分页）。

### 6.3 粘贴行为

1. 呼出时记录 `GetForegroundWindow()`（若等于本程序窗口则忽略并保留上次有效句柄）。
2. 选中后：写剪贴板（按类型）→ 隐藏本窗口 → `SetForegroundWindow(prev)`（必要时
   `AttachThreadInput` 绕过前台锁定）→ 等待 40–80ms → `SendInput` Ctrl+V。
3. 失败判定：目标进程提权（UIPI）/窗口已销毁/句柄无效 → 不重试，保留剪贴板内容并提示
   "已复制，请手动 Ctrl+V"（面板内 toast + 可选托盘气泡）。
4. 设置项：粘贴后是否保留剪贴板内容（默认保留）、是否粘贴后置顶所选记录（默认是）、
   粘贴按键（默认 Ctrl+V，可切 Shift+Insert）。
5. 禁止记录本程序自身回写剪贴板的内容（序列号/内部标记双重判断）。

### 6.4 管理台

左侧 184px 导航：**历史 / 收藏 / 设置 / 关于**。

- 历史：虚拟化表格（类型、内容摘要、来源、时间、大小、收藏），搜索 + 类型/时间过滤，
  右/下详情面板：文本全文（只读、可复制、等宽、滚动）、图片大图（缩放/另存）、文件清单；
  多选批量删除/收藏/导出；分页 500/页。
- 收藏：收藏列表（永不自动清理），文件收藏显示"已保存副本/仅路径"状态，可"另存为"。
- 设置：快捷键录制（含 Win+V 接管开关与状态诊断）、容量与清理（条数/图片 MB/文件 MB/
  单条文本上限/单文件上限）、行为（粘贴后保留、失焦隐藏、开机自启、恢复剪贴板）、
  忽略应用名单（进程名，带"从运行中的进程添加"）、数据（打开目录、导出 JSON、
  清空历史、重建索引、恢复默认）。
- 关于：版本、快捷键帮助、日志目录、数据目录、许可。

## 7. 视觉设计规范

**风格关键词**：深色、磨砂玻璃、克制、信息密度高但不拥挤、圆角与细线。

### 7.1 色板

| 语义 | 值 |
|---|---|
| 窗口底（亚克力上叠层） | `#0E1116` @ 82% |
| 面板/侧栏 | `#131820` @ 92% |
| 卡片/输入框 | `#1A212B` |
| 分隔线/边框 | `#2A3340`（1px，8% 白叠加） |
| 主文本 | `#E6EDF3` |
| 次文本 | `#8B98A9` |
| 弱文本/占位 | `#5C6878` |
| 强调（选中/链接/按钮） | `#6E8BFF`（悬停 `#8AA2FF`） |
| 危险 | `#FF6B6B` |
| 成功/已收藏 | `#4ADE80` / `#F5C451` |
| 类型色 | 文本 `#6E8BFF`、图片 `#4ADE80`、文件 `#F5A524` |

### 7.2 字体与字号

- 中文：优先 `Deng.ttf`（等线）→ `simhei.ttf` → `msyh.ttc` → `simsun.ttc`（复用现有
  `fonts.rs` 的回退加载模式，字体放 Proportional/Monospace 族末尾）。
- 拉丁/数字：egui 默认字体（Inter 风格）。
- 字号：标题 15（semibold 效果用颜色/字重近似）、列表标题 13.5、正文 13、辅助 11.5、
  代码预览用 Monospace 12。
- 行高：列表行 64px；搜索框高 40px；圆角：窗口 12、卡片 10、输入框 9、缩略图 8、胶囊 999。

### 7.3 组件

- 搜索栏：左侧放大镜图标、占位"搜索剪贴板历史…"、右侧清空 ✕ 与快捷键提示。
- 过滤胶囊：选中强调色实底白字，未选中透明描边灰字。
- 底部提示条：`↑↓ 选择 · Enter 粘贴 · Ctrl+1…9 快选 · Esc 关闭`，右侧统计 `共 1,234 条` + 齿轮。
- Toast：窗口下部浮起，1.6s 自动消失（如"已复制"、"已收藏"）。
- 滚动条：宽 6px、圆角、悬停加亮。
- 空状态：居中图标 + "还没有记录，复制点什么试试"；搜索无结果时给"清除筛选"按钮。

### 7.4 动效

- 窗口出现：透明度 0→1 + 上移 6px，120ms ease-out。
- 行选中/悬停：背景色 80ms 线性。
- 模式切换（Picker↔Manager）：150ms 尺寸/位置过渡（egui 无原生窗口动画，用内容淡入替代）。
- 不做弹跳、粒子、旋转等花哨动效。

## 8. 功能清单

### 8.1 v1 必做（P0/P1）

1. 剪贴板监听与采集：文本 / 图片 / 文件列表 / 混合格式优先级；1s 内入库可见。
2. 去重（SHA-256）与置顶最近；来源应用/窗口标题记录（尽力而为）。
3. 搜索（FTS5 trigram，降级 LIKE）、类型过滤、收藏过滤、分页/虚拟化。
4. 收藏/取消收藏、删除、清空（二次确认）、暂停记录（托盘/面板）。
5. 全局快捷键：Win+V 接管 + 备用可配置快捷键；冲突检测与提示；录制式设置。
6. Enter 自动粘贴（前台恢复 + SendInput）；Ctrl+Enter 仅复制；失败降级提示。
7. 持久化与容量治理（5000 条 / 200MB 图片 / 200MB 收藏文件 / 单条文本 2MB，均可调）。
8. 文件收藏实体副本（单文件 ≤50MB；原文件删除后仍可粘贴，还原到恢复目录）。
9. 托盘图标 + 菜单：打开面板 / 管理历史 / 暂停记录 / 清空历史 / 设置 / 开机自启 / 退出。
10. 开机自启（HKCU Run，开关）、单实例（命名互斥体 + 唤起）。
11. 忽略应用名单 + 自动跳过 `Clipboard Viewer Ignore` 格式（密码管理器）。
12. 深色亚克力 UI（Win11 DWM，回退半透明深色）、全中文。
13. 日志 + `--selftest` / `--diagnose` 无界面自检。
14. 数据备份：导出/导入 JSON（含设置与文本；图片/文件以相对路径引用可选）。

### 8.2 后续（v1.1+，不在本次验收）

多选收藏夹分类、快捷键直接粘贴第 N 条收藏、条目标签、图片 OCR、富文本、亮色主题、
便携模式（数据放 exe 同目录）、多显示器 DPI 增强。

### 8.3 明确不做

见 §2.2。

## 9. 架构

### 9.1 进程与线程

| 线程 | 职责 | 备注 |
|---|---|---|
| UI 主线程 | eframe/winit 事件循环；唯一创建窗口的线程；egui 渲染 | 所有 UI 状态、动作调度 |
| Sys 线程 | 隐藏消息窗口（`AddClipboardFormatListener`、`RegisterHotKey`、单实例消息）；`WM_CLIPBOARDUPDATE` 采集；`WM_HOTKEY`；低级键盘钩子（Win+V）；写剪贴板；恢复前台 + SendInput | 线程内 `GetMessage` 循环 |
| Store 线程 | 独占 SQLite 连接；写入队列（批量事务）、查询、清理、blob 落盘、收藏文件实体复制 | UI 永不直接碰数据库 |
| Thumb 线程 | 懒加载缩略图（PNG→RGBA），LRU 缓存 | 1 条线程足够 |
| 托盘 | `tray-icon`（内部自建线程/窗口）；事件由 UI 每帧 `try_recv` 轮询 | 生命周期跟随 App |

线程间通信：`std::sync::mpsc` 双向（Event / Command），每条事件/命令均为 enum；UI 通过
`egui::Context::request_repaint()`（预存 Context 克隆）唤醒。禁止跨线程共享可变状态，
唯一共享为 `Arc<Mutex<Settings>>`（UI 读写设置、Sys 线程读取热键/忽略名单）。

### 9.2 模块划分（文件即边界，尽量 <800 行/文件）

```
src/
├─ main.rs            入口：--selftest/--diagnose/--demo-seed/正常启动；单实例检查
├─ logging.rs         滚动日志（%APPDATA%\ClipNest\logs\app.log，1MB×3）
├─ config.rs          路径解析、settings.json 读写（原子写）、默认值
├─ model.rs           共享类型：ClipKind/ClipItem/ClipFilter/Settings/HotkeyChord/
│                     Event/Command/SearchPage/ClipError
├─ app.rs             egui App：模式状态机、事件泵、动作执行、快捷键路由、托盘
├─ store/
│  ├─ mod.rs          StoreHandle：命令通道封装与对外 API
│  ├─ db.rs           schema/迁移/CRUD/去重/查询/分页/清理/元数据
│  ├─ blob.rs         内容寻址 blob 存储、引用计数、缩略图文件、恢复目录
│  ├─ export.rs       JSON 导出/导入
│  └─ tests.rs        单元测试（内存库 + 临时目录）
├─ clipboard/
│  ├─ mod.rs          类型与优先级、采集结果模型
│  ├─ reader.rs       打开剪贴板、读取文本/PNG/DIBV5/HDROP、重试与锁竞争处理
│  ├─ writer.rs       按类型写回剪贴板（CF_UNICODETEXT/CF_DIB/CF_HDROP/PNG）
│  └─ dib.rs          DIB↔RGBA、PNG 编码、尺寸解析
├─ sys/
│  ├─ mod.rs          SysHandle：启动线程、发送命令、事件接收
│  ├─ thread.rs       隐藏窗口类、消息循环、剪贴板监听、单实例消息
│  ├─ hotkey.rs       RegisterHotKey 注册/解析/格式化、冲突诊断
│  ├─ winv.rs         Win+V 键盘钩子接管与降级探测
│  ├─ paste.rs        前台窗口恢复、SendInput、UIPI 失败识别
│  ├─ autostart.rs    HKCU Run 读写
│  └─ util.rs         HWND/进程名/窗口标题/DPI 辅助
├─ ui/
│  ├─ mod.rs          UiState、模式与页面路由
│  ├─ theme.rs        色板、字体、控件样式、圆角、动效
│  ├─ picker.rs       候选窗：搜索/过滤/列表/底部提示/toast
│  ├─ manager.rs      管理台外壳（导航 + 页面路由 + 标题栏拖拽/缩放）
│  ├─ pages/          history.rs / favorites.rs / settings.rs / about.rs
│  ├─ widgets.rs      复用控件（搜索框、胶囊、列表行、开关、录制器、确认框）
│  ├─ thumbs.rs       缩略图缓存与后台加载
│  └─ fonts.rs        中文字体加载（复用现有实现模式）
├─ win/               （预留：亚克力/窗口工具；也可并入 sys）
└─ diag.rs            --selftest / --diagnose / --demo-seed
tools/screenshot.ps1  窗口截图验收脚本（复用已有实现）
docs/CONTRACT.md      模块接口契约（实现前先写，作为并行开发的边界）
```

### 9.3 数据流

```
Ctrl+C
  → Sys 线程 WM_CLIPBOARDUPDATE
  → clipboard::reader 读取并归一化（文本/图片/文件）
  → 计算 SHA-256、构造 ClipInput
  → 发送 StoreCommand::Upsert
  → Store 线程事务写库（重复则更新 last_used_at/use_count 并置顶）
  → 发送 Event::ClipsChanged(ids)
  → UI 线程 request_repaint → 面板列表增量刷新

搜索：UI(防抖) → StoreCommand::Search{gen, query, filter, page}
       → Store 线程查询 → Event::SearchResult{gen, items, total}
       → UI 丢弃过期 gen → 渲染（虚拟化）

粘贴：UI 选条目 → SysCommand::Paste{clip_id, plain} →
      Sys 线程写剪贴板 → 隐藏命令回 UI（UI 隐藏窗口）→ 恢复前台 → SendInput → Event::PasteResult
```

注：Windows 隐藏/显示窗口必须在 UI 线程执行；粘贴动作顺序为 UI 先隐藏窗口再通知 Sys 执行
SendInput（Sys 收到显式 `PasteNow` 命令后执行）。

## 10. 数据模型与存储

### 10.1 目录

```
%APPDATA%\ClipNest\
├─ data.db              SQLite 主库（WAL）
├─ data.db-wal / -shm
├─ blobs\
│  ├─ images\ab\cd\abcdef….png      图片原图（内容寻址，2 级目录）
│  ├─ thumbs\ab\cd\abcdef….jpg      96px 缩略图
│  ├─ files\<blob_id>\<原始文件名>…  收藏文件实体副本（保留相对路径）
│  └─ meta.json? （不建，blob 元数据入库）
├─ restored\<yyyyMMdd-HHmmss>\       收藏文件粘贴时的恢复目录
├─ logs\app.log (+ .1/.2)
└─ settings.json
```

### 10.2 SQLite schema（v1）

```sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA cache_size=-2048;          -- 2MB
PRAGMA temp_store=MEMORY;
PRAGMA foreign_keys=ON;

CREATE TABLE clips (
  id           INTEGER PRIMARY KEY,
  kind         INTEGER NOT NULL,            -- 0=text 1=image 2=files
  hash         TEXT    NOT NULL UNIQUE,     -- SHA-256(归一化内容)
  text         TEXT,                        -- 文本全文（≤2MB）或文件路径 JSON 冗余
  preview      TEXT    NOT NULL DEFAULT '', -- 首 8KB 归一化文本/摘要，供搜索与列表
  blob_id      TEXT,                         -- 图片 PNG / 文件实体目录 id
  files_json   TEXT,                         -- [{name,path,size,exists}] 文件清单
  byte_len     INTEGER NOT NULL DEFAULT 0,
  width        INTEGER, height INTEGER,
  created_at   INTEGER NOT NULL,            -- unix ms
  last_used_at INTEGER NOT NULL,
  use_count    INTEGER NOT NULL DEFAULT 1,
  pinned       INTEGER NOT NULL DEFAULT 0,  -- 收藏
  source_app   TEXT, source_title TEXT
);
CREATE INDEX idx_clips_order  ON clips(pinned DESC, last_used_at DESC);
CREATE INDEX idx_clips_kind   ON clips(kind, last_used_at DESC);
CREATE INDEX idx_clips_pinned ON clips(pinned, last_used_at DESC);

CREATE VIRTUAL TABLE clips_fts USING fts5(
  preview, text, content='clips', content_rowid='id', tokenize='trigram'
);  -- 探测失败则不建表，走 LIKE 降级
-- 外部内容表靠触发器同步（INSERT/UPDATE/DELETE 各一个），与 clips 同事务
-- 创建顺序：先建 transitions 再回填 'rebuild'

CREATE TABLE blobs (
  id        TEXT PRIMARY KEY,
  kind      INTEGER NOT NULL,               -- image / file
  rel_path  TEXT NOT NULL,                  -- 相对 blobs/ 的路径
  byte_len  INTEGER NOT NULL,
  sha256    TEXT NOT NULL,
  refs      INTEGER NOT NULL DEFAULT 0,     -- 引用计数
  created_at INTEGER NOT NULL
);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- meta: schema_version, fts_enabled, created_at, last_prune_at
```

去重规则：`hash` 唯一；重复复制 → `last_used_at=now, use_count+=1, pinned 不变`，并置顶；
不新增行、不新增 blob。

### 10.3 采集归一化

| 剪贴板格式 | 处理 |
|---|---|
| `CF_HDROP` | 文件列表（路径、名称、大小、存在性）；优先于文本 |
| `CF_UNICODETEXT` | 文本；去尾部 `\0`、保留内部换行；空白文本忽略 |
| `PNG`（注册格式） | 优先作为图片源（已是压缩数据） |
| `CF_DIBV5` / `CF_DIB` | 解析 BITMAPINFO(+V5) → RGBA → PNG（含 32/24/16/8bpp、top-down/bottom-up、BI_BITFIELDS） |
| 其他（HTML/RTF 等） | v1 忽略 |
| `Clipboard Viewer Ignore` 存在 | 不记录 |
| 进程名在忽略名单 | 不记录 |

限制：单条文本 ≤2MB（截断并标记，可配置）；图片像素 ≤ 40MP（超出跳过并日志）；
单条文件列表 ≤100 个；图片 PNG 编码失败则跳过。

### 10.4 清理（Retention）

触发：启动后 30s 一次 + 每 100 次写入 + 设置变更时。

顺序（不清理 `pinned=1`）：
1. 条数 > limit（默认 5000）→ 删最旧 `last_used_at` 的非收藏条目直到达标。
2. 孤儿图片 blob（refs=0）→ 物理删除文件与记录（图片随条目删除自动变孤儿）。
3. 收藏文件实体总量 > 上限（默认 200MB）→ **不再为新的收藏项复制实体**，改为记事仅路径
   并在 UI 提示"收藏文件副本已达上限"；**绝不清除已有收藏实体**（保护用户数据）。
4. 清理后刷新 FTS/统计；清理动作写日志。

容量统计：meta 缓存近似值（`image_bytes`、`fav_file_bytes`、`clip_count`），每写入更新，
启动时全量校准，避免每帧全表 SUM。

## 11. 搜索规格

- UI 输入防抖 80ms；输入即触发新 generation，旧结果丢弃。
- 查询：`query.trim()`；空查询 → 按 `pinned DESC, last_used_at DESC` 分页。
- FTS5 trigram：查询长度 ≥3 个字符时用 `clips_fts MATCH '"…"'`（精确子串，避免通配注入），
  按 `pinned`、`bm25`/时间排序；<3 字符或 FTS 不可用 → `preview LIKE '%…%' ESCAPE`。
- 过滤：kind ∈ {全部, 文本, 图片, 文件}，收藏过滤；时间过滤（管理台：今天/7 天/30 天）。
- 分页：search 页 200 条；管理台 500 条；返回 `total` 供底部统计。
- 中文、大小写不敏感（ASCII lower）；路径分隔符 `/` 与 `\` 等价处理。

## 12. 图片与文件细节

### 12.1 图片

- 采集：DIB → RGBA → PNG（`image::codecs::png`，压缩级别 6）写 blob；同时记录尺寸。
- 缩略图：最长边 96px，JPEG q80 存 `thumbs/`，UI 懒加载；内存 LRU：最多 64 张/16MB。
- 列表显示缩略图；详情大图按窗口缩放（egui texture 上传），支持"另存为"。
- 粘贴：优先 `PNG` 注册格式 + `CF_DIB`（32bpp BI_RGB，top-down 兼容）；个别老程序只认 DIB。

### 12.2 文件

- 采集只记录路径（不阻塞复制流程）；存在性在展示时刷新（管理台/浮层）。
- 收藏时：把文件实体复制到 `blobs/files/<id>/<相对路径>`（单文件 ≤50MB、总量 ≤200MB，
  超限提示）；文件夹递归复制但限制 200 个文件/200MB；失败则保留路径并标注"仅路径"。
- 粘贴：CF_HDROP（DROPFILES + 宽字符双 `\0` 结尾）。若条目有实体副本且原路径失效 →
  先还原到 `restored/<时间戳>/` 再放剪贴板；否则原路径直接放回。
- 删除条目时减少 blob 引用，refs=0 的文件实体按 retention 清理。

## 13. Win+V 接管方案（分三级，自动降级 + 设置可控）

用户要求"把现在 Win+V 互斥系统剪贴板改成我的这个"。Windows 默认把 Win+V 交给系统剪贴板
历史（Shell 注册），直接 `RegisterHotKey` 通常会被占用。实现按优先级探测：

**L1 直接注册**：`RegisterHotKey(hwnd, ID, MOD_WIN, 'V')`。成功则最干净（无钩子）。
- 探测流程：启动时先尝试注册；若失败记录状态 `WinVMode::RegisterFailed`。
- 若注册成功且系统剪贴板历史仍开启，可能双触发（系统 + 本程序）：两者同时收到？注册成功
  说明系统未占用，则系统历史不会弹；仍建议用户关闭系统历史（见 L3 可选）。

**L2 低级键盘钩子**：`WH_KEYBOARD_LL` 监听 `VK_LWIN/VK_RWIN` 状态与 `V` 键。
- 命中 Win+V → 吞掉 V down/up（`return 1`），发送 `Event::Hotkey(OpenPicker)`。
- 忽略 `LLKHF_INJECTED`（避免拦截 SendInput）。
- 已知风险：吞键后系统可能仍认为 Win 键"单独按下"导致开始菜单弹出。P0 阶段用脚本
  实测（模拟 Win+V 后截屏检查开始菜单），若出现则启用补偿策略：
  a) 补吞 Win 键 up 并在 30ms 后用 `SendInput` 注入一次"Win down+up"由系统消费；或
  b) 回退 L1/L3/L4，并在设置中明确说明。
- 提供独立开关："接管 Win+V（键盘钩子）"，状态在设置页显示（成功/失败/已降级）。

**L3 关闭系统剪贴板历史（可选、可恢复）**：
- 写 `HKCU\Software\Microsoft\Clipboard\EnableClipboardHistory = 0`，避免 Win+V 被系统
  抢走或双弹窗；设置里提供"恢复系统剪贴板历史"按钮。仅在用户开启"接管 Win+V"时询问。
- 注意：该注册表改动需重启 explorer 或下次登录完全生效，代码内做尽力而为 + 提示。

**L4 备用快捷键**：始终保留一个可配置的全局快捷键（默认 `Ctrl+\``），通过
`RegisterHotKey` 注册，Win+V 任何一种方案失败时用户仍可正常使用；设置页显示当前生效方式。

验收要求：Win+V 在本机实测能打开本工具面板且不弹系统剪贴板历史/开始菜单；
失败时能说明原因并自动使用 L4。

## 14. 错误处理与降级

| 场景 | 处理 |
|---|---|
| 剪贴板被占用（OpenClipboard 失败） | 5 次 ×20ms 重试；仍失败记日志跳过，不崩溃 |
| 延迟渲染格式读取失败 | 换格式继续；全失败跳过 |
| 图片解码失败 | 跳过并日志（不写库） |
| SQLite 打开/迁移失败 | 弹错误对话框 + 日志；提供"打开数据目录/重建数据库" |
| 数据库损坏 | 启动检测 `PRAGMA quick_check`，失败则改名备份并重建空库，提示 |
| 热键注册失败 | 设置页红字提示冲突；保留备用快捷键 |
| 亚克力不可用 | 回退半透明深色背景（功能不变） |
| SendInput 失败/目标提权 | 保留剪贴板 + toast 提示手动粘贴 |
| 托盘创建失败 | 窗口仍可用，关闭时提示"最小化到托盘失败，可退出" |
| 设置文件损坏 | 用默认设置并备份坏文件为 `.bad` |
| 单实例通信失败 | 允许再启动一个实例（不阻塞） |

## 15. 性能与内存预算

| 指标 | 目标 | 验收方法 |
|---|---|---|
| 空闲内存（隐藏/托盘） | ≤45MB | `Get-Process` WorkingSet64 抽样 10s |
| 面板打开内存 | ≤60MB | 同上 |
| 冷启动到托盘就绪 | ≤1.0s | 日志时间戳 + 实测 |
| 面板呼出到可输入 | ≤150ms | 日志/人工秒表 + 截图 |
| 复制到出现在列表 | ≤300ms（后台） | 日志 |
| 搜索响应（5000 条） | ≤100ms | 内置计时日志 |
| 首次落库 1 万条后列表滚动 | 60fps 不卡 | 虚拟化 + 压测 |
| exe 体积 | ≤15MB（release strip + lto） | 文件属性 |
| 数据库 5000 条典型 | ≤50MB（不含图片） | 目录统计 |

实现手段：列表虚拟化只渲染可见行；缩略图 LRU；DB 页缓存 2MB；无 WebView；写入批处理
（同一帧多条合并事务）；搜索走独立线程；日志异步（简单 Mutex + 缓冲）。

## 16. 安全与隐私

- 全本地、零网络：无 HTTP 客户端依赖；代码审查确认无网络调用。
- 不记录密码管理器内容：识别 `Clipboard Viewer Ignore` 格式；忽略名单可加进程名。
- 数据权限：`%APPDATA%` 用户私有目录；日志不含完整剪贴板内容（只记类型/大小/来源）。
- 删除即物理删除 blob；清空历史提供"同时删除图片/文件副本"复选。
- 设置页提供"暂停记录"（托盘也可一键暂停，图标变灰）。
- 不申请管理员权限；不写 HKLM；自启只写 HKCU。

## 17. 测试策略与验收

### 17.1 自动化

- `cargo test`：store（CRUD/去重/FTS 降级/清理/分页）、clipboard DIB 解析、热键字符串
  解析、忽略规则、设置读写、文件清单序列化、retention 边界、导出/导入往返。
- `clipnest.exe --selftest report.json`：无界面全链路（临时目录建库 → 造文本/图片/文件
  → 查询/收藏/清理 → 校验）输出 JSON。
- `clipnest.exe --diagnose report.json`：版本、OS、依赖能力（FTS5、亚克力、托盘、热键、
  Win+V 探测结果）、数据目录、统计。
- `clipnest.exe --demo-seed N`：向真实库插入演示数据（供截图与演示，带确认）。
- UI 截图：`tools/screenshot.ps1` 截取面板/管理/设置/关于 4 张图，人工比对设计稿。

### 17.2 验收清单（v1）

- [ ] 复制文本/截图/多个文件 → 1s 内出现在面板；重启程序/重启系统后仍在。
- [ ] Win+V 打开本工具面板（本机实测），系统剪贴板历史不弹；备用 Ctrl+` 可用。
      若 P0 实测证明本机 Win+V 无法可靠接管（被系统/安全软件占用），验收降级为：
      L2 或 L4 生效 + 设置页明确显示降级原因 + 用户确认接受；不得静默失效。
- [ ] Enter 自动粘贴成功：记事本 / 浏览器输入框 / Excel 单元格 / 资源管理器（文件）。
- [ ] 目标为管理员进程时失败有明确提示且剪贴板内容正确。
- [ ] 收藏的已删除源文件仍能粘贴出文件（恢复目录）。
- [ ] 容量：5000 条 + 200MB 图片压测后，启动/搜索/滚动/内存达标（§15）。
- [ ] 忽略名单、暂停记录、开机自启、单实例、清空历史、导出/导入全部可用。
- [ ] 全部中文、深色亚克力效果符合设计、无联网请求、无崩溃日志。
- [ ] `cargo test` 全绿；`--selftest` / `--diagnose` 通过；README/截图/interfaces 文档齐全。

## 18. 实施阶段与团队分工

| 阶段 | 内容 | 负责人 |
|---|---|---|
| P0 骨架与风险验证 | Cargo 工程、依赖锁定、egui 空窗 + 亚克力、托盘、热键、Win+V 钩子实测、单实例、日志/配置、`docs/CONTRACT.md` | Lead |
| P1 存储层 | db/blob/retention/export + 单元测试（并行） | Teammate A（store） |
| P2 Win32 层 | 剪贴板采集/回写、DIB、粘贴、热键、Win+V、托盘、自启（并行） | Teammate B（win32） |
| P3 UI 层 | theme/picker/manager/pages/widgets/thumbs/fonts（并行） | Teammate C（ui） |
| P4 集成与验收 | app.rs 接线、联调、压测、内存实测、截图、README、release | Lead |
| P5 独立复核 | 验收清单逐项复核 + 代码审查 + 边界测试（独立 subagent） | Verifier |

并行开发约定：
- 先写 `docs/CONTRACT.md` 锁定 `model.rs` 类型与各模块公开 API；实现中如需变更契约，
  先改 CONTRACT.md 并在任务板同步。
- 文件级写入域互不重叠：A 只写 `src/store/**`、B 只写 `src/clipboard/**`+`src/sys/**`、
  C 只写 `src/ui/**`；Lead 写 `src/main.rs`/`src/app.rs`/`src/model.rs`/`src/config.rs`/
  `src/logging.rs`/`src/diag.rs`/`Cargo.toml`/`docs/**`/`tools/**`。
- 各模块先写可编译的桩 + 单元测试，再填实现；严禁跨域改文件（需要时由 Lead 协调）。
- 每完成一个里程碑跑 `cargo test` 与 `cargo build`，Lead 最后统一跑 release 与实测。

## 19. 风险登记（Top 8）

| # | 风险 | 概率 | 影响 | 对策 |
|---|---|---|---|---|
| 1 | Win+V 钩子导致开始菜单弹出/键状态异常 | 中 | 高 | P0 实测；补偿吞 Win up；失败降级 L4 并明确提示 |
| 2 | 管理员目标窗口无法 SendInput | 高 | 中 | 检测失败提示手动粘贴；剪贴板内容保留 |
| 3 | 亚克力在透明窗口下不生效/发黑 | 中 | 中 | window-vibrancy + 自绘半透明兜底；P0 截图验证 |
| 4 | egui 单窗口模式切换尺寸/装饰闪烁 | 中 | 低 | P0 验证 Decorations/WindowLevel 命令；必要时改为独立管理器窗口 |
| 5 | 浏览器/Office 复制产生多格式，误判类型 | 中 | 中 | 优先级表 + 实机抽测（Chrome/Excel/Word/截图工具） |
| 6 | 中文搜索 FTS5 trigram 不可用 | 低 | 中 | 启动探测；LIKE 降级仍可用 |
| 7 | 长时间运行内存上涨（缩略图/字体纹理） | 中 | 中 | LRU + 纹理释放 + 24h 压测 |
| 8 | DIB 变体解析错误导致花图 | 中 | 低 | 多 bpp/方向单元测试 + 实机截图比对 |

## 20. 附录

### A. 默认参数

| 参数 | 默认 | 可调范围 |
|---|---|---|
| 总条数上限 | 5000 | 500–无限 |
| 图片总量上限 | 200MB | 50MB–无限 |
| 收藏文件实体上限 | 200MB | 0–2GB |
| 单条文本上限 | 2MB | 64KB–16MB |
| 收藏单文件上限 | 50MB | 1–512MB |
| 单条文件数上限 | 100 | 1–1000 |
| 面板尺寸 | 620×560 | 小/中/大 3 档 |
| 面板位置 | 光标屏幕居中 | 居中/记住位置 |
| 失焦隐藏 | 开 | 开/关 |
| 粘贴后保留剪贴板 | 开 | 开/关 |
| 粘贴后置顶 | 开 | 开/关 |
| 暂停记录 | 关 | — |
| 开机自启 | 关 | 开/关 |
| 备用快捷键 | Ctrl+` | 任意组合 |
| Win+V 接管 | 自动（L1→L2→L4） | 自动/仅钩子/关闭 |
| 日志级别 | info | error/warn/info/debug |

### B. 快捷键总表

| 场景 | 按键 | 说明 |
|---|---|---|
| 全局 | Win+V（接管） / Ctrl+`（备用） | 打开面板 |
| 面板 | ↑↓ / PgUp/PgDn / Home/End | 选择 |
| 面板 | Enter | 粘贴 |
| 面板 | Ctrl+Enter | 仅复制 |
| 面板 | Ctrl+1…9 | 快选粘贴 |
| 面板 | Tab / Shift+Tab | 切换过滤 |
| 面板 | Ctrl+P | 收藏 |
| 面板 | Delete | 删除 |
| 面板 | Ctrl+, | 设置 |
| 面板 | Esc | 关闭 |
| 管理台 | Ctrl+F | 搜索聚焦 |
| 管理台 | Ctrl+A | 全选 |
| 管理台 | Del | 删除选中 |
| 管理台 | Ctrl+E | 导出 JSON |
| 管理台 | Esc | 返回/关闭 |
| 托盘 | 单击 | 打开面板 |
| 托盘 | 右键 | 菜单 |

### C. 命令行

```
clipnest.exe                      正常启动（无参数）
clipnest.exe --selftest out.json  无界面全链路自检
clipnest.exe --diagnose out.json  环境与能力诊断
clipnest.exe --demo-seed 300      写入演示数据（需 --yes 确认）
clipnest.exe --version
clipnest.exe --help
```

## 21. 待确认 / 假设

- 用户已批准设计；名称 ClipNest（剪贴匣）为暂定名，可随时改。
- Win+V 最终生效方式取决于本机 P0 实测结果，降级策略见 §13；不承诺在所有 Windows 版本
  100% 接管（例如被组策略/安全软件拦截时回退备用快捷键）。
- Windows 自带剪贴板历史默认开启时是否与 L1 注册冲突，以 P0 实测为准。

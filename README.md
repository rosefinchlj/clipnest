# ClipNest · 剪贴匣

> Windows 深色磨砂玻璃剪贴板管理器。Rust + eframe/egui 实现：托盘常驻、Win+V / 全局快捷键呼出候选窗、文本/图片/文件历史持久化、收藏与容量治理、选中即自动粘贴回原窗口。

![Picker 候选窗](docs/screenshots/picker.png)

| 管理台 · 历史 | 管理台 · 设置 |
|---|---|
| ![历史](docs/screenshots/manager-history.png) | ![设置](docs/screenshots/manager-settings.png) |

## 为什么重新造一个

现有剪贴板工具常见问题：侧栏常驻占屏、界面粗糙、内存占用偏高（100MB+）、历史容量不可控。
ClipNest 的目标是**低占用、键盘优先、视觉克制**：

- 纯 Rust，无 WebView；**守护进程不加载 GUI**，空闲内存约 **8MB（专用工作集）/ 10.6MB（Private）**；
  界面按需以子进程启动、用完退出（详见「架构」）。
- 托盘常驻，默认**不占任务栏**；按快捷键在光标屏幕中央弹出候选窗，选完即粘贴回原窗口；
- Ctrl+C 即入库，SQLite 持久化，重启/关机不丢；默认 5000 条 / 图片 200MB，可调、可改无限；
- 文本、图片（截图）、文件列表三类内容都支持；文件默认只记路径，**收藏时**才复制实体进库；
- 深色 Win11 磨砂玻璃（Mica/Acrylic）外观，中文界面。

## 功能

- **采集**：监听系统剪贴板（Ctrl+C / 右键复制 / 截图工具），自动去重、最近置顶，记录来源应用。
- **候选窗**：全局快捷键呼出；输入即搜（FTS5 trigram，中文子串友好）；`↑↓` 选择、`Enter` 粘贴、`Ctrl+Enter` 仅复制、`Ctrl+1…9` 快选、`Tab` 切过滤、`Ctrl+P` 收藏、`Delete` 删除、`Esc` 关闭。
- **自动粘贴**：先恢复呼出前的前台窗口，再模拟 Ctrl+V（可选 Shift+Insert）；目标窗口提权等情况自动降级为"已复制，请手动粘贴"。
- **管理台**：历史（搜索/分页/详情/多选/批量删除/导出）、收藏、设置、关于。
- **收藏**：收藏项永不自动清理；文件收藏会把实体复制进库，原文件被删也能粘贴出来。
- **容量治理**：条数 / 图片总量 / 收藏文件实体总量 / 单条文本 / 单文件上限均可配置，超出按最旧非收藏淘汰。
- **系统集成**：托盘菜单（打开面板 / 管理历史 / 暂停记录 / 清空 / 设置 / 开机自启 / 退出）、单实例、HKCU 开机自启、暂停记录、忽略应用名单。
- **隐私**：纯本地、零网络；密码管理器等带 `Clipboard Viewer Ignore` 标记的复制不会被记录；日志不含剪贴板全文。

## 快捷键

| 场景 | 快捷键 | 说明 |
|---|---|---|
| 全局 | `Win+V` | 呼出候选窗（接管系统剪贴板历史，见下） |
| 全局 | `Ctrl+`` | 备用呼出快捷键（可配置，冲突时自动降级提示） |
| 候选窗 | `↑` `↓` / `PgUp` `PgDn` / `Home` `End` | 选择 |
| 候选窗 | `Enter` | 粘贴到原窗口 |
| 候选窗 | `Ctrl+Enter` | 仅复制，不自动粘贴 |
| 候选窗 | `Ctrl+1`…`9` | 直接粘贴列表第 1–9 项 |
| 候选窗 | `Tab` / `Shift+Tab` | 全部 → 文本 → 图片 → 文件 → 收藏 |
| 候选窗 | `Ctrl+P` | 收藏 / 取消收藏 |
| 候选窗 | `Delete` | 删除选中项 |
| 候选窗 | `Ctrl+,` | 打开设置 |
| 候选窗 | `Esc` | 关闭面板 |
| 管理台 | `Ctrl+F` / `Ctrl+A` / `Delete` / `Ctrl+E` / `Esc` | 搜索 / 全选 / 删除 / 导出 / 返回 |

## Win+V 接管说明

Windows 把 `Win+V` 注册给系统剪贴板历史，普通 `RegisterHotKey` 会失败（错误 1409）。
ClipNest 按以下顺序自动降级（设置页显示实际状态）：

1. **L1**：直接注册 `Win+V`（极少数系统可成功）；
2. **L2**：安装低级键盘钩子，吞掉 `Win+V` 并打开 ClipNest，可选补偿逻辑避免开始菜单误弹；
3. **L4**：以上失败时仍可用备用快捷键（默认 `Ctrl+``，可自定义；被占用时会临时降级到其他组合并提示）。

> 系统剪贴板历史与 ClipNest 可同时存在；如只想用 ClipNest，可在 **Windows 设置 → 系统 → 剪贴板** 中关闭「剪贴板历史记录」（ClipNest 不代改系统设置）。

## 数据与目录

```
%APPDATA%\ClipNest\
├─ data.db         SQLite（WAL，FTS5 trigram 全文索引）
├─ blobs\images\   图片原图（内容寻址 SHA-256）与缩略图
├─ blobs\files\    收藏文件的实体副本
├─ restored\       收藏文件原路径失效后的恢复目录
├─ logs\app.log    滚动日志（不含剪贴板全文）
└─ settings.json   设置（原子写）
```

- 软件为单文件绿色 exe，不安装、不写系统目录（仅 HKCU 自启项与上述数据目录）。
- 默认容量：5000 条 / 图片 200MB / 收藏文件实体 200MB；设置里可改，收藏项不会被自动清理。
- 数据目录可用环境变量 `CLIPNEST_DATA_DIR` 覆盖（便携/测试用）。

## 构建与运行

需要 Rust 1.85+ 与 MSVC 工具链（Windows 10 1809+ / Windows 11）：

```powershell
cargo build --release
.\target\release\clipnest.exe     # 启动守护进程：托盘常驻、无窗口（双击也不弹控制台）
```

启动后：**托盘图标**常驻；按 `Win+V`（或实际生效的备用键，默认 `Ctrl+``，冲突时自动降级）
呼出候选窗；选中后自动粘贴回原窗口。右键托盘图标可打开管理台/设置/暂停/退出。

开发与自检：

```powershell
cargo test                              # 单元/集成测试
cargo run -- --ui-preview picker        # 只预览 UI（假数据，开发用）
.\target\release\clipnest.exe --selftest  target\selftest.json   # 无界面全链路自检
.\target\release\clipnest.exe --diagnose  target\diagnose.json   # 环境/能力诊断
.\target\release\clipnest.exe --demo-seed 300 --yes              # 写入演示数据
.\target\release\clipnest.exe --ui-picker --target 0             # 手动打开候选窗（调试）
.\target\release\clipnest.exe --ui-manager --page settings       # 手动打开管理台（调试）
```

开发环境变量：`CLIPNEST_DATA_DIR`（数据目录覆盖）、`CLIPNEST_SKIP_FONTS=1` / `CLIPNEST_SKIP_MICA=1`
（UI 子进程调试：跳过字体/背景材质，用于内存对比）。

## 架构（v1.1：守护进程 + 按需 UI 子进程）

```
clipnest.exe（守护进程，常驻，无 GUI）
├─ Sys 线程：隐藏消息窗口 / 全局热键 / Win+V 钩子 / 托盘 / 剪贴板监听 / 自动粘贴
├─ Store 线程：SQLite（WAL、FTS5、去重、容量治理、blob）
└─ 主线程：事件循环；收到呼出请求时 spawn 子进程 ↓
        clipnest.exe --ui-picker --target <hwnd>   （eframe 候选窗，粘贴后退出）
        clipnest.exe --ui-manager --page <page>    （eframe 管理台，关闭后退出）
```

- **为什么这样做**：v1.0 把 eframe/OpenGL/中文字体都放在常驻进程里，即使隐藏窗口也要 ~55MB
  Private；现在守护进程完全不初始化 GUI，空闲只有 **8MB 专用工作集 / 10.6MB Private**
  （对比 Ditto 的 ~10MB），启动更快、也不再有"隐藏窗口收不到事件"的问题。
- **界面是短命子进程**：候选窗/管理台关闭或粘贴完成后立即退出，UI 相关内存全部释放；
  子进程直接打开同一个 SQLite（WAL 多进程安全）做搜索/收藏/删除，粘贴时直接调用
  `clipboard::write_clipboard` + `sys::paste::restore_and_paste`。
- **设置同步**：UI 里保存设置后写 `settings.json`，守护进程 1 秒内轮询并重新注册快捷键/暂停状态。
- **单实例**：命名互斥体保护守护进程；再次双击 exe 会唤起已运行实例的候选窗。
- 候选窗/管理台 UI 仍由 egui 渲染（深色磨砂、自定义标题栏），列表虚拟化 + 缩略图 LRU + SQLite 2MB 页缓存。
- 模块边界与接口见 [`docs/CONTRACT.md`](docs/CONTRACT.md)，设计与实施计划见
  [`docs/superpowers/specs/`](docs/superpowers/specs/) 与 [`docs/superpowers/plans/`](docs/superpowers/plans/)。

> 注：界面子进程在部分机器上会因显卡/OpenGL 驱动（例如 NVIDIA 的 `nvgpucomp64.dll`）占用较多内存，
> 本机实测约 120–135MB；它只在面板打开期间存在，关闭/粘贴后即退出。守护进程常驻部分不受影响。

## 已知限制

- 仅支持 Windows（10 1809+ / 11），暂不做跨平台；
- 面板/管理台为按需子进程：首次打开约有 0.3–0.5s 启动延迟；界面进程会加载 OpenGL 驱动，
  在部分显卡机器上瞬态内存较高（本机 NVIDIA 驱动约 120–135MB），关闭后释放；
- 富文本（HTML/RTF）暂不保留格式，只按纯文本处理；
- 图片/文件导入导出暂不内嵌二进制（后续版本计划）；
- `Win+V` 的最终接管方式取决于系统/安全软件，若被占用会在设置页提示并回退备用快捷键；
- 以管理员权限运行的窗口会被 UIPI 拦截模拟按键，此时保留剪贴板内容供手动粘贴。

## 更新记录

- **v1.1.0**：修复托盘右键菜单点击无响应（tray-icon 内置菜单不产生 MenuEvent，改为 muda 直接弹菜单）；
  修复自动粘贴失败（托盘唤起时目标窗口被误判为自身、面板未抢到前台、隐藏窗口关闭异常）；
  修复"启动即 70MB"：改为**低内存守护进程 + 按需 UI 子进程**，空闲 ~8MB 专用工作集 / 10.6MB Private；
  设置保存后 1 秒内热键/暂停状态生效；托盘"设置"直达设置页。
- **v1.0.0**：首个可交付版本（Picker/Manager、Win+V 钩子、文本/图片/文件采集、SQLite 持久化）。

## 开发文档

- 设计规格：[`docs/superpowers/specs/2026-10-31-clipnest-design.md`](docs/superpowers/specs/2026-10-31-clipnest-design.md)
- 实施计划：[`docs/superpowers/plans/2026-10-31-clipnest-implementation.md`](docs/superpowers/plans/2026-10-31-clipnest-implementation.md)
- 模块契约：[`docs/CONTRACT.md`](docs/CONTRACT.md)
- 验收清单：[`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)
- 验收结果：[`docs/ACCEPTANCE-RESULT.md`](docs/ACCEPTANCE-RESULT.md)

## 许可

MIT

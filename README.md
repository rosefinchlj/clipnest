# ClipNest · 剪贴匣

> Windows 磨砂玻璃剪贴板管理器（深色 / 浅色）。Rust + eframe/egui 实现：托盘常驻、全局快捷键呼出候选窗、文本/图片/文件历史持久化、收藏与容量治理、选中即自动粘贴回原窗口。

![Picker 候选窗](docs/screenshots/picker.png)

| 管理台 · 历史 | 管理台 · 设置（深色） |
|---|---|
| ![历史](docs/screenshots/manager-history.png) | ![设置](docs/screenshots/manager-settings.png) |

| 浅色皮肤 · 候选窗 | 浅色皮肤 · 设置 |
|---|---|
| ![浅色候选窗](docs/screenshots/picker-light.png) | ![浅色设置](docs/screenshots/manager-settings-light.png) |

## 为什么重新造一个

现有剪贴板工具常见问题：侧栏常驻占屏、界面粗糙、内存占用偏高（100MB+）、历史容量不可控。
ClipNest 的目标是**低占用、键盘优先、视觉克制**：

- 纯 Rust，无 WebView；**单进程常驻并预先创建窗口**，按快捷键秒开（实测 20–80ms）；
  内存约 57MB Private（v1.2 按用户要求暂不再做低内存优化）；
- 托盘常驻，默认**不占任务栏**；按快捷键在光标屏幕中央弹出候选窗，选完即粘贴回原窗口；
- Ctrl+C 即入库，SQLite 持久化，重启/关机不丢；默认 5000 条 / 图片 200MB，可调、可改无限；
- 文本、图片（截图）、文件列表三类内容都支持；文件默认只记路径，**收藏时**才复制实体进库；
- **深色 / 浅色两套皮肤**（设置 → 外观切换，保存后立即生效），Acrylic 模糊 + 半透明叠层；
  托盘图标按 DPI 预渲染，16px 也清晰；顶部拖拽条/管理台标题栏可按住拖动窗口。

## 功能

- **采集**：监听系统剪贴板（Ctrl+C / 右键复制 / 截图工具），自动去重、最近置顶，记录来源应用。
- **候选窗**：全局快捷键呼出；输入即搜（FTS5 trigram，中文子串友好）；`↑↓` 选择、`Enter` 粘贴、`Ctrl+Enter` 仅复制、`Ctrl+1…9` 快选、`Tab` 切过滤、`Ctrl+P` 收藏、`Delete` 删除、`Esc` 关闭。
- **自动粘贴**：先恢复呼出前的前台窗口，再模拟 Ctrl+V（可选 Shift+Insert）；目标窗口提权等情况自动降级为"已复制，请手动粘贴"。
- **管理台**：历史（搜索/分页/详情/多选/批量删除/导出）、收藏、设置、关于。
- **收藏**：收藏项永不自动清理；文件收藏会把实体复制进库，原文件被删也能粘贴出来。
- **容量治理**：条数 / 图片总量 / 收藏文件实体总量 / 单条文本 / 单文件上限均可配置，超出按最旧非收藏淘汰。
- **外观**：深色/浅色皮肤、整窗 Acrylic 磨砂、顶部 16px 拖拽条（候选窗）与管理台标题栏拖动。
- **系统集成**：托盘菜单（打开面板 / 管理历史 / 暂停记录 / 清空 / 设置 / 开机自启 / 退出）、单实例、HKCU 开机自启、暂停记录、忽略应用名单。
- **隐私**：纯本地、零网络；密码管理器等带 `Clipboard Viewer Ignore` 标记的复制不会被记录；日志不含剪贴板全文。

## 快捷键

| 场景 | 快捷键 | 说明 |
|---|---|---|
| 全局 | `Ctrl+``（可自定义） | 呼出/隐藏候选窗；在设置页录制后立即生效，冲突会给提示 |
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

## 快捷键与 Win+V 说明

- **v1.2 起不再接管 `Win+V`**（按用户反馈移除）：不注册 Win+V、也不安装低级键盘钩子，
  系统剪贴板历史保持 Windows 原生行为。
- 呼出面板使用**备用快捷键**（默认 `Ctrl+``，可在 **设置 → 系统集成/快捷键** 中录制修改）。
  保存后立即重新注册，无需重启；若组合被其他程序占用，设置页会显示"注册失败/被占用"提示。
- 快捷键注册失败时不会静默换键：面板右上角/设置页会显示实际注册状态，避免"设了没效果"。
- 提示：若系统剪贴板历史与 ClipNest 都想用，可自行在 **Windows 设置 → 系统 → 剪贴板** 里开关。

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
.\target\release\patch-icon.exe .\target\release\clipnest.exe assets\icon.ico   # 写入 exe 图标（无 rc.exe 也可）
.\target\release\clipnest.exe     # 启动后托盘常驻、无可见窗口（双击也不弹控制台）
```

仓库根目录也提供 `tools\build-release.ps1`（构建 + 打图标一步完成）。

启动后：**托盘图标**常驻；按实际生效的快捷键（默认 `Ctrl+``，设置里可改）呼出候选窗；
选中后自动粘贴回原窗口。右键托盘图标可打开管理台/设置/暂停/退出。

开发与自检：

```powershell
cargo test                              # 单元/集成测试
cargo run -- --ui-preview picker        # 只预览 UI（假数据，开发用）
.\target\release\clipnest.exe --selftest  target\selftest.json   # 无界面全链路自检
.\target\release\clipnest.exe --diagnose  target\diagnose.json   # 环境/能力诊断
.\target\release\clipnest.exe --demo-seed 300 --yes              # 写入演示数据
```

开发环境变量：`CLIPNEST_DATA_DIR`（数据目录覆盖）、`CLIPNEST_SKIP_FONTS=1` / `CLIPNEST_SKIP_MICA=1`
（跳过字体/背景材质，用于内存对比）。

## 视觉效果（v1.3：Acrylic 磨砂玻璃）

- 窗口背景使用 **Acrylic**（`SetWindowCompositionAttribute`）实现真正的背景模糊；Win11 的 Mica
  （DWM system backdrop）在 egui/OpenGL 窗口下不会合成，因此只作为 Acrylic 失败时的兜底。
- 整窗统一铺 75% 深色半透明层（面板 84%），卡片/输入框保持较高不透明度，兼顾"透出模糊背景"和文字可读性。
- **需要在 Windows 设置 → 个性化 → 颜色 → 透明效果 中保持开启**；关闭后系统会禁用模糊，只剩纯色。
- 托盘图标按系统 DPI 从内嵌的 16/20/24/28/32px 预渲染图中选择同尺寸版本，避免高分辨率图标被缩小发虚。

## 架构（v1.2：单进程，窗口预创建）

```
clipnest.exe（单进程常驻）
├─ 主线程：eframe/egui 窗口（启动时创建，默认移到屏幕外；快捷键/托盘唤起时移回光标屏幕）
├─ Sys 线程：隐藏消息窗口 / 全局快捷键 / 托盘 / 剪贴板监听 / 自动粘贴
└─ Store 线程：SQLite（WAL、FTS5、去重、容量治理、blob）
```

- **为什么回到单进程**：v1.1 的"守护进程 + 按需 UI 子进程"虽然空闲内存只有 ~8MB，
  但每次打开面板都要启动子进程（0.3–0.5s 延迟）。v1.2 按用户要求改回单进程：
  **窗口在启动时就创建好，OpenGL/字体/材质全部就绪，打开面板只需把窗口移回屏幕**，
  实测 **20–80ms**；代价是常驻内存较高（约 57MB Private），暂不优化。
- 窗口在屏幕外时保持 `visible`（而不是真正隐藏），这样事件循环不会被挂起；
  隐藏时把前台还给呼出前的窗口，避免"面板关掉了键盘还没回来"。
- 设置保存后由 UI 直接 `SysCmd::SetHotkeys` 立即重新注册，并监听 `settings.json`
  变化作为兜底（托盘暂停/自启、外部修改也会生效）。
- 单实例：命名互斥体；再次双击 exe 会唤起已运行实例的候选窗。
- 模块边界与接口见 [`docs/CONTRACT.md`](docs/CONTRACT.md)，设计与实施计划见
  [`docs/superpowers/specs/`](docs/superpowers/specs/) 与 [`docs/superpowers/plans/`](docs/superpowers/plans/)。

## 已知限制

- 仅支持 Windows（10 1809+ / 11），暂不做跨平台；
- 单进程常驻内存约 57MB Private（v1.2 按用户要求不再做低内存拆分）；
- 富文本（HTML/RTF）暂不保留格式，只按纯文本处理；
- 图片/文件导入导出暂不内嵌二进制（后续版本计划）；
- v1.2 起不接管 `Win+V`，系统剪贴板历史保持 Windows 原生行为；
- 以管理员权限运行的窗口会被 UIPI 拦截模拟按键，此时保留剪贴板内容供手动粘贴。

## 更新记录

- **v1.4.0**：新增**浅色皮肤**（设置 → 外观，保存后立即切换）；候选窗顶部新增拖拽条、管理台标题栏可拖动窗口；
  修复 exe 没有图标/任务管理器无图标（无 `rc.exe` 环境下用 `patch-icon` 写 RT_GROUP_ICON 资源）；
  清理全部编译警告（移除已废弃的 Win+V 模块/类型、未使用方法和字段；粘贴置顶改为单调时间戳，避免同毫秒排序错乱）。
- **v1.3.0**：修复"看不到磨砂玻璃"——Mica 在 OpenGL 窗口下不合成，改为 Acrylic 模糊 + 全窗半透明叠层
  （包含侧栏/透明区域统一铺底）；重做托盘图标：按 DPI 选 16/20/24/28/32px 预渲染图，高对比剪贴板造型，
  解决原来 256px 图标被缩小导致的模糊/看不清问题。
- **v1.2.0**：按反馈移除 `Win+V` 接管（不再安装键盘钩子）；修复自定义快捷键保存后不生效
  （UI 保存后立即重注册 + settings.json 监听兜底，失败会明确提示）；回到 v1.0 单进程方案，
  窗口启动时预创建、打开面板 20–80ms，隐藏时自动把前台还给原窗口。
- **v1.1.0**：修复托盘右键菜单点击无响应（tray-icon 内置菜单不产生 MenuEvent，改为 muda 直接弹菜单）；
  修复自动粘贴失败（托盘唤起时目标窗口被误判为自身、面板未抢到前台、隐藏窗口关闭异常）；
  曾引入"低内存守护进程 + 按需 UI 子进程"架构（v1.2 已按用户要求回退）。
- **v1.0.0**：首个可交付版本（Picker/Manager、Win+V 钩子、文本/图片/文件采集、SQLite 持久化）。

## 开发文档

- 设计规格：[`docs/superpowers/specs/2026-10-31-clipnest-design.md`](docs/superpowers/specs/2026-10-31-clipnest-design.md)
- 实施计划：[`docs/superpowers/plans/2026-10-31-clipnest-implementation.md`](docs/superpowers/plans/2026-10-31-clipnest-implementation.md)
- 模块契约：[`docs/CONTRACT.md`](docs/CONTRACT.md)
- 验收清单：[`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)
- 验收结果：[`docs/ACCEPTANCE-RESULT.md`](docs/ACCEPTANCE-RESULT.md)

## 许可

MIT

# ClipNest v1 验收结果

- 验收日期：2026-10-31
- 环境：Windows 11 build 26300 x64，双 2560×1440 显示器；用户进程非提权；本机同时运行 Ditto（占用 `Ctrl+`` 全局热键）
- 被测版本：`target\release\clipnest.exe`（8.52 MiB，release + strip + LTO）
- 源码快照：git commit `（见仓库 tag v1.0.0）`；验收方式：Lead 独占桌面 E2E + 独立只读审计（subagent）+ 自动化单测/自检

## 1. 自动化结果

| 项 | 结果 | 证据 |
|---|---|---|
| `cargo test` | **PASS**：146 passed / 0 failed / 1 ignored（ignored 为真实剪贴板冒烟，手动执行过 3 次通过） | `cargo test` 输出 |
| `cargo build --release` | **PASS**，exe 8.52 MiB（目标 ≤15MB） | `target\release\clipnest.exe` |
| `--selftest`（无界面全链路） | **PASS**：14/14 checks，61ms | `target\selftest-final.json` |
| `--diagnose` | **PASS**：OS 10.26300、FTS5 trigram=true、Mica(tabbed)=true、SQLite 3.54.1 | `target\diagnose-final.json` |
| 无网络 | **PASS**：无 HTTP 客户端依赖；源码无网络调用（仅示例 URL 文本） | 依赖树/源码审计 |
| 隐私 | **PASS**：日志只记类型/字节/hash 前 8 位/来源；`Clipboard Viewer Ignore` 与自写标记不入库 | 源码审计 + 运行日志 |
| 独立审计 | 带风险 PASS（风险均为收尾项，已在 v1 修复；详见 §4） | subagent 审计报告 |

## 2. 真机 E2E（Lead 独占桌面执行）

| 场景 | 结果 | 证据 |
|---|---|---|
| 启动/托盘/单实例 | **PASS**：Sys 隐藏窗口、托盘创建、AddClipboardFormatListener 注册；第二个实例被单实例互斥体拦截并发 WM_COPYDATA 唤起首个实例，进程数保持 1 | 日志：`检测到已有 ClipNest 实例` / `呼出面板（第二实例唤起）` |
| 文本采集 | **PASS**：`Set-Clipboard` 复制文本 → 900ms 内日志 `采集剪贴板：类型=文本 字节=33 hash=964fc9b7 来源=notepad.exe` | `target\regress-data\logs\app.log` |
| 备用快捷键降级链 | **PASS**：`Ctrl+`` 被 Ditto 占用（1409）→ 自动使用 `Ctrl+Shift+V`，状态 `ok=true`；注入热键可稳定呼出面板 | 日志：`快捷键降级：Ctrl+` 被占用，已临时使用 Ctrl+Shift+V` |
| Win+V 接管 | **L2 生效**：L1 `RegisterHotKey(MOD_WIN,V)` 失败（1409）→ 低级键盘钩子安装成功（含开始菜单补偿）；**物理 Win+V 需用户人工验收**（见 §3） | 日志：`Win+V：L2 键盘钩子已安装（含开始菜单补偿）` |
| 面板交互 | **PASS**：热键呼出后 Picker 取得前台焦点；截图 `target\e2e2-picker.png`、`target\regress-data\...` | PrintWindow 截图 |
| 自动粘贴（关键） | **PASS**：Picker 中 Enter → 窗口隐藏 → 恢复原前台窗口（记事本）→ SendInput Ctrl+V；记事本内容 `REGRESSION-171705-新命令通路` 与期望完全一致（match=True） | 两次独立 E2E（17:00、17:17）均 PASS |
| 图片采集 | **PASS**：STA 线程 `Clipboard.SetImage(320×200 PNG)` → 入库 kind=1，宽高 320×200，PNG 1.0KB；重复事件被去重为 1 条 | SQLite 查询 `target\e2e3-data\data.db` |
| 文件采集 | **PASS**：`SetFileDropList(2 个文件)` → 入库 kind=2，`files_json` 含 2 条路径/大小/exists，重复事件去重为 1 条 | SQLite 查询 `target\e2e4-data\data.db` |
| 持久化 | **PASS**：多轮进程重启后 SQLite 数据仍在；`--selftest` 验证去重/收藏/清理/导出 | 多次启动 |
| 内存（release，真实 Sys 常驻） | 隐藏空闲：**Private 54.6MB** / WS 110.8MB；面板打开：**Private 62.0MB** / WS 123.2MB（目标≤60MB，空闲达标、面板略超 2MB，见 §4） | `target\final-data` 采样 |
| 字体内存优化 | Private 84.3MB → 54.6MB（-30%），mmap 零拷贝 + `FontData::from_static` | 日志 `内存映射零拷贝` |

## 3. 仍需用户人工验收（约 2 分钟）

1. **物理 Win+V**：启动 release（默认隐藏，托盘常驻）→ 在记事本按物理 `Win+V` → 期望面板出现、开始菜单/系统剪贴板历史不弹；再单按 `Win`（开始菜单正常）、`Win+E`（资源管理器正常）、`Esc` 关闭；再用实际生效的备用键（本机为 `Ctrl+Shift+V`）复验一次。
2. **托盘**：左键单击打开面板；右键检查 7 项菜单（打开面板/管理历史/暂停记录/清空历史/设置/开机自启/退出）；"暂停记录"后复制不入库、"退出"后进程与图标消失。
3. **开机自启**：设置页打开开关后 `reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v ClipNest` 应存在；关闭后消失。
4. **UAC 粘贴降级**：以管理员运行的应用中触发粘贴，应提示"已复制，请手动粘贴"且剪贴板内容正确。
5. **其它粘贴目标**：浏览器输入框、Excel 单元格、资源管理器（文件列表）各测一次 Enter 粘贴。
6. **忽略名单/密码管理器**：忽略名单加入 notepad.exe 后复制不记录；密码管理器复制不记录（Clipboard Viewer Ignore）。
7. **清空/导出（UI 路径）**：管理台清空（二次确认 + 可选删文件）、导出 JSON 实际写盘。
8. **长稳**：常驻 2h 观察内存曲线（目标 ≤+10MB）。
9. **视觉**：深色磨砂、中文字体、64px 行、184px 导航、标题栏拖拽/缩放目检（截图见 `docs/screenshots/`）。

## 4. 已知偏差与说明

| 项 | 说明 |
|---|---|
| 面板内存 62.0MB（目标 60MB） | 打开面板时 egui 字体图集/纹理增长约 7MB；空闲 54.6MB 达标。可按需在 v1.1 降低字体尺寸档位/裁剪图集进一步压缩。 |
| 物理 Win+V | 无法用 SendInput 自动验证（Windows 忽略注入 Win 组合），已实现补偿逻辑，需用户按 §3 步骤验收；若不通过可在设置页关闭接管、仅用备用键。 |
| `Ctrl+`` 与 Ditto 冲突 | 本机 Ditto 占用 `Ctrl+``；ClipNest 自动降级为 `Ctrl+Shift+V` 并在设置页显示原因。退出 Ditto 后可改回。 |
| v1.1 计划项 | 系统剪贴板历史开关（现提示用户到 Windows 设置操作）、从运行中进程添加忽略项、导入 JSON、重建索引、详情大图（当前 96px 缩略图放大）、时间过滤、多选收藏分类。 |
| 审计发现的收尾项 | 截图归档、设置页占位文案、粘贴双取载荷/连按丢粘贴、README 快捷键/内存口径、契约注释 —— 已在 v1 修复。 |
| 审计快照 | 独立审计快照 17:09 曾出现瞬时编译失败（并行编辑），最终 release 17:13 之后重新全绿并完成 E2E；本验收以最终源码为准。 |

## 5. 证据文件

- `target\selftest-final.json`、`target\diagnose-final.json`：自检/诊断结果
- `target\e2e2-picker.png`、`target\final-picker.png`：真实采集后的 Picker 截图
- `target\regress-data\logs\app.log`、`target\final-data\logs\app.log`：最终 E2E 日志
- `docs\screenshots\`：README 使用的 Picker/Manager/设置/关于 4 张 UI 截图
- `docs\P0-FINDINGS.md`：Win+V/托盘/热键能力验证与物理按键验收步骤

---

## 6. v1.1 修复轮（2026-10-31 晚，用户实测反馈）

用户反馈：① 托盘右键菜单点击无反应；② 启动内存 70MB（Ditto 仅 ~10MB）；③ 无法粘贴。

### 根因与修复

| 问题 | 根因 | 修复 |
|---|---|---|
| 托盘菜单无响应 | tray-icon 内置菜单用 `TrackPopupMenu`（无 `TPM_RETURNCMD`），选中项只发 WM_COMMAND 到无人处理的窗口，muda `MenuEvent` 永不触发（事件/退出/管理台全部无效） | 不再把菜单交给 tray-icon；右键时在 Sys 线程用 muda `show_context_menu_for_hwnd` 弹出，选中项正常产生 MenuEvent → WM_APP_MENU → 托盘动作 |
| 无法粘贴（托盘/热键路径） | ① 托盘菜单打开时前台是本进程托盘隐藏窗口/自身窗口，`target_hwnd` 被记录为自身 → 恢复目标失败；② 隐藏窗口上 `ViewportCommand::Focus` 抢不到前台；③ 关闭时隐藏窗口的 close 命令不被处理，子进程残留 | ① Sys 线程 300ms 轮询记录"最近一个非本进程前台窗口"作为粘贴目标兜底；② 显示后显式 `SetForegroundWindow`（AttachThreadInput）+ 40ms 重试；③ 粘贴前把窗口移出屏幕（保持 visible），粘贴后用原生 `WM_CLOSE` 关闭 |
| 启动内存 70MB | v1.0 常驻进程初始化 eframe/OpenGL/CJK 字体（本机 NVIDIA 驱动还会加载 GPU 编译器等，Private 84–130MB） | **守护进程 + 按需 UI 子进程**：常驻进程不创建 GUI；候选窗/管理台按需 spawn，关闭/粘贴后退出、内存全部释放 |

### v1.1 实测

| 项 | 结果 |
|---|---|
| 守护进程空闲 | **专用工作集 7.6MB / Private 10.6MB**（Get-Counter；同机 Ditto ~10MB） |
| 托盘左键 / 右键菜单 | 左键打开候选窗；右键 7 项菜单全部生效：打开面板/管理历史/暂停/清空/设置（直达设置页）/开机自启/退出 |
| 托盘菜单 → 粘贴 | 记事本为目标：菜单打开面板 → Enter → 记事本内容与期望完全一致；子进程 exit code 0 |
| 热键 → 粘贴 | `Ctrl+Shift+V`（`Ctrl+`` 被 Ditto 占用后的降级组合）→ Enter → 记事本内容一致 |
| Esc / 失焦 / 关闭 | 候选窗关闭后子进程退出（exit 0），守护进程继续常驻 |
| 管理台 | 托盘"管理历史"→ 历史页；托盘"设置"→ 设置页；关闭后子进程退出 |
| 单实例 | 再次双击 exe → 唤起已运行实例的候选窗，进程数保持 1 |
| 托盘退出 | 守护进程与所有子进程退出，托盘图标移除 |
| 界面子进程内存 | 打开期间 Private 约 120–135MB（本机 NVIDIA OpenGL 驱动导致，`nvgpucomp64.dll` 等；与字体/Mica 无关，已用 `CLIPNEST_SKIP_FONTS/SKIP_MICA` 对照）；关闭后完全释放 |

### 说明 / 仍需人工验收

- 界面子进程的 GPU 驱动内存属于 egui/OpenGL 渲染栈固有成本（空白 eframe 程序同机测量同样 >130MB），
  若要求面板也保持 10MB 级，需要将 Picker 改为原生 Win32/Direct2D 绘制（可在 v2 评估）。
- 物理 `Win+V` 仍建议用户按 §3 步骤人工确认（注入按键无法验证官网）。
- 面板首次打开约 0.3–0.5s 子进程启动延迟（低内存架构的取舍）；再次打开同量级。
- 设置里修改快捷键/暂停后守护进程 1 秒内生效。

---

## 7. v1.2 修复轮（2026-11-02，用户反馈）

用户要求：① 删除 Win+V 接管；② 自定义快捷键没效果；③ 打开界面慢，回到 v1.0 单进程方案（内存暂不考虑）。

### 修改

- **移除 Win+V 接管**：不再注册 Win+V、不再安装低级键盘钩子（启动日志固定为
  "Win+V 接管：已移除"）；设置页原来的接管选项改为只读说明，Win+V 回到 Windows 原生行为。
- **快捷键立即生效**：UI 保存设置后直接 `SysCmd::SetHotkeys` 重新注册（不再依赖守护进程轮询）；
  同时监听 `settings.json` 变化兜底；注册失败会明确提示"注册失败/可能被占用"，不再静默降级。
  托管配置保存为原子写；监听端严格解析，读到半截文件不会被误判为"用户改成默认值"。
- **回到单进程（v1.0 方式）**：eframe 窗口在启动时创建（移到屏幕外、`with_active(false)` 不抢焦点），
  Store/Sys 线程常驻；快捷键/托盘唤起时把窗口移回光标屏幕并强制前台，**实测打开耗时 20–80ms**。
  隐藏时把窗口移回屏幕外并把前台还给呼出前的窗口；粘贴、Esc、失焦、托盘退出均验证。

### v1.2 实测（release，本机）

| 项 | 结果 |
|---|---|
| 启动 | 无可见窗口；窗口在 (-30000,-30000)，不抢前台；Private ~57MB（按用户要求暂不优化） |
| 打开速度 | 热键 78ms / 托盘菜单 68ms / 再次呼出 22ms（日志"候选窗显示耗时"） |
| 自定义快捷键运行时生效 | 设置保存后日志"热键重配：请求=Alt+` 生效=Alt+` ok=true"；随后按 Alt+` 成功呼出 |
| 隐藏后前台恢复 | Esc 隐藏后 `GetForegroundWindow` 回到呼出前窗口（验证 5837798→ 面板→5837798） |
| 托盘菜单粘贴 | 托盘菜单打开面板 → Enter → 记事本内容与期望完全一致；窗口随即移回屏幕外 |
| 托盘菜单项 | 打开面板 / 管理历史 / 设置 / 退出均验证；管理台 Esc 按页面语义返回/关闭 |
| 退出 | 托盘退出后进程退出（exit 0），Sys/Store 线程正常清理 |
| 测试 | `cargo test` 146 passed；`--selftest` 14/14 PASS；`--diagnose` OK；release exe 8.52MB |

### 说明

- 物理 Win+V 已不再被拦截，按 Win+V 会打开 Windows 自带剪贴板历史（符合"删除该快捷方式"的要求）。
- 自定义快捷键若被其他软件占用，设置页会显示注册失败原因；请换一个组合。
- v1.1 的"低内存守护进程 + UI 子进程"设计已撤销；docs/CONTRACT.md §7 保留为历史记录。

---

## 8. v1.3 修复轮（2026-11-02，用户反馈 "没有磨砂玻璃 / 托盘图标看不清"）

### 根因

- **磨砂玻璃看不到**：原先使用 Win11 Mica（DWM `DWMWA_SYSTEMBACKDROP_TYPE`），DWM 属性确实设置成功
  （诊断 `backdrop_type=4`），但 eframe/OpenGL 交换链的窗口内容对 DWM 是"不透明"的，Mica 不会合成到
  client 区；UI 本身又用 82%/100% 不透明深色填充，所以最终看起来就是纯深色。
- **托盘图标模糊**：托盘加载的是 256×256 `icon.png`，交给 Windows 后被缩放到 16/20/24px，细节（线条、
  夹子）糊成一团；如果 exe 旁没有 assets 目录还会退化成 16×16 纯蓝方块。

### 修复

- 背景材质改为 **Acrylic**（`SetWindowCompositionAttribute`，Win10/11 下对 OpenGL 窗口也能真正模糊），
  Mica 仅作兜底；启动日志会显示 `窗口背景材质生效: acrylic`。
- UI 改为半透明分层：整窗统一铺 75% 深色层（侧栏/透明区域也铺，避免"破洞"），面板 84%，
  卡片/输入框保持高不透明度；实测白色窗口后背景由纯 `#0E1116` 变为 `#44474A`（模糊透出），
  管理台可见背后窗口的模糊轮廓。
- 托盘图标重做：高对比蓝色渐变圆角块 + 白色剪贴板剪影；按系统 DPI 选择**同尺寸**预渲染 PNG
  （16/20/24/28/32px，全部内嵌进 exe），不再把 256px 缩到 16px。
- 由于本机 MSVC 环境缺少 `rc.exe`，exe 文件图标资源无法在构建时更新（托盘图标已内嵌，不受影响）；
  build.rs 会明确警告，不再静默失败。

### 实测

| 项 | 结果 |
|---|---|
| 背景材质 | 启动日志 `窗口背景材质生效: acrylic`；截图采样背景 `#44474A`（白色窗口后），非纯色 |
| 托盘图标 | 日志 `托盘图标：使用预渲染 16×16 图标（DPI 96，目标 16px）`；任务栏截图放大后可清楚识别"剪贴板" |
| 功能回归 | 热键/托盘打开面板 58–77ms；粘贴/管理台/退出正常 |
| 测试 | `cargo test` 146 passed；`--selftest` 14/14 PASS；release exe 8.49MB |

> 注意：Windows「设置 → 个性化 → 颜色 → 透明效果」必须开启，系统关闭透明效果时 Acrylic 模糊也会被禁用。

---

## 9. v1.4.0（2026-11-02，用户反馈 exe 图标/浅色皮肤/拖动/警告）

### 1) exe 没有图标、任务管理器也不显示

- 根因：本机 MSVC 环境缺少 `rc.exe`，原 `build.rs` 的资源嵌入静默失败（只有旧图标残留）。
- 方案：删除 `build.rs` 与 `embed-resource` 依赖；新增 `patch-icon` 小工具（`src/bin/patch_icon.rs`），
  在 cargo 构建后用 Win32 `BeginUpdateResourceW/UpdateResourceW/EndUpdateResourceW` 把
  `assets/icon.ico` 的 9 个尺寸写成 `RT_GROUP_ICON + RT_ICON` 资源。
- 构建入口：`tools/build-release.ps1`（cargo build + patch-icon 一步完成）。
- 验证：`ExtractAssociatedIcon(clipnest.exe)` 返回 32×32 **新剪贴板图标**（旧图标是白底文件），
  资源管理器/任务管理器读取的就是这个资源。

### 2) 新增浅色皮肤

- `Settings` 新增 `theme: ThemeMode{Dark,Light}`（默认深色，旧配置兼容）；
  `theme.rs` 从静态深色常量重构为**可切换的动态色板**（深色/浅色全套语义色、阴影、描边、卡片色）。
- 设置页新增「外观 → 皮肤」深色/浅色切换（保存后立即切换，不需要重启）；
  切换时同步重装 egui Visuals、重设 Acrylic 色调与 DWM 深浅标题栏属性。
- 验证：debug F6 热切换截图（深→浅）、设置落盘后重启仍为浅色；
  截图见 `docs/screenshots/picker-light.png`、`manager-settings-light.png`。
- 浅色同样依赖 Windows「透明效果」开启（关闭则只有纯色浅底）。

### 3) 拖动面板

- 候选窗顶部新增 16px 拖拽条（悬停显示三个抓取点，按住任意位置可拖动），搜索框仍正常输入；
- 管理台标题栏空白区域（右侧窗口按钮除外）按住可拖动。
- 验证：自动化鼠标拖动后窗口从 (970,416) → (1090,476)；管理台 (750,346) → (850,386)。

### 4) 清理 never-use 代码 / 消除编译警告

- 删除已废弃的 `src/sys/winv.rs`（Win+V 钩子整套实现）、`WinVMode/WinVStatus` 类型、
  `Event::WinVStatus`、SysCmd 的 winv 字段、相关 UI 状态与预览赋值。
- 删除未使用方法/常量：`ClipKind::icon`、`ClipItem::is_pasteable`、`HotkeyChord::VK_WIN_L`、
  `Event::ClipsChanged.total`；`ClipItem.created_at/source_title` 改为在详情面板展示（不再 dead）；
  `ThemeMode::label` 在设置页使用。
- 顺带修复测试暴露的排序问题：`bump_used` 改为单调时间戳（同一毫秒连续粘贴也能置顶）。
- 结果：`cargo build --release` 与 `cargo test` **0 warnings**；142 tests passed / 0 failed；
  `--selftest` 14/14 PASS；`--diagnose` OK。


---

## 10. v1.5.0（2026-11-02，面板视觉打磨）

### 磨砂透明度可调

- `Settings.glass_opacity`（35–100，默认 75）；设置页「外观 → 磨砂透明度」滑杆，**拖动实时预览**、
  保存后持久化（`UiAction::PreviewGlassOpacity` 只改运行时，不落盘）。
- 窗口/面板/卡片 alpha 与 Acrylic 叠色都随数值联动：数值越低越透明，背景模糊越明显（终端式观感）。
- 验证：F5 循环 75→55→35，截图 `picker-transparent.png` 可见背景透出明显增强。

### 面板跟随光标 + 贴边翻转

- `picker_position` 改为光标定位：水平以光标为中心并夹在工作区；垂直优先光标下方；
  下方放不下则整体上翻（底边贴近光标上方），上方也放不下则贴住工作区顶部，任何情况下不越界。
- 实测：光标 y=1280（靠下）→ 面板 y=238..798（在光标上方）；光标 y=40（靠上）→ 面板 y=54..614（在光标下方）。

### 视觉细节

- 窗口顶部增加轻微玻璃高光 + 向下渐隐（深色白色叠加/浅色黑色叠加），面板更有厚度感；
- 顶部拖拽条抓取点常驻显示（悬停加亮），可拖动一眼可见；
- 调整透明度时取消重复的“暂停记录”toast，交互更安静。

### 校验

- `cargo build --release` / `cargo test`：**0 warnings**；142 tests passed；
  `--selftest` PASS；`--diagnose` OK；release exe 8.91MB（含 9 尺寸图标资源）。


---

## 11. v1.6.0（2026-11-02，图标/并发窗口/粘贴速度）

### 图标统一

- exe 图标资源继续由 `patch-icon` 写入（9 个尺寸，RT_GROUP_ICON + RT_ICON）；
- 主候选窗和管理台窗口都通过 `ViewportBuilder::with_icon` 设置内嵌 PNG 图标
  （之前主窗口是 eframe 默认的 “e” 图标，任务管理器/任务栏看不到 ClipNest 图标）；
- 验证：对主窗口/管理台窗口查询 `WM_GETICON`，small icon 均非 0；exe `ExtractAssociatedIcon` 为 32×32 新图标。
- 如资源管理器仍显示旧图标，是 Windows 图标缓存，F5/重启 Explorer 即可。

### 管理台独立窗口（主界面不隐藏）

- 主进程保持“候选窗”单窗口；打开管理台时 spawn `clipnest.exe --ui-manager --page X --target HWND` 子进程。
- 子进程只启动 Store（共享 SQLite），不启动 Sys（托盘/快捷键仍在主进程），因此：
  - 管理台打开时按快捷键，候选窗在主窗口出现，管理台窗口保持可见；
  - 候选窗关闭/粘贴后管理台仍在；
  - 管理台关闭不影响主进程。
- 管理台粘贴目标：主进程把最后的外部前台窗口作为 `target` 传给子进程，子进程直接写剪贴板 + `restore_and_paste`；
  粘贴目标排除所有 ClipNest 进程（主/子），避免把管理台自己当成目标。
- 管理台保存设置后通过 WM_COPYDATA(SETTINGS_MAGIC, JSON) 通知主进程立即应用（主题/透明度/快捷键/暂停），
  不依赖 settings.json 轮询。
- 托盘“管理历史/设置”会复用同页面窗口；不同页面则关闭旧的再开新的；主进程退出时连带关闭管理台。
- 验证：主进程 + 子进程共存（进程数=2），管理台标题 `ClipNest · 剪贴匣 · 管理 · history`，直接主进程退出后两个进程均退出。

### 粘贴延迟

- 原流程固定等待：面板移出屏幕 120ms + 恢复前台固定 60ms + SendInput；
- 现在：20ms 移出等待 + 前台就绪即发送（最多 60ms 轮询，通常 0–10ms），目标拿到前台立刻 SendInput。
- 新增日志 `粘贴：载荷到达` / `自动粘贴：开始` / `自动粘贴完成` 便于定位耗时。

### 校验

- `cargo test` 142 passed / 0 failed；`cargo build --release` 0 warnings；`--selftest` PASS。


---

## 12. v1.7.0（2026-11-02，主界面不磨砂/循环选择/底部提示可读）

- **管理台不磨砂**：新增 `theme::set_glass_enabled(bool)`；管理台子窗口调用 `set_glass_enabled(false)`，
  窗口底/面板/卡片全部不透明，不再调用 `apply_backdrop`（无 Acrylic）；候选窗仍保持磨砂。
  透明度设置只影响候选窗。
- **列表循环**：`keys::move_selection` 单步 `↓` 在最后一条回绕到第一条，`↑` 在第一条回绕到末尾；
  避免边界重复设置滚动位置造成的闪烁；更新了 `keys`/`picker` 单元测试。
- **底部提示条可读性**：候选窗 hint bar 增加 panel 底色 + 圆角，文字从 `text_weak` 提升为 `text_secondary`；
  浅色/白底/高透明下也能看清。
- 校验：`cargo test` 142 passed；`cargo build` 0 warnings；截图确认 hint bar 底色与文字对比。

---

## 13. v1.7.2（2026-11-02，白底/浅色桌面可读性 + 终端磨砂透明）

### 用户反馈
- 候选窗叠在白色/浅色窗口上时，整块面板被透成浅灰，白字看不清（v1.7.1 的内容区不透明底又让磨砂透明感消失，用户要求保留终端式透明）。

### 根因
- 磨砂滑杆最低 35% 时，Acrylic 深色 tint alpha 只有 70/255；叠上 window_bg 后仍被白色背景冲成浅灰，白字对比度不足。
- 单纯给内容区加不透明底会破坏终端式透光，不可取。

### 修复
- 去掉内容区不透明底，恢复整窗磨砂透光；src/window.rs::apply_backdrop 的 Acrylic tint alpha 改为
  150 + (pct-35)*60/65（35% 时仍保留 150/255 深色 tint，100% 时 210/255），白底上文字可读且背景模糊仍可见。
- 深色主题辅助文本提亮：text_secondary #8B98A9 → #A5B2C4、text_weak #5C6878 → #7F8C9E；行时间、搜索占位、空状态/加载文案提升为次级文字。
- 设置页磨砂说明改为「终端式 Acrylic 磨砂；深色 tint 有下限，白底/亮色窗口上文字仍可读」。
- 主题测试用 TEST_LOCK: Mutex<()> 串行化全局状态，修复并行测试偶发失败。

### 校验
- cargo test --release 142 passed / 0 failed（连续 5 次全绿）；cargo build --release 0 warnings；--selftest PASS。
- 透明感与可读性为互斥目标，最终采用「Acrylic 深色 tint 下限」而非不透明内容底；需用户在真实桌面（白色/彩色背景）下目检确认。

---

## 14. v1.7.3（2026-11-02，候选窗键盘移动闪烁）

### 用户反馈
- 从最后一个列表项按 ↑ 移动到上一条时，候选窗会闪一下。

### 根因
- 键盘导航每次按键都把选中行重新居中滚动（`pending_scroll_row` + `vertical_scroll_offset`），在列表底部附近容易产生视口跳动。
- 列表选中高亮使用 80ms `animate_bool_with_time`，移动时旧行淡出/新行淡入，会额外放大闪烁感。

### 修复
- `src/ui/picker.rs` 新增 `scroll_offset_for_row()`：选中行仍在视口内时保持原滚动偏移，只有超出视口才做最小滚动；`UiState` 保存上一帧 `picker_scroll_offset`。
- `src/ui/row.rs` 选中态改为瞬时切换（不再对 selection 做淡入淡出），hover 动画保留。

### 校验
- `cargo test --release` **143 passed / 0 failed**；新增 `keyboard_scroll_is_minimal_and_stable_at_bottom` 覆盖「最后一条 → 上一条不滚动」与回绕到顶部。
- `cargo build --release` 0 warnings；`--selftest` PASS。

---

## 15. v1.8.0（2026-11-02，克制的动效）

### 用户反馈
- 使用过程中「没有任何动效」，希望加一点不夸张、兼顾实用和高级感的动效。

### 实现
- **候选窗出现**：`UiState.picker_appear_at` + `src/ui/picker.rs` 的 160ms ease-out 内容淡入（opacity 0.62 → 1.0，只改内容透明度，不移动窗口、不影响磨砂底与粘贴时延）；淡入期间主动 `request_repaint()`。
- **Toast**：`src/ui/widgets.rs::paint_toast` 淡入/淡出时上浮 6px，避免凭空出现。
- **按钮**：`button_at` 增加 90ms hover 插值；主按钮 accent→accent_hover、次按钮 card→card_hover、危险按钮淡红、幽灵按钮背景淡入。
- **筛选条**：`filter_chips` 选中 120ms、hover 90ms 插值（底色/描边/文字一起过渡）。
- **搜索框**：聚焦 120ms 过渡（底色、描边、放大镜图标渐变为强调色）。
- **管理台导航**：当前页 120ms 选中过渡、hover 90ms；切换页面不再硬跳。
- 未加窗口飞入/缩放/列表滚动动画：这些在常驻小面板上容易显慢或与粘贴路径冲突，保持「快」优先。

### 校验
- `cargo test --release` **143 passed / 0 failed / 1 ignored**；`cargo build --release` 0 warnings；`--selftest` PASS。
- 动效均为纯 UI 绘制层，不影响剪贴板监听、粘贴时序与 SQLite 路径。

---

## 16. v1.8.1（2026-11-02，设置页固定保存栏）

### 用户反馈
- 设置项改完后，保存按钮在最底部，必须滚到最后才能点，使用不便。

### 实现
- `src/ui/pages/settings.rs` 把设置页拆成两块：上方 `ScrollArea` 放所有设置卡片，下方 64px 高的**固定操作栏**始终可见。
- 固定栏使用卡片底色 + 边框，左侧「保存设置 / 放弃修改」，右侧始终显示「● 有未保存的修改」或「设置已保存」；不再依赖滚动位置。
- 内容区底边预留 12px 间距，滚动内容不会钻到固定栏下面；设置页顶部提示文案同步改为「底部固定栏」。
- 新增 `split_page()` 纯函数与 `sticky_footer_splits_content_without_overlap` 单测，锁定固定栏高度、底部对齐、内容不重叠。

### 校验
- `cargo test --release` **144 passed / 0 failed / 1 ignored**；`cargo build --release` 0 warnings；`--selftest` PASS。
- 截图：`target/settings_sticky.png`（顶部）与 `target/settings_scrolled.png`（滚动到底部后保存栏仍固定）；已更新 `docs/screenshots/manager-settings.png` / `manager-settings-scrolled.png`。


---

## 17. v1.8.2（2026-11-02，管理台三处小修）

### 用户反馈
- 管理台左侧「收藏/设置」旁边的空白处有一个小方框，选中「设置」时又看不到。
- 「容量与清理」只有上限和占用，没有可以主动点的清理按钮。
- 「磨砂透明度」看不到滑轨，但拖动数值会变化。

### 根因与修复
- **幽灵方框**：`src/ui/pages/history.rs::table` 里调用 `widgets::checkbox(ui, &mut header_all)` 只是为了拿交互语义，真正的表头复选框另用 painter 画在列位置；旧调用会按当前布局游标分配一个 16px 控件，落到页面左侧（窗口坐标约 x=4..17, y=139..153）。设置页没有表格，所以不出现；选中设置时被高亮行盖住。新增 `widgets::checkbox_at(ui, rect, id, checked)`，在指定矩形内绘制并交互、**不推进布局游标**；表头直接使用它并处理全选，删除旧的 check_zone 交互。新增 `checkbox_at_does_not_advance_layout_cursor` 回归测试。
- **清理按钮**：`capacity_section` 原本只有上限与占用。新增「立即清理」按钮，点击后 push `UiAction::RefreshStats`（Store 的 RefreshStats 分支会执行 `retention::prune`），并提示“正在清理并刷新占用…”。文案说明按**已保存**的上限执行，收藏内容与收藏文件实体不会被删除。
- **滑轨不可见**：egui 0.32 的 `Slider` 用 `visuals.widgets.inactive.bg_fill` 画滑轨，主题里该值等于 `card()`，和设置卡片同色。现在在滑块外包 `ui.scope`，把 `inactive.bg_fill` 改为 `theme::border()`，hovered/active 与 `selection.bg_fill` 用强调色，并开启 `slider_trailing_fill`，滑轨和已选段都可见。

### 校验
- `cargo test --release` **145 passed / 0 failed / 1 ignored**；`cargo build --release` 0 warnings；`--selftest` PASS。
- 截图：`target/mgr_fixed_fav.png` / `target/mgr_fixed_history.png`（方框消失）、`target/settings_fixed.png`（滑轨可见）、`target/settings_fixed_cleanup.png`（立即清理按钮）。


---

## 18. v1.9.0（2026-11-02，回收站：手动删除保留 2 天）

### 用户需求
- 手动删除的记录进入回收站，保留 2 天。

### 设计决策（已与用户确认）
- 仅**手动单条/批量删除**进入回收站；容量自动淘汰与「清空历史」保持永久删除。
- 回收站保留 **2 天**，期间支持**恢复 / 彻底删除 / 清空回收站**；条目继续占用磁盘。
- 重新复制回收站中的相同内容时，自动从回收站恢复。
- 「清空历史」仍永久删除，并包含回收站（确认文案已明确）。

### 实现
- **Schema v2**：`clips` 新增 `deleted_at INTEGER NULL`；`src/store/schema.rs::ensure_trash_schema()` 对旧库做幂等 `ALTER TABLE`，并创建 `idx_clips_trash` 部分索引；`SCHEMA_VERSION = 2`。
- **删除语义**：`Db::soft_delete()` 只标记 `deleted_at`；`Db::restore()` 清除标记并把 `last_used_at` 推到历史顶部；`Db::purge()` 只允许删除回收站行并释放 blob；`Db::empty_trash()`、`Db::purge_expired_trash()` 负责清空与到期清理。保留期常量 `TRASH_RETENTION_MS = 2 * 24 * 60 * 60 * 1000`。
- **查询隔离**：`SearchQuery.trash` 控制 active / trash 范围；active 查询统一追加 `deleted_at IS NULL`，回收站按 `deleted_at DESC` 排序；`item()`、`paste_payload()`、`counts()`、`bump_used()` 等只作用于 active 行。
- **自动恢复**：`upsert`/`import_upsert` 命中已删除 hash 时调用 `restore`，再按正常去重/使用计数处理。
- **到期清理**：`retention::prune()` 开头调用 `purge_expired_trash`，启动、刷新统计与定期 prune 都会执行；`PruneReport.purged_trash` 记录数量。
- **Store 命令**：`Delete` 改为 `soft_delete`；新增 `Restore / Purge / EmptyTrash`，成功后触发 dirty + stats。
- **管理台**：`ManagerPage::Trash` + 新页 `src/ui/pages/trash.rs`；导航图标、搜索、清空按钮、每行「恢复 / 彻底删除」、删除时间与剩余时间、二次确认；`Ctrl+F` 搜索、`Delete` 彻底删除（确认）。
- **UI 修复（实测发现）**：`widgets::button_at()` 的 egui id 原来只用按钮文案，回收站每行同名「恢复/彻底删除」会互相吞点击；改为把按钮坐标纳入 id。回收站列表遍历时执行恢复/删除会修改 `state.items` 导致越界 panic；改为先克隆列表再绘制。

### 校验
- `cargo test --release` **149 passed / 0 failed / 1 ignored**；新增：
  - `schema_v1_migrates_trash_column`
  - `trash_soft_delete_search_restore_and_purge`
  - `trashed_hash_upsert_restores_and_empty_trash_works`
  - `expired_trash_is_purged_after_two_days`
- `cargo build --release` 0 warnings；`--selftest` PASS。
- 实测：管理台删除 → 回收站出现；恢复后行消失、历史恢复；彻底删除/清空确认可执行；2 天到期保留逻辑由单测覆盖。
- 截图：`docs/screenshots/manager-trash.png`。


---

## 19. v1.9.1（2026-11-02，候选窗列表序号）

### 用户需求
- 粘贴列表中显示序号，而不是数据库 id。

### 实现
- `src/ui/row.rs::ClipRow` 新增 `ordinal: usize`；`clip_row` 在行左侧绘制 1 起始的序号，使用等宽字体，选中行用强调色、普通行用次文本色。
- 类型图标/缩略图与标题区整体右移 28px，为序号留出固定列；序号按当前查询/筛选后的列表位置计算，不读取 `ClipItem.id`。
- `src/ui/picker.rs` 传给行组件 `ordinal: i + 1`，滚动、筛选、搜索后序号随可见结果重新排序。

### 校验
- `cargo test --release` **149 passed / 0 failed / 1 ignored**；`cargo build --release` 0 warnings；`--selftest` PASS。
- 截图：`target/ui-picker.png`，已更新 `docs/screenshots/picker.png`。


---

## 20. v1.9.2（2026-11-02，←/→ 切换分类）

### 用户需求
- 粘贴列表支持左右移动，切换 tab。

### 实现
- `src/ui/keys.rs`：新增 ←/→ 映射为 `FilterPrev` / `FilterNext`，与 Tab / Shift+Tab 行为一致；`route_picker_keys` 增加 `allow_arrows` 参数。
- 搜索框有文字且聚焦时 `allow_arrows=false`，←/→ 留给 TextEdit 移动光标；搜索框为空或未聚焦时切换分类，符合输入与导航的直觉。
- `src/ui/picker.rs` 传入 `allow_arrows`；底栏提示改为「←→ 切分类」。
- 新增 `route_left_right_switch_filters_only_when_allowed` 单测，覆盖允许/禁止两条路径。

### 校验
- `cargo test --release` **150 passed / 0 failed / 1 ignored**；`cargo build --release` 0 warnings；`--selftest` PASS。
- 截图：`target/ui-picker.png`，已更新 `docs/screenshots/picker.png`。


---

## 21. v1.9.3（2026-11-02，管理台投影闪烁）

### 用户反馈
- 从托盘点击设置时，有个影子从左边一闪，然后界面显示在右边；关闭时界面消失后也会留下影子闪烁。

### 根因
- 管理台窗口创建时默认可见，winit 先在系统默认位置显示一帧，随后首帧才 `OuterPosition` 到光标附近；透明窗口的 DWM 投影就在旧位置闪一下。
- 关闭时直接销毁透明窗口，DWM 投影可能残留一帧。

### 修复
- `src/manager_window.rs`：创建时放到屏幕外 `[-30000, -30000]`，`with_active(false)`；首帧拿到真实 DPI 后用 `picker_position` 定位并聚焦。
- `src/window.rs`：`request_close_window` 先把窗口移到屏幕外再发 `WM_CLOSE`；不先 `SW_HIDE`，避免隐藏后 winit 不处理关闭消息、管理台进程残留。
- 实测：窗口启动时先出现在屏幕外，再出现在光标附近；点击关闭按钮后进程正常退出。

### 校验
- `cargo test --release` **150 passed / 0 failed / 1 ignored**；`cargo build --release` 0 warnings；`--selftest` PASS。

---

## 22. v1.9.4（2026-11-04，候选窗选中色改为青绿）

### 用户反馈
- 候选窗选中的蓝色在深色主题下文字看着不舒服，浅色主题也很一般；希望换一个视觉合理的颜色。

### 设计
- 将「操作蓝」与「选中色」拆开：搜索焦点、主按钮继续使用蓝色；候选窗选中态改用玉石青绿，避免整屏蓝色发闷。
- 深色主题：选中行半透明青绿 + 亮青绿竖条/序号，行文字保持主题白色；选中过滤胶囊使用深青绿实底 + 白字。
- 浅色主题：使用同色相的深青绿（`#0F766E`），选中行为浅青绿底 + 深色文字，胶囊仍为深青绿底白字。
- 选中底色保持半透明（alpha 48），不破坏 Acrylic 磨砂质感；不修改行标题/预览文字主色，避免对比度下降。

### 实现
- `src/ui/theme.rs`：`Palette` 新增 `selection` / `selection_bar`；新增 `selection()`、`selection_bar()`、`selection_text()`，`selection_fill()` 由蓝色改为青绿 48 alpha；深色竖条 `#2DD4BF`，浅色竖条 `#0F766E`。
- `src/ui/row.rs`：选中行背景、左侧竖条、序号改用 `selection_*`。
- `src/ui/widgets.rs::filter_chips`：选中胶囊的填充、描边、文字改用 `selection_*`。
- 更新色板/alpha 单测断言。

### 校验
- `cargo test --release` **150 passed / 0 failed / 1 ignored**；`cargo build --release` 0 warnings；`--selftest` PASS。
- 截图：`docs/screenshots/picker.png`（深色）、`docs/screenshots/picker-light.png`（浅色）。

---

## 23. v1.9.5（2026-11-04，收藏与置顶解耦）

### 用户反馈
- 收藏大量内容后，因为收藏会自动置顶，想找刚复制的内容很不方便。
- 希望收藏列表能单独支持置顶。
- 希望收藏过滤放在最左边；默认仍选「全部」。

### 根因与设计
- 旧实现把「收藏」和「置顶」合并为同一个 `pinned` 标记：点星即置顶，历史、候选窗、收藏页共用同一排序，导致收藏一多就把新内容挤出视野。
- 拆成两个独立标记：`favorited` 只表示收藏，`pinned` 只表示收藏列表内的置顶；`pinned` 仅在收藏页排序时生效。
- 过滤顺序调整为 收藏 → 全部 → 文本 → 图片 → 文件，进入候选窗仍默认「全部」。
- SQLite schema v1/v2 → v3：新增 `clips.favorited`，迁移时把旧 `pinned=1` 转为 `favorited=1` 并清除旧的自动置顶；以迁移前版本号判断，失败重试也安全。

### 实现
- `src/model.rs`：schema v3、`ClipItem.favorited`、`ClipFilter` 顺序、`StoreCmd::SetFavorited`。
- `src/store/schema.rs`：`ensure_favorite_schema` 补列/迁移/索引。
- `src/store/query.rs`：历史按 `last_used_at DESC`；收藏页按 `pinned DESC, last_used_at DESC`；收藏过滤 `favorited=1`。
- `src/store/db.rs`：`set_favorited` 取消收藏时同时取消置顶；`set_pinned` 只允许收藏项。
- `src/store/retention.rs`：自动清理保护 `favorited`，不再保护 `pinned`。
- `src/store/export.rs`：导出格式 v2 区分收藏与置顶，兼容旧版导入。
- UI：星标只收藏；收藏页行内新增独立置顶图标，右键菜单与批量/详情操作区分收藏/置顶；候选窗 `Ctrl+P` 只收藏，收藏过滤下才显示置顶菜单。
- 管理台历史页也提供「收藏」过滤，置顶按钮只在收藏上下文中出现。

### 校验
- `cargo test --release` **151 passed / 0 failed / 1 ignored**；新增迁移与收藏/置顶互斥测试。
- 截图：`docs/screenshots/picker.png`（收藏在最左、默认全部）、`docs/screenshots/manager-history.png`（历史页收藏过滤）、`docs/screenshots/manager-favorites.png`（收藏页星标 + 独立置顶）。

---

## 24. v1.9.6（2026-11-04，Toast / 置顶按钮 / 输入法 / 配色）

### 用户反馈
- 删除后出现的 Toast 文字看不清，全是黑色。
- 收藏列表的置顶/取消置顶需要可见按钮，不想每次都右键。
- 中文输入法还在输入拼音、尚未按空格上屏时就触发搜索；回车还会触发粘贴。
- 希望选中强调色可自选，默认提供 3 种通用配色（当前玉石青绿）。

### 根因与设计
- Toast 背景固定为深色，但文字使用主题正文色；浅色主题正文是深色，形成黑字压深底。
- 收藏页只有行内小图标和右键菜单，缺少工具栏级可发现入口。
- 候选窗在搜索框绘制前就路由键盘，Enter 总被当作“粘贴”；同时输入法 Preedit 变化直接触发了搜索。
- 候选强调色写死在主题色板里，没有用户设置入口。

### 实现
- `src/ui/widgets.rs`：新增 `toast_text_color()`，Toast 始终使用固定浅色文字（`#F4F7FB`），不再跟随主题正文色；补充主题无关的对比度单测。
- `src/ui/pages/history.rs`：收藏页工具栏在「显示/隐藏详情」旁新增「置顶 / 取消置顶」按钮，仅选中收藏项时出现；行内置顶图标与右键菜单保留。
- `src/ui.rs`：`UiState` 新增 `ime_composing`。
- `src/ui/picker.rs`：路由键盘前读取 `egui::Event::Ime`（Preedit/Commit/Enabled/Disabled），组合期间把 `allow_enter=false`，并暂停搜索刷新；上屏/提交后再搜索。
- `src/ui/keys.rs`：`route_picker_keys(..., allow_enter)`，IME 组合时不再消费 Enter/Ctrl+Enter、Delete 或方向键。
- `src/model.rs`：新增 `SelectionPreset`（玉石青绿默认 / 靛蓝 / 琥珀）与 `Settings.selection_preset`，旧 settings.json 缺字段时默认玉石青绿。
- `src/ui/theme.rs`：选中色改为运行时预设，深色竖条与浅色竖条分别按预设取色；新增预设单测。
- `src/ui/pages/settings.rs`：外观卡片新增「选中配色」，保存后立即生效；启动与设置保存路径都会应用预设。

### 校验
- `cargo test --release` **154 passed / 0 failed / 1 ignored**；`cargo build --release` 0 warnings。
- 视觉：`docs/screenshots/picker-toast-light.png`（浅色主题下 Toast 文字为浅色）、`docs/screenshots/manager-settings.png` / `manager-settings-light.png`（三种选中配色）、`docs/screenshots/manager-favorites.png`（收藏页工具栏「置顶 / 取消置顶」按钮）。
- 截图：`docs/screenshots/manager-settings.png`、`manager-settings-light.png`、`manager-favorites.png`、`picker.png` 更新为 v1.9.6。


---

## 25. v1.9.7（2026-11-04，置顶底色 / 置顶按钮 / 输入法焦点）

### 用户反馈
- 中文输入法回车问题解决了，但回车后搜索框没有焦点，无法继续输入。
- 收藏列表看不到「置顶 / 取消置顶」按钮。
- 置顶内容需要特殊背景色标记。

### 根因与修复
- 输入法 Commit 的同一帧，egui 可能因 Enter 让搜索框失焦；在 `src/ui/picker.rs` 检测到 IME 事件的帧主动 `request_focus()`，并把 `search_focus` 延续一帧，确保上屏后仍可继续输入。
- 收藏页首次进入时 `selected` 可能为空，工具栏置顶按钮按“选中项”条件渲染而消失。`src/ui.rs::enter_manager` / `go_manager_page` 现在为列表页默认选中首行；`src/ui/pages/history.rs` 详情面板新增独立「置顶此条 / 取消置顶」按钮，工具栏按钮保留。
- 置顶缺少视觉标记。`src/ui/theme.rs` 新增 `pinned_fill()`（金色 38 alpha）与 `pinned_fill_strong()`（金色 72 alpha）；`history.rs` 置顶行始终叠金色底，选中时用更强金色并保留金色左侧竖条；行内置顶图标置顶时改为金色。

### 校验
- `cargo test --release`：**155 passed / 0 failed / 1 ignored**。
- 截图：`docs/screenshots/manager-favorites.png`（浅色主题，置顶行金色底 + 工具栏/详情面板置顶按钮）。


---

## 26. v1.9.8（2026-11-04，剪贴板置顶 / 窄窗口布局）

### 用户反馈
- 剪贴板面板也要置顶按钮、置顶内容特殊背景。
- 管理台截图红框：表头「内容摘要 / 来源」重叠，详情来源标题被右侧裁切，正文区域空白。

### 修复
- `src/ui/row.rs`：剪贴板行新增星标左侧的置顶图钉（仅收藏项显示）；置顶行使用金色背景，选中时用更强金色，左侧竖条与序号同步改为金色/高对比文字；置顶行的时间与副标题提升为正文色，避免金色底上发灰。
- `src/ui/picker.rs`：处理图钉点击，右键菜单在所有筛选分类下都可对收藏项置顶/取消置顶。
- `src/ui/pages/history.rs`：
  - `Columns` 增加窄屏/极窄屏两级布局：宽度 <620px 隐藏「来源」列并压缩类型/时间；<400px 再隐藏「大小」列，摘要列保留可读宽度。表头改为按列宽省略，不再互相压字。
  - 详情面板两行元信息改用 `paint_elided` 按面板宽度省略。
  - 文本正文为空但摘要存在时回退显示摘要，避免空白详情区。

### 校验
- `cargo test --release`：**158 passed / 0 failed / 1 ignored**。
- 窄窗口截图：`docs/screenshots/manager-narrow.png`（885×558，表头与内容无重叠，详情无裁切）。
- 剪贴板截图：`docs/screenshots/picker.png`（置顶行金色底 + 图钉/星标）。


---

## 27. v1.9.9（2026-11-04，置顶金色标记范围收敛）

### 用户反馈
- 只有收藏列表需要置顶特殊颜色，其他列表不用。

### 修复
- `src/ui/row.rs` 新增 `highlight_pinned`，仅当剪贴板当前筛选为「收藏」时才用金色底、金色竖条和高对比文字标记置顶行；其它分类保持普通选中/hover 样式。
- `src/ui/pages/history.rs` 的金色置顶底也只在 `pin_mode`（收藏页或「收藏」筛选）时绘制；普通历史页选中置顶行仍使用正常选中色。
- 置顶图钉按钮本身仍对所有已收藏条目显示，功能不受影响。

### 校验
- `cargo test --release`：**158 passed / 0 failed / 1 ignored**。
- 剪贴板「全部」列表截图：`docs/screenshots/picker.png`（置顶行不再金色，仅保留图钉/星标）。


---

## 28. v1.9.10（2026-11-04，关闭按钮图标修复）

### 用户反馈
- 管理台标题栏右侧出现两个「—」，看起来像两个最小化按钮；实际第二个是关闭按钮。

### 根因
- `src/ui/paint.rs::paint_close` 在已 `shrink(16)` 的 12×8 图标区内，又按 `width()*0.32` 做了一次 shrink，导致图标框只剩约 4×0.3px，两条对角线几乎水平，渲染成短横线。
- 高 DPI 或窗口较窄时更明显。

### 修复
- `paint_close` 改为在中心取正方形图标框（`min(width,height)`），再小幅内缩，保证 ✕ 始终是清晰的对角交叉线。
- `src/ui/manager.rs` 关闭图标绘制区域由 `close_rect.shrink(16)` 调整为 `shrink(12)`，图标更大更易辨认。
- 新增 `close_icon_is_square_not_flat` 回归测试，断言关闭图标框为正方形且不小于 10px。

### 校验
- `cargo test --release`：**159 passed / 0 failed / 1 ignored**。
- 标题栏截图：最小化为「—」、关闭为清晰「✕」。


---

## 29. v1.9.11（2026-11-05，时间范围筛选）

### 用户反馈
- 希望在搜索框下面加入时间范围可选项，按时间展示列表。

### 实现
- 新增 `TimeRange`：全部时间 / 今天 / 近7天 / 近30天 / 近90天，默认全部时间。
- 候选窗在分类胶囊下方新增时间范围胶囊行；管理台历史/收藏/回收站在搜索框下方新增同样一行。
- 时间条件按 `last_used_at` 过滤，与关键词、分类、收藏、回收站条件叠加；翻页与总数都使用同一条件。
- 进入候选窗或切换管理台页面时重置为「全部时间」，避免筛选状态跨页面残留。
- SQL 使用 `clips.last_used_at >= ?`，复用现有的 `idx_clips_order(last_used_at DESC)` 索引。

### 校验
- `cargo test --release`：**162 passed / 0 failed / 1 ignored**
  - `time_range_filters_by_last_used`
  - `time_range_cutoffs_are_ordered`
  - `entering_picker_or_manager_resets_time_range`
- 截图：`docs/screenshots/picker.png`、`docs/screenshots/manager-history.png` 均包含时间范围胶囊行。


---

## 30. v1.9.12（2026-11-05，指定日期筛选与大列表滚动优化）

### 用户反馈
- 希望时间范围再提供一个可以选择任意一天的组件。
- 列表数据量很大时，是否还能继续优化。

### 实现
- `TimeRange` 新增 `Custom`（「指定日期」）；候选窗和管理台的时间胶囊行最后增加该选项。
- 选择「指定日期」后打开月历弹窗：可切换上/下月、点选任意一天、点「今天」回到当天，或取消；选中后时间行下方显示 `YYYY-MM-DD` 按钮，可再次点开修改。
- 新增强类型 `SearchQuery.custom_day`，SQL 按本地自然日左闭右开区间过滤：`last_used_at >= 当天 00:00` 且 `last_used_at < 次日 00:00`，与关键词、分类、收藏、回收站条件叠加。
- 日历使用 egui `Modal`；弹窗打开时禁用候选窗键盘路由，避免回车误粘贴；关闭后立即重新搜索。
- 大列表滚动加载：`UiState::apply_search_page` 追加页时，原来每个条目都用 `items.iter().any(...)` 全量查重，连续滚动会退化为 O(n²)；改为维护持久 `loaded_ids: HashSet<ClipId>`，追加页 O(1) 判重。
- 列表本身的虚拟化与分页保持不变：候选窗每页 200、管理台每页 500，`ScrollArea::show_rows` 只绘制可见行；数据量大时不会一次性渲染全部条目。

### 校验
- `cargo test --release`：**163 passed / 0 failed / 1 ignored**
  - `time_range_filters_by_last_used` 增加「今天命中、昨天不命中」的自定义日期断言。
  - `entering_picker_or_manager_resets_time_range` 增加自定义日期清理断言。
- 截图：`docs/screenshots/picker.png`（指定日期按钮）、`docs/screenshots/picker-date.png`（日历弹窗）、`docs/screenshots/manager-history.png`、`docs/screenshots/manager-date.png`。

---

## 31. v1.9.13（2026-11-05，日期范围与筛选开关）

### 用户反馈
- 日期选择现在只能选某一天，需要支持一个范围。
- 新增的日期行默认隐藏，给出一个筛选控制开关。

### 实现
- `TimeRange::Custom` 从单日改为日期范围：`custom_day` 为起始日、`custom_end_day` 为结束日（均本地 00:00，包含结束日）；起止顺序颠倒时自动交换，单日时起止相同。
- SQL 使用左闭右开区间：`last_used_at >= 起始日 00:00` 且 `last_used_at < 结束日次日 00:00`，与关键词、分类、收藏、回收站等条件继续叠加。
- 日历弹窗改为范围选择：第一次点选开始日期，第二次点选结束日期并应用；起止日期用实色胶囊、区间内日期用浅色底；顶部提示「先点开始日期，再点结束日期 / 再点结束日期 / 点选新的开始日期」；「今天」选择当天单日，「取消」关闭。
- 候选窗和管理台的时间/日期行默认收起，只保留「筛选」按钮；展开后显示时间胶囊与日期范围按钮，收起时若有生效中的时间筛选，按钮显示青色小圆点。
- 日期范围按钮显示 `YYYY-MM-DD`（单日）或 `YYYY-MM-DD ~ YYYY-MM-DD`（范围），点击可重新打开日历修改。

### 校验
- `cargo test --release`：**163 passed / 0 failed / 1 ignored**
  - `time_range_filters_by_last_used` 增加日期范围命中、起止倒置自动交换断言；
  - `entering_picker_or_manager_resets_time_range` 增加结束日期与筛选展开状态清理断言。
- 截图：`docs/screenshots/picker.png`（默认收起）、`docs/screenshots/picker-date.png`（范围日历）、`docs/screenshots/manager-history.png`、`docs/screenshots/manager-date.png`。

---

## 32. v1.9.14（2026-11-05，详情面板提示清理）

### 用户反馈
- 管理台详情面板底部的「只读预览 · 可选中复制」一行字让整体 UI 不协调。

### 修复
- 删除 `src/ui/pages/history.rs::detail_panel` 中文本详情内容区下方的该提示，详情区只保留正文与滚动。
- 不改变只读预览/选中复制行为。

### 校验
- `cargo test --release`：**163 passed / 0 failed / 1 ignored**。
- 截图：`docs/screenshots/manager-history.png` 更新为无底部提示的详情面板。


## 33. v1.9.15（2026-11-05，导出另存为与数据路径确认）

### 用户反馈
- 数据目录显示为 `C:\Users\<用户名>\AppData\Roaming\ClipNest`，开源项目是否动态、是否会泄露本机用户名？
- 点击「导出」没有明显效果。

### 实现与结论
- 数据目录由 `src/config.rs::data_dir()` 在运行时读取 `%APPDATA%` 环境变量后拼接 `ClipNest`；也可用 `CLIPNEST_DATA_DIR` 覆盖。仓库与文档只写 `%APPDATA%\ClipNest`，不包含任何真实用户名或固定盘符。
- 原导出实际已把 JSON 写进数据目录（`export-时间戳.json`），但没有让用户选择位置，所以看起来像“没反应”。
- 新增 `src/ui/export_dialog.rs`，调用 Windows 原生 `GetSaveFileNameW` 打开「另存为」对话框；默认文件名 `clipnest-export-YYYYMMDD-HHMMSS.json`，导出成功后仍由 Store 线程写文件并 Toast 显示实际路径。取消对话框不生成文件。
- 候选窗设置页与管理台工具栏的导出动作统一走该对话框；`Cargo.toml` 增加 `Win32_UI_Controls_Dialogs` feature。

### 校验
- `cargo test --release`：**163 passed / 0 failed / 1 ignored**。
- `--selftest`：14 项通过。
- 已实际从管理台按 `Ctrl+E` 打开原生「导出 ClipNest 数据」对话框，并确认导出写入用户选择的位置（系统对话框，无需产品内截图）。

## 34. v1.9.16（2026-11-05，搜索结果关键字标红）

### 用户反馈
- 搜索结果中，把关键字标红。

### 实现
- `src/ui/paint.rs` 新增 `match_ranges`、`layout_job_highlighted`、`elided_highlighted`、`paint_elided_highlighted`：按当前搜索词切分 `LayoutJob` 文本片段，命中的字符使用 `theme::danger()`（深色为亮红、浅色为深红）绘制；ASCII 大小写不敏感，中文按原样逐字符匹配，返回 UTF-8 字符边界。
- 候选窗 `src/ui/row.rs::clip_row` 的标题与预览都接入高亮，`ClipRow` 新增 `keyword` 字段；`src/ui/picker.rs` 传入 `UiState.search_text`。
- 管理台历史/收藏列表摘要（`src/ui/pages/history.rs::paint_table_row`）、详情面板标题接入高亮；详情正文的只读 `TextEdit` 使用自定义 `layouter` 复用同一套命中切片，正文中的关键字也标红。
- 回收站摘要（`src/ui/pages/trash.rs::paint_row`）同步接入高亮。
- 关键字为空或未命中时直接走原来的单行省略排版，不改变现有列表文案。

### 校验
- `cargo test --release`：**166 passed / 0 failed / 1 ignored**
  - 新增 `match_ranges_finds_all_case_insensitive_occurrences`（大小写不敏感、多处命中）；
  - 新增 `match_ranges_handles_chinese_and_empty_keyword`（中文与空关键字）；
  - 新增 `match_ranges_keeps_utf8_boundaries`（UTF-8 字符边界，避免 LayoutJob 切片 panic）。
- 截图：`docs/screenshots/search-highlight.png`（候选窗搜索 `ClipNest`，标题/预览命中标红）、`docs/screenshots/manager-search.png`（管理台列表摘要与详情正文标红）。

## 35. v1.9.17（2026-11-05，候选窗 Ctrl+F 聚焦搜索框）

### 用户反馈
- 「剪切版界面，如果搜索输入框失去焦点后，无法用快捷键 Ctrl+F 快速聚焦。」
- 「输入框里面也需要有一个 Ctrl+F 的提示符。」

### 实现
- `src/ui/keys.rs`：`PickerCommand` 新增 `FocusSearch`；`route_picker_keys` 在修饰键分支消费 `Ctrl+F`，因此不受搜索框当前焦点、是否有输入文本影响。
- `src/ui/picker.rs`：`handle_command` 收到 `FocusSearch` 时设置 `state.search_focus = true`，复用 `widgets::search_box` 已有的 `auto_focus` 路径在同帧请求 `TextEdit` 焦点；`Ctrl+F` 在按键路由阶段被消费，不会落到文本框。
- 搜索框右侧快捷键键帽由候选窗呼出键提示改为 `Ctrl+F`；空输入时显示键帽，有内容时仍显示清除按钮。
- 同步更新 `src/ui/pages/about.rs` 快捷键表（「候选窗 / 管理台：聚焦搜索框」）与 README 快捷键表、功能说明。

### 校验
- `cargo test --release`：**168 passed / 0 failed / 1 ignored**
  - 新增 `route_ctrl_f_focuses_search_even_without_enter_or_delete`；
  - 新增 `ctrl_f_refocuses_search_box`。
- 截图：`docs/screenshots/picker.png`、`docs/screenshots/picker-light.png`（搜索框右侧 `Ctrl+F` 键帽）。

## 36. v1.9.18（2026-11-05，发布版静态链接运行库）

### 用户反馈
- 准备把 exe 发给别人使用，确认对方双击能否启动。

### 实现
- 新增 `.cargo/config.toml`，对 `x86_64-pc-windows-msvc` 固定 `rustflags = ["-C", "target-feature=+crt-static"]`。
- 发布版由动态 MSVC CRT 改为静态 CRT：exe 的 PE 导入表不再包含 `VCRUNTIME140.dll` 与 `api-ms-win-crt-*`，只依赖 Windows 自带的系统 DLL。
- 目标机器无需安装 VC++ 2015–2022 运行库；单个 `clipnest.exe` 复制到任意目录后双击即可启动，数据目录由 `%APPDATA%` 动态创建。

### 校验
- PE 导入表解析确认无 `VCRUNTIME140.dll` / `api-ms-win-crt-*`。
- 将 exe 单独复制到临时目录，并指定全新的 `CLIPNEST_DATA_DIR` 启动，进程保持运行；日志确认版本 `1.9.18`、数据库与目录自动创建、托盘与快捷键注册成功。
- 构建后的 exe 约 9.4 MB，仍为单文件；`--selftest` 14 项通过。

## 37. v1.10.0（2026-11-05，第一批数据安全：备份/清理/隐私/文件副本）

### 用户需求
- 针对深度痛点分析确认的第一批 A1–A5：完整备份/恢复、清空与退出清理、隐私默认值与临时暂停、收藏文件副本可靠性、首次启动可感知。
- 约束：普通复制只记文件路径，收藏时才复制实体，避免无谓磁盘占用；退出清理只在明确退出时执行。

### 实现
- **A1 完整备份/恢复**：新增 `src/store/backup.rs`，生成单文件 `.cnbak`（zip：`VACUUM INTO` 的 `data.db` + `blobs/` + `manifest.json`）。manifest 记录格式版本、程序版本、时间、条目/图片/文件数量及逐个文件大小与 SHA-256；写入临时文件后原子替换。恢复先解压到 staging 并逐项校验，再在单事务中合并导入（按内容 hash 去重，保留时间、收藏、置顶、文件状态与来源），失败回滚并清理新建文件。设置页新增「完整备份… / 从备份恢复…」，恢复前显示预览 modal。
- **A2 清空与退出清理**：`StoreCmd::Clear` 增加 `include_favorites`，默认只清未收藏并返回实际清理/保留数量；含收藏清空需输入「清空收藏」，并在 Store 线程执行 `secure_delete` + `VACUUM` + WAL checkpoint。`StoreCmd::ClearForExit(policy)` 在 `request_quit` 中先入队再 Shutdown/join，确保落盘后才退出；关闭面板或隐藏不触发。设置页新增「不清理 / 清空未收藏 / 全部清空」三档。
- **A3 隐私**：`Settings` 首次缺省时注入 9 个常见密码管理器进程名（显式空数组不覆盖）；条目右键菜单与详情面板新增「忽略来源应用」，立即保存并同步主进程。新增运行期 `TempPause`（15 分钟 / 1 小时 / 直到重启），不写设置文件，托盘与设置页均可恢复，到期自动恢复。
- **A4 文件副本状态**：`FileEntry` 增加 `copy_state`（Pending/Stored/Failed/TooLarge/Missing/NotNeeded）；收藏时异步复制并逐文件持久化状态，详情页显示徽标与「重试保存副本」。
- **A5 首次启动**：新增 `first_run_done`，第一次真正显示面板时提示快捷键/托盘/默认不自启；第二实例检测到已有实例时弹 MessageBox 显示版本并尝试唤起，不再静默退出。
- 两批开发基于独立 worktree；本版合并 A1 与 A2–A5，发布分支 `release/v1.10`。

### 校验
- `cargo test --release`：**182 passed / 0 failed / 1 ignored**。
  - 新增 `tests_backup`：完整备份/恢复文本、图片、收藏文件副本；备份被篡改时校验失败且不触碰现有数据库。
  - 新增清空范围、退出清理策略、临时暂停、忽略名单规范化、文件副本状态与重试等测试。
- `cargo check --all-targets`：零错误。
- 合并冲突处理：`src/store.rs` 导入、`src/ui.rs` 恢复预览/清空确认/忽略应用、`src/ui/pages/settings.rs` 数据区重复块均已人工核对。
- 说明：完整备份/恢复与退出清理以单元测试和 Store 线程测试覆盖；原生文件对话框与首次启动提示需实际运行验证。
## 38. v1.11.0（2026-11-05，第二批体验效率：富文本/搜索/排序/文件操作）

### 用户需求
- 第二批 B1–B5：富文本保真、搜索升级、排序与批量清理、文件/图片操作、时间语义与可访问性。
- 约束：B6 可选数据库加密涉及密码学与迁移风险，单独评估，不阻塞本版。

### 实现
- **B1 富文本**：SQLite schema v4，`clips` 增加 `html` / `rtf` 两列，读取/写入/粘贴/导出全链路带上；新增 `src/clipboard/html.rs` 解析 CF_HTML、生成 CF_HTML、清洗危险标签与外部资源、HTML→纯文本、RTF 基本过滤。写入时优先同时写 `CF_UNICODETEXT` + `CF_HTML` + `RTF`，目标程序不支持时降级纯文本。详情面板显示 HTML/RTF 徽标。
- **B2 搜索**：搜索词按空白拆成多个关键词（双引号短语不拆），支持「任一 / 全部」；新增来源应用筛选与 `ListSourceApps`；设置项 `keep_search_on_open` 可保留上次搜索。FTS5 多词 MATCH，短词/路径走逐词 LIKE。
- **B3 排序与回收站**：新增「最近 / 最早 / 最大 / 类型 / 使用最多」排序并贯通 picker 与管理台；收藏页置顶优先于排序；回收站改为 `ScrollArea::show_rows` 虚拟化并触底分页，修复只能看到前 500 条；支持全选当前已加载结果。
- **B4 文件/图片操作**：新增 `src/sys/shell.rs`（ShellExecuteW / explorer /select / 复制路径）；文件条目支持打开、所在文件夹、复制路径；图片另存为 PNG、文本另存为 TXT，走系统另存为对话框；候选窗右键、管理台表格右键与详情面板均接入。
- **B5 时间语义 / 缩放 / 可访问性**：`TimeBasis{LastUsed, Captured}` 贯通设置、查询与 UI；日历头部支持 `‹‹ / ››` 按年跳转；候选窗可拖拽缩放并写回 `picker_size`；新增 `ui_scale` 0.8–1.5 与紧凑/标准/宽大预设；组词中按 Esc 不再关闭候选窗。
- 集成：从 `release/v1.10` 建 `release/v1.11`，依次合并 B1、B2+B3、B4+B5；冲突涉及 model/store/ui/picker/history/settings/tests，逐块人工合并（保留各批功能与测试，补齐 SearchQuery 新字段、PastePayload html/rtf、忽略来源与文件动作并存）。

### 校验
- `cargo test --release`：**227 passed / 0 failed / 1 ignored**（B1 后 203、B2+B3 后 215、B4+B5 后 227）。
- `cargo check --all-targets`：零错误。
- 已知限制：多文件条目仅对第一个文件提供打开/定位/复制路径；另存为依赖图片/文本 blob，blob 被清理时提示内容不存在；`ui_scale` 只缩放 egui 内容、不改变原生窗口尺寸；复杂 CSS/脚本不保留。
- B6（可选数据库加密）本版未实现，留待单独评估密码解锁、备份/导入与搜索兼容性。

## 39. v1.12.0（2026-11-05，第三批 B6：可选数据库加密）

### 用户需求
- B6：可选数据库加密，默认关闭；开启后数据库文件不可被其它本机账户直接读取；备份/恢复、搜索需兼容。

### 实现
- 构建：`rusqlite` 切换到 `bundled-sqlcipher-vendored-openssl`；本机用 Git MSYS Perl + VS 工具链静态编译 OpenSSL 3.0.16（`E:\use\ai\deepseek\Rust\openssl-static`），release 仍为单 exe / 静态 CRT。构建环境需 `OPENSSL_DIR`、`OPENSSL_STATIC=1`、`OPENSSL_NO_VENDOR=1`、`OPENSSL_LIBS=libssl:libcrypto`。
- 密钥：新增 `src/db_crypto.rs`。32 字节随机密钥（`BCryptGenRandom`）经 Windows DPAPI（当前用户）加密后写入 `数据目录/db.key`，启动时自动解密解锁，不把明文密钥落盘；同机其他 Windows 用户无法解密。
- 数据库：`DbOptions.key_hex` / `Db.key_hex`；打开加密库时先 `PRAGMA key = "x'...'"` 并查询 `sqlite_master` 验证，缺密钥/密钥错误返回明确错误，不触发损坏重建。
- 开关：`StoreCmd::SetEncryption`、`Event::EncryptionChanged`；`Db::set_encryption()` 启用/关闭均通过 `sqlcipher_export()` 导出临时库（`data.encrypt.tmp` / `data.plain.tmp`），关闭旧连接、原子替换 `data.db`、再重新打开；失败时保留原库与密钥并回滚。
- UI：设置 → 数据新增「本地数据加密（SQLCipher）」开关；`config::load_settings` 以 `db.key` + 实际数据库头校正 `encrypt_db` 状态；主窗口与管理台均处理开关与结果事件。
- 备份：`backup::inspect_backup`/`restore_backup` 打开加密快照时传入当前 `key_hex`，加密库的完整备份可在同一 Windows 用户下检查与恢复。

### 校验
- `cargo test --release`：**231 passed / 0 failed / 1 ignored**。新增 `database_encryption_roundtrip_and_disable`（写库 → 开启加密 → 关闭旧连接用 key 重开 → 数据保留 → 关闭加密 → 明文重开数据保留）；`db_crypto` 新增密钥 DPAPI 往返与密文头检测测试。
- `cargo check --all-targets`：零错误。
- `--selftest`：PASS（15 项）；release 构建 + 9 尺寸图标 patch 成功。

### 取舍与限制
- 为保持启动自动解锁与单 exe，采用 **Windows DPAPI 当前用户密钥**而非每次输入密码；同一 Windows 账户可自动打开，换机/换用户或丢失 `db.key` 无法解密。
- 加密后的 `.cnbak` 备份同样绑定当前 Windows 用户；跨机迁移应先关闭加密或导出未加密备份。
- B6 未实现“输入密码解锁 / 跨机密码迁移”；如需该能力，应在后续版本增加密码派生密钥与解锁对话框。

## 40. v1.12.1（2026-11-05，暂停同步与反馈修复）

### 用户反馈
- 管理台设置页的临时暂停（15 分钟 / 1 小时 / 到重启）点击无反应。
- 托盘「暂停记录」与设置页按钮状态不同步；再次点击托盘暂停后仍不能复制，需要点「恢复记录」。
- 询问「任一关键词 / 全部关键词」的含义。
- 询问 DPAPI 自动解锁如何验证。
- 询问收藏文件副本的路径、取消收藏是否删除、反复收藏/取消的行为。

### 修复
- `src/model.rs`：`TempPause` 增加序列化；新增 `RuntimeState { pause_capture, temp_pause, updated_ms }` 与 `is_paused()`；`MatchMode::hint()` 提供搜索模式说明。
- `src/config.rs`：新增 `runtime.json` 原子读写；主进程把运行期暂停状态写入，管理台轮询读取。
- `src/sys/thread.rs`：新增 `WM_COPYDATA` 临时暂停消息（`TEMP_PAUSE_MAGIC = 0x434C_4E50`）与 `send_temp_pause_update()`；300ms 定时器检查临时暂停到期。
- `src/sys/state.rs`：`toggle_pause` 在永久或临时任一暂停时调用 `resume_capture()` 同时清除两者；`SysCmd::SetPause(false)` 清除临时暂停；托盘勾选改为根据 `RuntimeState::is_paused()` 显示；暂停状态变化 emit `SettingsUpdated`。
- `src/manager_window.rs`：初始化读取 `runtime.json`，每 500ms 同步设置页；临时暂停按钮改为向主进程发送 IPC，不再只 toast。
- `src/ui.rs`：`set_pause_state()` 同步 `paused` 与设置草稿。
- `src/ui/picker.rs`、`src/ui/pages/history.rs`：搜索模式下拉框增加悬停说明。
- `src/ui/pages/settings.rs`：数据区显示收藏副本目录，并提供「打开收藏副本目录」按钮。

### 校验
- `cargo test --release`：**235 passed / 0 failed / 1 ignored**（新增 `runtime_state_roundtrip_and_pause_summary`、`set_pause_state_keeps_settings_and_draft_in_sync`、`database_encryption_auto_unlocks_from_env_key`、`match_mode_hints_explain_any_and_all`）。
- `cargo check --all-targets`：零错误。
- 手动验证：启动新构建后检查 `%APPDATA%\ClipNest\runtime.json` 随暂停/恢复变化；管理台设置页与托盘勾选同步；临时暂停到期自动恢复。

### 说明
- 收藏文件副本目前固定在 `数据目录\blobs\files\<hash>\`，不做用户自定义目录；取消收藏不删除副本，重复收藏复用已有副本。若后续需要自定义目录或清理未收藏副本，需要新增设置并处理备份/迁移兼容。

## 41. v1.12.2（2026-11-05，收藏副本延迟清理 / 候选窗与托盘精简）

### 用户反馈
- 反复收藏/取消收藏是否会重新计算文件 hash、是否耗时？希望在数据库标记取消收藏，并在延迟一段时间或退出后自动清理。
- 候选窗不需要「全部来源」；搜索框内提示要体现「任一/全部关键词」。
- 候选窗支持上下拖动高度。
- 列表显示使用次数（粘贴次数）。
- 托盘菜单移除「恢复记录」「清空历史」。

### 实现
- `src/store/schema.rs`：新增 `favorite_cleanup(clip_id PRIMARY KEY, blob_id, marked_at)` 及 `idx_favorite_cleanup_marked`；旧库由 `CREATE TABLE IF NOT EXISTS` 自动补齐，不升 schema 版本。
- `src/store/db.rs`：`set_favorited(false)` 在事务内为文件条目写入/刷新待清理标记，重新收藏时删除标记；新增 `cleanup_favorite_copies()`，退出/启动时删除未收藏条目的 `blobs/files/<blob_id>/`、`blobs` 行，把 `files_json` 的 `stored` 复位，并清理孤立标记。`save_favorite_files` 增加「已取消收藏则不再复制」的竞态保护，避免收藏复制命令与取消收藏交错时产生无主副本。
- `src/store/blob.rs`：`collect_files` 提升为 `pub(crate)` 供清理统计文件数。
- `src/store.rs`：Store 启动时清理上次退出遗留标记，Store 线程退出前再清理一次（WAL checkpoint 之前）。
- `src/ui/picker.rs`：移除来源下拉与 `next_source`；搜索框占位按 `match_mode` 显示「空格分词，命中任意一个即可 / 空格分词，每个词都必须命中」；底部增加 6px 竖向拖拽区，`ViewportCommand::BeginResize(ResizeDirection::South)` 调整窗口高度。
- `src/ui.rs`：`effective_source_app()` 在候选窗始终返回 None（管理台保留来源筛选）。
- `src/ui/row.rs`：候选窗行右侧时间后追加「N 次」使用次数。
- `src/ui/pages/history.rs`：管理台表格新增「使用」列（窄窗口隐藏），列宽与表头同步调整。
- `src/sys/tray.rs`、`src/sys/state.rs`：移除托盘「清空历史…」「恢复记录」菜单项及 `MENU_CLEAR`/`MENU_RESUME` 处理；临时暂停仍保留，托盘「暂停记录」可清除永久+临时暂停。

### 校验
- `cargo test --release`：**237 passed / 0 failed / 1 ignored**（新增 `unfavorite_marks_copy_for_delayed_cleanup`、`picker_does_not_carry_source_filter`，并更新依赖收藏状态的文件复制/导出测试）。
- `cargo check --all-targets`：零错误。
- 手动验证建议：收藏一个文件条目 → 取消收藏 → 退出 ClipNest，确认 `数据目录\blobs\files\<blob_id>\` 被删除；再次收藏应重新复制源文件。候选窗不再出现来源下拉，底部拖动可改变高度，列表显示使用次数；托盘菜单无「清空历史」「恢复记录」。

### 说明
- 文件复制仍**不计算文件内容 hash**，只对剪贴板条目 hash + 文件路径字符串做 SHA-256；已有副本通过目录名与文件大小复用，不会每次复制。
- 清理发生在退出/下次启动，不是固定延迟计时；运行期间反复收藏/取消不会触发删除，避免抖动。收藏副本目录仍固定，未新增自定义路径。

## 42. v1.12.3（2026-11-05，候选窗下拉重叠 / 日期点击 / 图片行间距）

### 用户反馈
- 候选窗「任一关键词」「最近使用」两个下拉控件重叠。
- 日期选择组件点击没反应。
- 列表第一行/第二行也有重叠，似乎只有图片条目出现。

### 实现
- `src/ui/picker.rs`：`mode_rect` 宽度 88 → 104；与排序下拉的间距从 `theme::GAP_XS`（4px）增到 12px，避免 ComboBox 按钮的箭头/内边距超出传入矩形后相互覆盖。
- `src/ui/pages/history.rs`：管理台「指定日期」按钮的纵坐标由 `time_rect.bottom() + 6.0` 改为 `basis_rect.bottom() + 6.0`，不再与「时间依据」整行重叠。
- `src/ui/widgets.rs`、`src/ui.rs`、`src/ui/picker.rs`、`src/ui/pages/history.rs`：`date_picker_modal()` 增加 `allow_close` 参数；UI 状态新增 `date_picker_just_opened`，打开弹窗的首帧忽略 `Modal::should_close()`，避免打开它的同一次点击被当成点击遮罩而立刻关闭。
- `src/model.rs`：图片条目标题改为 `图片 {w}x{h}`，副标题改为 `{size}  {w}x{h}`（ASCII `x`、双空格分隔），避免 `×`/`·` 走回退字体时行高/基线不一致造成两行文字拥挤的观感。

### 校验
- `cargo check --all-targets`：零错误。
- `cargo test --release`：**237 passed / 0 failed / 1 ignored**。
- `cargo build --release` + `tools/build-release.ps1` 9 尺寸图标 + `--selftest` 15 项 PASS。
- 预览截图确认候选窗两个下拉已分离；图片行标题为 `图片 1920x1080`。

### 说明
- 日期弹窗首帧不响应外部点击，从第二帧起恢复点击遮罩/取消关闭。
- 图片行改用 ASCII 尺寸格式，不改变实际尺寸、复制或粘贴行为。

## 43. v1.12.4（2026-11-05，图片行行距 / 日期弹窗）

### 用户反馈
- 候选窗图片条目的上下间距仍与文本行不一致，左侧缩略图之间出现空白差。
- 日期选择器点击没反应，无法选择起止日期范围。

### 根因
- `src/ui/row.rs::clip_row` 先按 64px 分配整行，再对图片缩略图调用 `ui.put(icon_rect, Image)`；`ui.put` 会把父 `Ui` 的光标推进到缩略图矩形底部（40px 图 + 行内偏移约 52px），而不是整行 64px。图片行因此实际只占 52px，后续行上移，形成“只有图片行间距不对”。
- `src/ui/widgets.rs::date_picker_modal` 仍依赖 `egui::Modal::should_close()`。打开弹窗的点击或遮罩点击在某些窗口/合成器下会在弹窗刚出现时被判定为“点击外部”，导致弹窗立即关闭，看起来像“点日期没反应”。

### 实现
- `src/ui/row.rs`：图片缩略图 `ui.put(...)` 后调用 `ui.advance_cursor_after_rect(rect)`，把布局光标恢复到整行底部。
- `src/ui/widgets.rs`：删除 `Modal::should_close()` 判定；弹窗只由「取消」/ `Esc` / 选定结束日期关闭。`allow_close` 为真时显式消费 Esc 返回 `DatePickerAction::Closed`。
- 保留 v1.12.3 的下拉宽度、管理台日期按钮位置和图片 ASCII 尺寸格式。

### 校验
- `cargo check --all-targets`：零错误。
- `cargo test --release`：**237 passed / 0 failed / 1 ignored**。
- `cargo build --release` + `tools/build-release.ps1` 9 尺寸图标 + `--selftest` 15 项 PASS。
- 预览确认：日期弹窗稳定显示，3–5 日范围高亮、可点击「今天/取消」；候选窗图片行后续行位置恢复 64px 步进。

### 说明
- 日期弹窗不再点击遮罩关闭，需点「取消」或按 Esc；避免误关。
- 图片行只修正布局高度，不改缩略图尺寸、粘贴或复制行为。

## 44. v1.12.5（2026-11-05，日期弹窗点击被列表抢占 / 首次选择未保存）

### 用户反馈
- v1.12.4 后日期弹窗能打开，但点击日历格仍没有反应，无法选择起止日期范围。

### 根因
- `src/ui/picker.rs::show` 中日期弹窗原先在 `render_list` **之前**绘制。弹窗视觉上在列表上方，但列表的点击区域在弹窗之后注册，实际抢走了日历格的点击，所以表现为“弹窗能打开、点日期没反应”。
- 日历第一下点开始日期时，`date_picker_modal` 只修改传入的 `start`，不返回 `DatePickerAction`；调用方只在 `Some(action)` 分支保存起止日期，导致下一帧又从旧值重绘，第一次点击被丢掉，看起来也像“点了没反应”。

### 实现
- `src/ui/picker.rs`：把 `date_picker_modal(...)` 整段从列表之前移到 `picker::show` 末尾（底部缩放抓手之后），确保弹窗最后绘制并优先接收点击。
- `src/ui/picker.rs`、`src/ui/pages/history.rs`：无论 `date_picker_modal` 是否返回动作，都把当前 `start`/`end`/`month` 写回 UI 状态，保留“第一次点开始日期”的选择。
- `src/ui/widgets.rs`：`date_picker_modal` 继续使用 `egui::Modal`（不调用 `should_close`，仅「取消」/Esc/选定范围关闭），保证背景遮罩和视觉层级正常。
- 管理台日期弹窗本来就在 table/detail 之后绘制，无需调整。

### 校验
- 预览实测：日历弹窗不透明覆盖列表；点击「今天」后弹窗关闭，日期范围标签更新为 `2026-11-05`；点击日历格后开始日期立即高亮、提示变为「再点结束日期」，证明第一次点击已保存。
- `cargo check --all-targets`：零错误。
- `cargo test --release`：**237 passed / 0 failed / 1 ignored**。
- `cargo build --release` + 9 尺寸图标 + `--selftest` 15 项 PASS。

### 说明
- 弹窗仍不使用点击遮罩关闭，避免打开弹窗的同一次点击把它误关；需点「取消」或按 Esc。
- 图片行行距修复沿用 v1.12.4。

## 45. v1.13.0（2026-11-05，候选窗选中行预览 / 移除筛选蓝点）

### 用户反馈
- 筛选按钮右上角多了一个蓝点，看起来像未读标记。
- 希望候选窗选中图片或溢出文本行时，弹出带弹框动画的预览，并在设置中提供预览开关。

### 实现
- `src/ui/picker.rs` 删除筛选按钮上的 active-filter 蓝点（它只表示时间筛选已生效）。
- `src/model.rs` 新增 `Settings.preview_on_select`，默认 `true`；旧配置缺字段时按默认开启。
- `src/ui/pages/settings.rs` 行为区新增「选中后预览图片 / 长文本」开关。
- `src/ui.rs` 新增 `picker_preview_id` / `picker_preview_at`，按选中项变化驱动动画。
- `src/ui/picker.rs` 新增 `paint_selection_preview`：
  - 仅在候选窗、预览开关开启、且无日期弹窗/确认框时显示；
  - 图片（有 `blob_id`）显示缩略图大图；文本超过 60 字或包含换行时显示完整正文，最多 6000 字，沿用关键字红色高亮；文件不预览；
  - 使用 `egui::Area(Order::Foreground)` + `set_opacity` + ease-out 位移实现 180ms 淡入上浮；
  - 卡片居中锚定列表区域，图片最大 352×236，文本卡片宽 420。
- `src/ui/preview.rs` 新增 `CLIPNEST_PREVIEW_SELECT`，用于截图时指定选中下标。

### 校验
- `cargo test --release`：**239 passed / 0 failed / 1 ignored**（新增选中预览条件、等比缩放测试）。
- 预览实测：选中图片显示「图片预览 / 1920x1080 · 2.20 MB」+ 大图；选中长文本显示「文本预览 / 88 字 · 使用 3 次」+ 完整正文；日期弹窗或确认框打开时预览自动隐藏。
- `cargo build --release` + 9 尺寸图标 + `--selftest` 15 项 PASS。
## 46. v1.13.1（2026-11-05，选中图片预览发糊）

### 用户反馈
- v1.13.0 的图片预览完全模糊，看不清图片内容，预览失去意义。

### 根因
- 预览卡直接取用了列表缩略图缓存 `ThumbCache`，而该缓存解码时最长边固定为 96px（`THUMB_MAX_SIDE`）。96px 纹理放大到 352×236 的卡片上必然糊。

### 实现
- `src/ui/thumbs.rs`：将 `ThumbCache` 泛化为可配置候选路径与最长边的通用缓存；新增 `PREVIEW_MAX_SIDE = 960`、`image_candidates`（只读 `blobs/images` 原图，不读 `blobs/thumbs`）、`ThumbCache::new_preview`。
- `src/ui.rs`：`UiState` 持有独立的高分辨率预览缓存 `preview_thumbs`；新增 `insert_preview_image` 供预览假数据注入。
- `src/ui/picker.rs`：图片预览优先取 `preview_thumbs` 的 960px 原图，后台解码完成前回退 96px 缩略图占位；预览卡宽度增至 500px，图片显示上限 472×280。
- `src/ui/thumbs.rs`：后台解码完成后主动 `request_repaint()`，避免预览卡停留在低清缩略图直到下一次输入事件。
- `src/ui/preview.rs`：预览模式为演示图片额外注入 480×270 的高清假图，保证截图验证真实。

### 校验
- 新增 `preview_candidates_skip_thumbnails`：预览候选路径全部位于 `blobs/images`，不含 `/thumbs/`。
- 新增 `preview_decode_keeps_original_detail`：1920×1080 原图经预览解码后为 960×540，不再缩到 96px。
- `cargo test --release`：**241 passed / 0 failed / 1 ignored**。
- 预览截图确认卡片为 500px 宽、图片为 960px 原图缩放，文字与图形边缘清晰。

## 47. v1.13.2（2026-11-05，浅色主题低透明度下发暗）

### 用户反馈
- 设置为浅色主题，但候选窗看起来是深色，文字几乎看不清。

### 根因
- `theme::window_bg()` 在磨砂模式下按 `glass_opacity` 计算窗口底色 alpha；用户把透明度滑到最低 35，窗口底色 alpha 只有 89/255。
- 当 Acrylic 背景材质未生效或背后是深色桌面时，这一层 35% 的浅色底叠在近黑背景上，实际渲染成深灰（用户截图面板像素约 `(83,85,86)`），深色文字对比度不足；搜索框等实色卡片仍是浅色，所以出现“浅色主题但面板发暗”的割裂观感。

### 实现
- `src/ui/theme.rs::window_bg()`：浅色主题窗口底色增加不透明度下限 220/255（约 86%）；深色主题仍按原透明度计算，不影响玻璃观感。
- 这样即使 Acrylic 失效或桌面为深色，浅色面板也不会被背景拖暗，文字保持可读。

### 校验
- 新增 `light_window_bg_keeps_readable_opacity_floor`：浅色 + 35% 透明度时 `window_bg().a() >= 220`。
- `cargo test --release`：**242 passed / 0 failed / 1 ignored**。
- `--ui-preview picker` 在深色预览外层背景下确认面板保持浅色、文字清晰。

## 48. v1.13.3（2026-11-05，优化选中预览关闭方式）

### 用户反馈
- 预览图弹出后不方便关闭；如果觉得太烦，也没有一键关闭预览功能的入口。

### 实现
- `src/ui/picker.rs`：预览打开时 `Esc` 先只关闭当前条目的预览并 toast「预览已关闭，Esc 再按隐藏面板」，再按一次才隐藏候选窗；关闭只作用于当前条目（`picker_preview_dismissed`），换行或重新打开候选窗自动恢复。
- `src/ui/widgets.rs`：底部提示条右侧新增「预览开 / 预览关」按钮（`HintBarOutput.preview_clicked`），点击立即切换。
- `src/ui.rs`：`UiState` 新增 `picker_preview_dismissed`、`set_preview_on_select()`；新增 `UiAction::SetPreviewOnSelect(bool)`。
- `src/app.rs` / `src/manager_window.rs`：处理一键开关，写入设置并持久化；管理台同步通知主进程。
- `src/ui/preview.rs`：预览模式本地消化该动作，便于截图验证。

### 校验
- `src/ui/tests.rs` 新增 `preview_toggle_resets_dismissed_and_setting`、`enter_picker_restores_preview_after_escape_dismissal`。
- `cargo test --release`：**244 passed / 0 failed / 1 ignored**。
- `--ui-preview picker` 截图确认底部「预览开」按钮与「共 1,234 条」、齿轮不重叠，预览卡显示正常。

## 49. v1.13.4（2026-11-06，时间筛选折叠时显示当前条件）

### 用户反馈
- 搜索时如果单独选了时间筛选，折叠后看不出筛选是否生效；希望有标识，或者自动展开，体验更好。

### 方案取舍
- 不自动展开：自动展开会让列表区域跳动、长期占用高度，用户想专注列表时反而碍事。
- 不只放一个小色点：v1.13.0 已移除筛选按钮上的蓝点，因为含义不明确（容易被当成未读/新内容标记）。
- 采用**按钮直接显示条件 + 强调色**：折叠时文案变成「筛选 · 近7天」「筛选 · 指定日期」等，点击仍展开时间筛选行；既清楚又不占额外空间。

### 实现
- `src/ui/picker.rs`：新增 `filter_button_label()`，折叠且 `picker_time_range != All` 时生成「筛选 · {范围}」；按钮宽度按文字实测宽度 + 44px 动态计算，右对齐，避免挤压排序下拉。
- `src/ui/widgets.rs`：新增 `ButtonKind::FilterActive`（强调色描边/文字、悬停加深）；折叠且筛选生效时使用，展开后恢复普通次按钮样式。
- 鼠标悬停提示当前范围（「时间筛选已生效：近7天。点击展开调整」）。
- `src/ui/preview.rs`：新增 `CLIPNEST_PREVIEW_TIME`（`today` / `7` / `30` / `90`）供截图验证。

### 校验
- `src/ui/picker.rs` 新增 `filter_button_reflects_active_time_range`。
- `cargo test --release`：**245 passed / 0 failed / 1 ignored**。
- `--ui-preview picker` + `CLIPNEST_PREVIEW_TIME=7` 截图 `docs/screenshots/picker-time-filter.png`：按钮显示「筛选 · 近7天」，强调色描边，右侧不越界。
- release 构建 + 9 尺寸图标 patch：exe 13,285,888B，SHA-256 `0782F0BC82BDED21348C0E7CAEBB3FD0DD4A50C87B2DD6542A34D18A4DF3A92F`；`--selftest` 15 项 PASS。

## 50. v1.13.5（2026-11-06，加密切换失败导致导出/完整备份不可用）

### 用户反馈
- 「我刚试了导出和完整备份，这两功能，不能用。生成的文件是个快捷方式。」用户给出 `%APPDATA%\Microsoft\Windows\Recent\clipnest-backup-20261106-090419.cnbak.lnk`；Recent 里两个快捷方式分别指向 `C:\Users\jun.liu\Downloads\clipnest-backup-20261106-090406.cnbak` / `...-090419.cnbak`，目标均不存在。

### 根因
- 08:51 在管理台勾选数据库加密。管理台是独立进程，自己打开着 `data.db`；`Db::set_encryption` 先替换连接为内存库，再删除/重命名主库文件，Windows 因管理台仍持有句柄返回 `os error 5（拒绝访问）`。
- 错误路径直接返回，`self.conn` 已指向空内存库，Store 后续所有查询都报 `no such table: clips / blobs`；09:04 的导出与完整备份因此在写文件前失败。
- Recent 的 `.lnk` 是 Windows 保存对话框自动创建的「最近使用」记录，不是程序输出；真实 `.cnbak` 没有生成。
- 磁盘上的原库仍在；`ClipNest.recover-20261106` 副本经 Python sqlite3 校验：clips=32（文本 21 / 图片 8 / 文件 3），meta schema_version=4、clip_count=28。

### 修复
- `src/store/db.rs::set_encryption`：替换前保留 `data.encrypt.bak` / `plain.bak`；先 checkpoint、关旧连接、rename 原库、rename 临时库、打开新库；任一步失败调用 `reopen_after_failed_switch(old_key)` 恢复原库，绝不让 Store 停留在内存连接；成功才删备份，开/关都清理临时文件。
- `src/store.rs::switch_database_encryption`：对 `拒绝访问` / `os error 5` / `另一个程序` 做 20 次 ×150ms 重试，等待管理台释放句柄。
- 加密切换改为主进程协调：管理台 `UiAction::SetDbEncryption` 通过 `WM_COPYDATA`（`ENCRYPTION_MAGIC=0x434C_4E45`）发请求后自行关闭；主进程收到 `Event::EncryptionRequested`，丢弃 Store、等待管理台窗口消失、转换、重建 Store，再重开设置页。
- 新增 `Event::EncryptionRequested`、`send_encryption_request()`、`handle_encryption_request()`；失败时 message box + toast，不静默。

### 校验
- 新增 `encryption_switch_failure_restores_original_database`：制造替换失败后断言原库仍可打开、条目仍在、`db.key`/临时文件已清理。
- `cargo test --release`：**246 passed / 0 failed / 1 ignored**。
- 临时数据目录实测：32 条 copied 数据；IPC 开启加密 → 日志「已加密本地数据库」，`db.key` 生成；管理台退出后 IPC 关闭 → 日志「已关闭数据库加密」；Python 复读 `clips=32`，双向转换数据保留。
- UI 实测：管理台「导出」选择 `target\ui-export-test.json`，写出 47,492B；设置 → 数据 →「完整备份…」选择 `target\ui-backup-test.cnbak`，日志「完整备份完成：28 条，图片 8」，文件 1,172,332B。均不再只留 `.lnk`。
- 用户真实数据目录未改动；`%APPDATA%\ClipNest` 保持原样，恢复副本在 `%APPDATA%\ClipNest.recover-20261106`。
- release 构建 + 9 尺寸图标 patch：exe 13,302,784B，SHA-256 `F222A66D6FBD0A828F74D96FE4BF6D94154DDDCE9F93540960C1BC44787A7FC9`；`--selftest` 15 项 PASS；app v1.13.5 启动后读取真实 `data.db`（32 条，明文）。

## 51. v1.13.6（2026-11-06，加密后无法恢复明文备份）

### 用户反馈
- 清空历史后，使用 `C:\Users\jun.liu\Downloads\clipnest-backup-20261106-100628.cnbak` 恢复没有效果。日志两次报 `检查备份失败：SQLite 错误: file is not a database`。

### 根因
- 该备份创建于 10:06（加密开启前），压缩包内 `data.db` 是明文 SQLite（Python 校验可读出 clips=34、integrity_check=ok）。
- 10:15 开启数据库加密后，恢复流程仍把当前 `db.key` 传给 `open_snapshot()`；SQLCipher 对明文库执行 `PRAGMA key` 会报 `file is not a database`。
- 是否加密应看**备份内数据库文件头**，而不是当前主库是否加密。

### 修复
- `src/store/backup.rs::open_snapshot()` 先用 `db_crypto::is_encrypted_file(path)` 判断快照是否加密：
  - 明文快照：即使调用方传入 key 也不执行 `PRAGMA key`；
  - 加密快照：必须提供 key，否则返回「备份中的数据库已加密，需要当前 Windows 用户密钥」；
  - 校验仍走 `PRAGMA quick_check`。
- 增加回归断言：明文备份分别用当前 key 检查/恢复都必须成功。

### 校验
- `cargo test --release backup`：5 passed / 0 failed（含加密备份往返与明文备份进加密库）。
- 新增 `plaintext_backup_restores_into_encrypted_database`：明文备份恢复到已加密主库，数据保留。
- `cargo test --release`：**247 passed / 0 failed / 1 ignored**。
- 用户备份解包校验：`data.db` 311,296B，`SQLite format 3`，clips=34、blobs=8、integrity_check=ok；恢复预览显示 30 条有效条目、8 张图片、完整性校验通过。
- 实机恢复：v1.13.6 管理台选择该 `.cnbak` → 预览 →「开始恢复」，日志 `从备份合并恢复完成：新增 30 条，跳过重复/缺失 0 条，图片 8`；管理台条目数 32（含恢复后新采集的剪贴板条目）。
- release 构建 + 9 尺寸图标 patch：exe 13,303,296B，SHA-256 `621D6A1FAA64B55D9AAB7D00E5C4B526816D74FE43B255F173FDB00A3B49A436`；`--selftest` 15 项 PASS；app v1.13.6 启动并在恢复后保持运行。

## 52. v1.13.7（2026-11-06，导出/备份/恢复结果没有提示）

### 用户反馈
- 「导出、备份、恢复，操作结果都没有提示，体验不佳。」

### 根因
- 操作流程其实已经产生提示：`src/app.rs` 与 `src/manager_window.rs` 收到 `BackupEvent::Created/Restored/Failed`、`Event::Toast` 后都会调用 `UiState::toast()`；导出开始时也会弹「正在导出到 …」。
- 但 toast **只在候选窗 `src/ui/picker.rs::show()` 里绘制**，管理台 `src/ui/manager.rs::show()` 没有绘制 toast。导出/备份/恢复按钮都在管理台，所以提示只存在于不可见的候选窗状态里，用户看起来就是「点了没反应」。
- 另外 toast 默认只有 1.6s，即使显示也容易错过包含路径/条数的结果。

### 修复
- `src/ui.rs`：
  - 增加 `pub(crate) fn toast_view() -> Option<(String, f32)>`，统一候选窗与管理台的淡入淡出计算；
  - `TOAST_DURATION` 从 1600ms 延长到 **3200ms**。
- `src/ui/manager.rs::show()` 末尾绘制 `widgets::paint_toast()`，位于窗口底部内容区上方、底部保存栏之上；失败提示同样可见。
- `src/ui/picker.rs` 改用 `state.toast_view()`，不再各自写一套 1.6s 公式。
- `src/ui/preview.rs` 允许 `CLIPNEST_PREVIEW_TOAST` 在管理台预览中注入假 toast，便于截图验证。

### 校验
- 新增 `toast_view_reports_text_and_alpha`：toast 中段文本正确、透明度接近 1。
- `cargo test --release`：**248 passed / 0 failed / 1 ignored**。
- `--ui-preview settings` + `CLIPNEST_PREVIEW_TOAST` 截图 `docs/screenshots/manager-toast.png`：管理台底部可见「完整备份完成：28 条，图片 8，收藏文件副本 0，1.17 MB」，位于保存栏上方、不遮挡按钮。
- release 构建 + 9 尺寸图标 patch：exe 13,303,296B，SHA-256 `5F19E622D6D41F69BBB8FC0A2891192D62DF245299EFF62DA42AAC8DF740595C`；`--selftest` 15 项 PASS。

## 53. v1.13.8（2026-11-06，管理台筛选行与候选窗统一）

### 用户反馈
- 用户 m18857 发管理台筛选行截图：「管理台：这里的ui表现和粘贴板不一样，我要做成一样的效果的。是不是可以做成一个组件复用？」
- 截图里管理台是「筛选」固定宽按钮 + 右上蓝点、关键词/排序各 96/112、来源 150、gap 6；候选窗已经是「筛选 · 近7天」动态强调色按钮，两者视觉与间距不一致。

### 实现
- `src/ui/widgets.rs` 新增共享组件：
  - `time_filter_label(time_range, expanded)`：展开或全部时间显示「筛选」，否则显示「筛选 · 近7天 / 指定日期」；
  - `time_filter_width(ui, time_range, expanded)`：无生效条件 64，有生效条件按文字宽 +44；
  - `time_filter_button(ui, rect, time_range, expanded) -> bool`：`ButtonKind::FilterActive` 强调色、chevron、hover 提示；
  - `FilterMetaResult` + `filter_meta_row(ui, start, id_prefix, match_mode, sort, source)`：关键词模式/排序固定 104，gap 12，来源 150 且可选；候选窗传 `None`，管理台传来源列表。
- `src/ui/picker.rs` 删除本地 `filter_button_label` 和整段 chevron/宽度代码，改用共享组件；管理台 `src/ui/pages/history.rs` 同步改为动态宽度 + 共享按钮，去掉蓝点，meta 行从按钮右侧 +12 开始，并应用共享组件返回的 match/sort/source。
- 修复 `src/ui/theme.rs::input_focused()` 在最高磨砂不透明度下 `card_alpha() + 8` 的 u8 溢出（`saturating_add(8)`），避免 debug 预览 panic、release 下焦点色 alpha 回绕。
- `src/ui/preview.rs` 允许 `CLIPNEST_PREVIEW_TIME` 同时作用于管理台预览，便于对比截图。

### 校验
- 更新 `filter_button_reflects_active_time_range` 调用共享 `widgets::time_filter_label`；新增 `input_focused_alpha_saturates_at_full_glass`。
- `cargo test --release`：**249 passed / 0 failed / 1 ignored**。
- `--ui-preview manager` + `CLIPNEST_PREVIEW_TIME=7` 截图 `docs/screenshots/manager-filter.png`：管理台显示「筛选 · 近7天」强调色按钮，无蓝点；关键词/排序/来源与候选窗同宽同间距。
- release 构建 + 9 尺寸图标 patch：exe 13,298,176B，SHA-256 `31DC9590ACBA4EA8EDF995E119768418EAB3F3FF082E68E9702217A9AD0F0A91`；`--selftest` 15 项 PASS。

## 54. v1.13.9（2026-11-06，中文字体改为微软雅黑）

### 用户反馈
- 用户 m19464：「UI字体是微软雅黑吗？怎么看起来不够黑，不是很清晰的样子。」
- 实际启动日志：`已加载中文字体：Deng.ttf`，即中文使用 **等线（DengXian）**，不是微软雅黑；西文仍使用 egui 自带的 Ubuntu-Light。等线字形偏细，叠加浅色/半透明背景后看起来发灰、不够清晰。

### 修复
- `src/ui/fonts.rs`：
  - 候选中文字体改为 `msyh.ttc`（索引 0，**Microsoft YaHei UI**）→ `Deng.ttf` → `simhei.ttf` → `simsun.ttc`，保证没有雅黑时仍可回退，不出现方块。
  - 新增西文字体加载：`segoeui.ttf`（Windows 原生 Segoe UI）优先，中文缺字时回退到 `msyh.ttc`。
  - Proportional 族顺序为 `[segoeui.ttf, msyh.ttc, egui 默认字体]`；Monospace 族保留 egui 等宽字体，中文追加到末尾。
  - 仍使用只读 mmap 零拷贝，不增加私有内存；字体缺失只跳过、不 panic。
- `candidate_order_matches_spec` 改为 `candidate_order_prefers_yahei`，断言雅黑优先且 ttc 索引为 0。

### 校验
- `cargo test --release`：**249 passed / 0 failed / 1 ignored**。
- `--ui-preview manager` 截图 `docs/screenshots/manager-filter.png`：中文为微软雅黑、西文为 Segoe UI，字形明显比等线饱满；列表/按钮/下拉均未出现截断或错位。
- release 构建 + 9 尺寸图标 patch：exe 13,299,712B，SHA-256 `0776FE81216C2E2F77D01015FECA8791CF93C7EDCE354F3952B4A8A243341C4F`；`--selftest` 15 项 PASS。

## 55. v1.13.10（2026-11-06，西文回退 + 空格预览开关）

### 用户反馈
- 用户 m19565：「1. 字母还是用 egui 自带的 Ubuntu-Light试试吧。目前的看着有点发虚。 2. 关于预览，新增一个快捷键，空格触发，再次空格可以关闭预览窗口」。

### 修改
- `src/ui/fonts.rs`：撤销 Segoe UI 西文优先，比例字体恢复「egui 自带 Ubuntu-Light 在前、微软雅黑 UI 在末尾回退」；中文仍为 `msyh.ttc` 索引 0，缺失时按等线 → 黑体 → 宋体回退。`load_named` 保留为共用加载入口。
- `src/ui/keys.rs`：`PickerCommand` 新增 `TogglePreview`；`route_picker_keys_ext(..., allow_space)` 在允许时消费裸 `Space`；原 `route_picker_keys` 包装为 `allow_space=true`，保持既有测试兼容；`PICKER_HINT` 改为「↑↓ 选择 · Enter 粘贴 · 空格 预览 · ←→ 切分类」。
- `src/ui.rs`：`UiState` 新增 `picker_preview_manual`；`set_preview_on_select` 与 `enter_picker` 重置该标记；切换选中条目时在预览绘制中复位。
- `src/ui/picker.rs`：
  - 搜索框聚焦且已有文本时 `allow_space=false`，空格留给分词输入；搜索框为空或未聚焦时空格用于预览。
  - `toggle_preview`：打开时设置 `picker_preview_manual`、清除 dismissed、写入 `picker_preview_id/at`；再次按空格关闭当前预览并记录 dismissed。
  - 自动预览关闭时仍允许空格临时查看；切换条目后回到自动策略。`Esc` 先关预览的逻辑兼容手动预览。

### 校验
- 新增 `route_space_toggles_preview_only_when_allowed`、`space_toggles_manual_preview_even_when_auto_preview_off`。
- `cargo test --release`：**251 passed / 0 failed / 1 ignored**。
- `--ui-preview picker` 确认底部提示条显示「空格 预览」且不挤压「共 N 条 / 预览开 / 齿轮」；字体日志为 `已加载中文字体：msyh.ttc`，西文恢复 Ubuntu-Light。
- release 构建、9 尺寸图标 patch 与 `--selftest` 结果见发布记录。

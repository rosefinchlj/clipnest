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

## 13. v1.7.1（2026-11-02，白底/浅色桌面候选窗文字可读性）

### 用户反馈
- 候选窗叠在白色/浅色窗口上时，整块面板被透成浅灰，标题、预览、时间、过滤胶囊都几乎看不清。

### 根因
- 磨砂透明度可低至 35%，窗口底与 `window_bg` 一起变透明后，列表行是**透明行**，白字直接叠在白/浅色背景上；
  辅助文本（`text_weak`）本身对比度也偏低。

### 修复
- 新增 `theme::content_bg()`：候选窗内容区在窗口磨砂层之上再铺一层**最低不透明度底**
  （深色 ≥ 200/255、浅色 ≥ 224/255），与用户滑杆值无关；外层边缘/圆角仍保留 Acrylic。
- `src/ui/picker.rs` 在绘制拖拽点/搜索/列表前铺 `content_bg()`，所有列表行不再直接叠在桌面背景上。
- 深色主题辅助文本整体提亮：`text_secondary #8B98A9 → #A5B2C4`、`text_weak #5C6878 → #7F8C9E`；
  行相对时间、搜索占位、空状态/加载文案从 `text_weak` 提升为 `text_secondary`。
- 设置页磨砂说明改为「外层越透，内容区自动保留最低不透明度」；截图更新 `docs/screenshots/picker.png`、
  `picker-transparent.png`（实际在白色窗口上以 35% 透明度截图验证）。

### 校验
- `cargo test --release` 142 passed / 0 failed（连续 5 次全绿；主题用例用 `TEST_LOCK: Mutex<()>`
  串行化全局主题状态，修复新增断言后的偶发并行失败）；`cargo build --release` 0 warnings；
  实机截图：白色窗口背景 + 35% 磨砂下，标题/预览/时间/胶囊均可读。

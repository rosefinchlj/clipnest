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

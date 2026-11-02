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

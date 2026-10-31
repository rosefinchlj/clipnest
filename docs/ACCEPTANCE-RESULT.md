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

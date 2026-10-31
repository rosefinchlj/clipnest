# ClipNest P0 能力验证结论（Win+V / 亚克力 / 托盘 / 热键）

- 日期：2026-10-31
- 验证环境：Windows 11 build 26300 x64，Session 1 真实桌面，进程**非提权**；
  用户机器上正在运行 **Ditto**（现有第三方剪贴板工具）；`HKCU\Software\Microsoft\Clipboard\EnableClipboardHistory = 1`
  （系统剪贴板历史开启）。
- 负责：win32-dev；产物与证据见文末。
- 状态：P0 风险验证完成，结论已用于 `src/sys/**` 实现决策。

## 1. 一句话结论

| 能力 | 结论 | 生效方案 |
|---|---|---|
| **L1** `RegisterHotKey(MOD_WIN,'V')` | ❌ 失败：`GetLastError = 1409 (ERROR_HOTKEY_ALREADY_REGISTERED)`，被系统剪贴板历史占用 | 不可行，跳过 |
| **L2** `WH_KEYBOARD_LL` 吞 Win+V | ✅ 钩子安装成功，注入的 Win+V 被完整捕获并吞掉 V down/up | **采用**（+开始菜单补偿） |
| 开始菜单误弹 | ⚠️ **无法用 SendInput 复现**（Windows 忽略注入的 Win 键组合，注入 Win 单击/注入 Win+V 均不弹开始菜单），需物理按键人工验收 | 已实现补偿；验收步骤见 §5 |
| 备用快捷键 `Ctrl+`` | ❌ 失败 1409：**被 Ditto 占用** | 需降级链（Lead 已定：`Ctrl+Shift+V` → `` Alt+` `` → `Ctrl+Alt+Shift+V`） |
| RegisterHotKey/WM_HOTKEY 通路 | ✅ 空闲组合 `Ctrl+Alt+Shift+F10` 注册成功，SendInput 触发 `WM_HOTKEY` 成功 | 通路可用 |
| 亚克力 | ✅ `window_vibrancy::apply_acrylic` 返回 Ok（截图见证据）；Lead 已实测 `apply_tabbed`（Win11 更佳） | 生产用 `apply_tabbed`，回退 acrylic/半透明深色 |
| tray-icon | ✅ eframe 主线程创建成功（8 项菜单，`assets/icon.png` 加载 OK）；Sys 线程创建也成功；模拟托盘点击后菜单事件可收到 | Sys 线程创建 + 菜单事件转发 |

## 2. 证据与实验数据

### 2.1 L1 / 备用热键（`target/p0/a_hotkey.log`、`a_hotkey.json`）

```
HOTKEY_REG|id=1 label=Ctrl+`             result=fail err=1409
HOTKEY_REG|id=2 label=Win+V              result=fail err=1409（ERROR_HOTKEY_ALREADY_REGISTERED）
HOTKEY_REG|id=3 label=Ctrl+Alt+Shift+F10 result=ok
HOTKEY_HIT|id=3（SendInput 注入后收到 WM_HOTKEY）
```

- 系统剪贴板历史开启时 Win+V 被系统占用 → **L1 恒失败**，且即使把 `EnableClipboardHistory` 改成 0，
  explorer 也可能保留 Win+V 处理，L1 不能作为主方案。
- **`Ctrl+`` 在本机被 Ditto 占用**（用户现有工具），因此备用快捷键必须带降级链 + 冲突诊断，
  否则默认配置直接不可用。

### 2.2 L2 钩子吞 Win+V（`target/p0/manual.log`、`fix.log`、`hook_*.json`）

注入 Win+V 全序列（Win down → V down → V up → Win up）后钩子日志（`swallow_v`）：

```
HOOK_EVENT|vk=0x5B down  injected=1 win_down=false
HOOK_EVENT|vk=0x56 down  injected=1 win_down=true
HOOK_SWALLOW|v_down（Win+V 命中）
HOOK_EVENT|vk=0x56 up    injected=1 win_down=true
HOOK_SWALLOW|v_up（Win+V 命中）
HOOK_EVENT|vk=0x5B up    injected=1 win_down=true
```

- 钩子能正确识别「Win 按住 + V」并吞掉 V down/up；其它按键（未命中策略）原样放行。
- 只吞 V、Win up 放行时：`GetAsyncKeyState(VK_LWIN)` 序列结束后正常为「抬起」，无粘滞。

### 2.3 开始菜单补偿（`swallow_v_winup` / `swallow_v_winup_fix`）

| 模式 | V down/up | Win up | 序列后 LWIN 异步状态 | 开始菜单 |
|---|---|---|---|---|
| `swallow_v`（只吞 V） | 吞 | 放行 | 正常（up） | 注入测不出（见下） |
| `swallow_v_winup`（吞 V+Win up） | 吞 | 吞 | **粘住（down）** | 无 |
| `swallow_v_winup_fix`（吞 V+Win up 后 30ms 注入 Win up） | 吞 | 吞 | **正常（up）** ✅ | 无 ✅ |

补偿修复日志：

```
HOOK_SWALLOW|win_up（开始菜单补偿）
HOOK_EVENT|vk=0x5B up injected=1 win_down=false     ← 30ms 后注入的 Win up
HOOK_COMPENSATE|inject_win_up（清除粘住的 Win 键状态）
```

**结论：**「吞 V + 吞 Win up + 30ms 后注入一次 Win up」可以同时做到
① 系统看不到「干净的 Win 单击」（不弹开始菜单）；② 清除被吞 Win up 造成的键状态粘滞。
注入的 Win up 不会被 shell 当作开始菜单触发（见 §2.4 限制），因此是安全的补偿。

### 2.4 ⚠️ 自动测试的边界：注入的 Win 键组合被 Windows 忽略

实测（`p0_keys.ps1` + 窗口枚举）：

- 注入一次「Win 单击」→ **不弹开始菜单**；
- 注入 Win+V（无钩子时）→ **不弹系统剪贴板历史**；
- 因此「物理按键吞 V 后开始菜单是否误弹 / 系统剪贴板历史是否被彻底拦住」**无法用 SendInput 自动复现**，
  必须物理按键人工验收（§5）。
- 推论：L2 方案能够拦截注入事件是确定的；对**物理**输入的拦截能力由同一 LL 钩子路径保证，
  但 explorer 对物理 Win 键状态的跟踪行为只能在实机人工确认。

### 2.5 亚克力 / 托盘（`target/p0/p0-spike-window.png`、`a_hotkey.json`、`tray_menu.json`）

- `apply_acrylic(frame, Some((14,17,22,210)))` 返回 **Ok**；窗口为透明无边框 always-on-top。
  截图 `target/p0/p0-spike-window.png`（560×420）。注：spike 未加载中文字体，截图中文显示为方框（正式 UI 有字体加载）。
- Lead 已用 `apply_tabbed`（Win11 build 26300）实测成功，P0 结论：**生产首选 tabbed，acrylic 回退**。
- tray-icon 0.26.1：
  - eframe 主线程（`update` 首帧）创建成功：8 项菜单（打开面板/管理历史/暂停记录/清空历史/设置/开机自启/退出 + 分隔符），
    `assets/icon.png` 加载成功；
  - headless 模式下由 Sys 线程创建也成功（说明正式实现「托盘放 Sys 线程」可行）；
  - 通过向 tray-icon 隐藏窗口（类名 `tray_icon_app`）投递 `WM_USER_TRAYICON(6002)` + `WM_LBUTTONUP` 模拟托盘左键：
    菜单弹出 → 向菜单窗口（类名 `#32768`）投递 `VK_DOWN/VK_RETURN` → 收到 **`MENU_EVENT|id=open`**；
  - 退出时 drop `TrayIcon` 后图标移除、进程干净退出。

### 2.6 进程清理

所有 spike 进程均 `exit_clean=true`（日志 `CLEANUP|unhook=ok` / `sys_thread_exit` / 托盘 drop），
未残留热键、钩子；`join_with_timeout` 防止菜单未关闭时卡死。

## 3. 对 `src/sys/**` 实现的决策

1. **Win+V 三级策略**：启动时仍先尝试 L1（记录 `WinVStatus`），失败按 `WinVMode`：
   - `Auto`/`HookOnly` → L2 钩子（`LLKHF_INJECTED` 一律忽略，避免拦截 SendInput）；
   - L2 生效发 `WinVStatus::HookActive { compensated: true }`；
   - `Off` 或钩子安装失败 → `Unavailable { reason }`，备用键仍可用。
2. **补偿**：命中 Win+V 后吞 V down/up；Win up 到来时若本轮吞过 V → 吞 Win up，并由 **Sys 线程用
   `SetTimer` 30ms 后** `SendInput` 注入一次 Win up（不在钩子回调里做耗时操作；spike 用临时线程验证概念）。
3. **只接管 V**：仅在 `win_down && vk == V` 时吞键；其它 Win+字母组合（Win+E/R/D…）必须原样放行；
   钩子内不做日志写盘等耗时操作（spike 为了取证才写日志）。
4. **热键降级链**（Lead 决策）：用户配置值注册失败（1409）→ 依次尝试 `Ctrl+Shift+V` → `` Alt+` `` →
   `Ctrl+Alt+Shift+V`，首个成功者生效，发 `Event::HotkeyStatus { ok:true, reason: Some("…已临时使用…") }`；
   全失败才 `ok:false`；实际生效组合保存用于 `--diagnose`。
5. **托盘**：在 Sys 线程创建/持有（`TrayIcon` 是 `!Send`，必须与菜单同线程 drop）；菜单事件用
   `MenuEvent::set_event_handler` → 转发到 Sys 隐藏窗口消息泵处理；退出时 drop 移除图标。
   菜单事件映射：打开面板→`PickerRequested`、管理历史/设置→`ManagerRequested`、暂停/自启→改设置+`Toast`、
   清空→`ManagerRequested`+`Toast` 交 UI 确认、退出→`QuitRequested`。
6. **容错**：热键/钩子/托盘失败都只发状态事件，`SysHandle::spawn` 不返回 Err、不 panic
   （主程序默认隐藏启动，Sys 挂掉用户无法唤出界面）。

## 4. 遗留风险（需在 P4/验收处理）

| # | 风险 | 说明 / 对策 |
|---|---|---|
| 1 | **物理 Win+V 后开始菜单可能误弹** | 无法自动复现（§2.4）。已实现补偿；P4 请人工按物理 Win+V 验收（§5 步骤），若仍弹 → 提示用户在设置页关闭接管，或启用 L3 关闭系统剪贴板历史（`HKCU\Software\Microsoft\Clipboard\EnableClipboardHistory=0`）。 |
| 2 | 物理 Win+V 是否仍触发系统剪贴板历史（双弹窗） | 注入无法验证；若出现：L3 关闭系统历史 + 提示；L4 备用键兜底。 |
| 3 | 吞 Win up 后 explorer 对 Win 键「按住」的跟踪 | 已注入 Win up 复位系统键状态；物理场景下建议验收时连续按 Win 键、Win+E 确认无异常。 |
| 4 | `Ctrl+`` 被 Ditto 占用 | 已按 Lead 决策实现降级链；设置页需展示冲突提示（UI 用 `HotkeyStatus.reason`）。 |
| 5 | 用户环境中 Ditto 等第三方钩子/热键可能干扰 | 已实测本机与 Ditto 共存时钩子/热键通路正常；设置页保留诊断信息。 |

## 5. 物理按键验收步骤（P4 人工，约 30 秒）

1. 启动 ClipNest（默认隐藏），确认托盘图标在；
2. 在记事本按 **物理 Win+V** → 期望：本工具 Picker 弹出；**开始菜单不弹**；系统剪贴板历史不弹；
3. Esc 关闭 Picker，再按 **物理 Win 键** 一次 → 期望：开始菜单正常弹出（说明 Win 键状态未损坏）；
4. 再按 **物理 Win+E** → 期望：资源管理器正常打开（说明未吞其它 Win 组合）；
5. 备用键（降级链实际生效组合）同样验证一次；
6. 若 2 失败：设置页关闭「Win+V 接管」，仅用备用键；并记录现象到验收文档。

## 6. 产物与复跑

| 文件 | 说明 |
|---|---|
| `examples/p0_spike.rs` | 能力验证 spike（eframe 透明窗 + 亚克力 + 托盘 + 热键 + Win+V 钩子；支持 GUI/headless、四种钩子模式、模拟托盘点击） |
| `examples/p0_test.ps1` | 自动化测试脚本（SendInput、窗口/开始菜单检测、托盘菜单模拟、结果汇总 JSON） |
| `examples/p0_keys.ps1` | 手动按键注入/键盘状态小工具（调试补偿用） |
| `target/p0/*.log` / `*.json` | 每次运行的原始日志与结果（`summary.json` 为汇总） |
| `target/p0/p0-spike-window.png` | spike 窗口截图（亚克力效果 + 各能力状态） |

复跑：

```powershell
cargo build --example p0_spike
pwsh -NoProfile -ExecutionPolicy Bypass -File examples\p0_test.ps1   # 约 60s，结束自动清理钩子/热键/托盘
```

依赖：仅用已锁定依赖（eframe 0.32.3 / tray-icon 0.26.1 / window-vibrancy 0.8.1 / windows-sys 0.61.2），
未修改 `Cargo.toml`。`CF_*` 常量在 windows-sys 中属于未启用的 `Win32_System_Ole` feature，
`src/clipboard` 内用 ABI 稳定数值常量 + 注释实现，不新增 feature。

## 7. SysHandle 真机验证（Sys 域最小集交付时）

在**临时数据目录**（`target/sys-smoke`，不读写用户真实库）启动真实 `clipnest.exe`，验证 Sys 线程接线（详见 `examples/sys_smoke.ps1`）：

| 项目 | 结果 |
|---|---|
| Sys 线程 + 隐藏窗口 `ClipNestHidden` | ✅ 日志 `Sys 线程已启动，隐藏窗口 hwnd=...` |
| `AddClipboardFormatListener` | ✅ `剪贴板监听已注册（AddClipboardFormatListener）` |
| 备用快捷键冲突降级链 | ✅ 请求 `Ctrl+` ` 被 Ditto 占用 → 生效 `Ctrl+Shift+V`；当另一实例已占用 `Ctrl+Shift+V` 时继续降级到 `Alt+` `（链式降级验证） |
| Win+V L2 | ✅ L1 1409 → `Win+V 状态：键盘钩子生效（含开始菜单补偿）` |
| 托盘 | ✅ `托盘创建成功`（打开面板/管理历史/暂停/清空/设置/开机自启/退出） |
| 复制 → 采集 | ✅ `采集剪贴板：类型=文本 字节=31 hash=7150444a 来源=...`（日志仅 hash 前 8 位，无内容） |
| 热键 → `Event::PickerRequested` | ✅ `呼出面板（备用快捷键），target_hwnd=...`，Picker 窗口显示刚采集的条目（截图 `target/sys-smoke/picker.png`） |
| 自动粘贴（hide → restore 前台 → SendInput Ctrl+V） | ✅ **Lead 独占桌面 E2E 验证 PASS**：注入 Ctrl+Shift+V → Picker 取得前台（轮询确认 foreground==clipnest hwnd）→ 注入 Enter → Notepad 中出现复制的 E2E 文本（match=True）；此前本域冒烟的失败为焦点竞争，非功能缺陷 |
| 单实例（命名互斥体 + WM_COPYDATA 唤起） | ✅ **Lead E2E 验证 PASS**：第二个实例日志「检测到已有实例 → 唤起 → 退出」，进程数保持 1，首个实例日志 `收到第二实例唤起消息（WM_COPYDATA）` + `呼出面板（第二实例唤起）` |
| 物理按键 Win+V（开始菜单/Win 键状态） | ⏳ **用户人工待验**（SentInput 无法复现注入 Win 键组合，见 §2.4）；按 §5 步骤 30 秒即可验收，不阻塞交付 |
| 退出清理 | 代码路径审查通过（鉴权：Drop 超时 detach、Sys 线程内清理）；真机优雅退出由 Lead E2E 复验 |

产物：`examples/sys_smoke.ps1`（真机冒烟；`-Quiet` 为不注入按键/不改剪贴板的静默版）、
`examples/paste_smoke.ps1` + `examples/paste_target.ps1`（自动粘贴受控目标冒烟，待恢复执行）。

## 8. 协作约定（真实桌面共享）

真实桌面同一时刻只允许一个 E2E 执行者：Set-Clipboard、注入全局按键、启动真实 clipnest 实例都属于
「独占桌面」操作。多人并行时约定：
1. 需要跑真机冒烟前先向 Lead 申请，完成后报告释放；
2. 回归用 `examples/sys_smoke.ps1 -Quiet`（只启动/读日志/kill，不改剪贴板、不注入按键）；
3. P0 证据脚本（`p0_test.ps1` 会注入 Win+V/Ctrl+Alt+Shift+F10）仅在独占桌面窗口期运行。

//! Sys 线程内部状态：剪贴板采集、热键/托盘事件、SysCmd 处理、退出清理。
//!
//! 仅由 [`crate::sys::thread`] 的窗口过程在 Sys 线程内调用（方法都要求 `&mut self`），
//! 不跨线程共享；本文件承载 Sys 线程的全部业务逻辑，`thread.rs` 只负责消息循环与生命周期。

#![allow(dead_code)]

use crate::clipboard;
use crate::config;
use crate::logging;
use crate::model::{now_ms, Event, RuntimeState, Settings, SysCmd, TempPause};
use crate::sys::thread::HOTKEY_ID_PICKER;
use crate::sys::{autostart, hotkey, paste, tray, util, SysDiag};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

/// Sys 线程内部状态（Box 后挂在窗口的 GWLP_USERDATA 上，仅本线程访问）。
pub(crate) struct SysState {
    pub(crate) hwnd: HWND,
    pub(crate) events: Sender<Event>,
    pub(crate) ctx: egui::Context,
    pub(crate) settings: Arc<Mutex<Settings>>,
    /// 备用呼出快捷键注册结果（含降级信息）。
    pub(crate) picker_hotkey: Option<hotkey::HotkeyRegistration>,
    /// Win+V 接管句柄。
    /// 托盘（None 表示创建失败；drop 即移除图标）。
    pub(crate) tray: Option<tray::TrayState>,
    /// 是否已收到 Shutdown。
    pub(crate) shutdown: bool,
    /// 运行状态快照（`--diagnose` 读取）。
    pub(crate) diag: Arc<Mutex<SysDiag>>,
    /// 内部命令队列：bridge 线程转投递，`WM_APP_CMD` 只作为唤醒信号。
    /// 不把指针放进 wparam，避免其它进程向隐藏窗口 PostMessage 伪造指针导致崩溃。
    pub(crate) cmd_rx: Receiver<SysCmd>,
    /// 最近一个不属于本进程的前台窗口（粘贴目标兜底；托盘菜单/已唤出面板时使用）。
    pub(crate) last_external_fg: isize,
    /// 运行期临时暂停（A3）：不写入 settings.json，重启即失效。
    pub(crate) temp_pause: TempPause,
}

impl SysState {
    // -----------------------------------------------------------------------
    // 事件辅助
    // -----------------------------------------------------------------------

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }

    /// 把管理台子进程推来的设置写入共享配置并转给 UI 线程应用（快捷键/暂停等）。
    pub(crate) fn apply_external_settings(&mut self, settings: Box<crate::model::Settings>) {
        let was_paused = self.settings_snapshot().pause_capture;
        let paused = settings.pause_capture;
        self.with_settings(|s| *s = (*settings).clone());
        // 管理台开关「暂停记录」：关闭时要一并清掉运行期临时暂停，否则用户
        // 看到开关已关、托盘勾选也消失，剪贴板却仍被临时暂停挡住。
        if was_paused != paused {
            if !paused && !matches!(self.temp_pause, TempPause::Off) {
                self.temp_pause = TempPause::Off;
                self.emit(Event::TempPauseChanged(TempPause::Off));
            }
            self.sync_tray_pause_state();
        }
        let _ = self.events.send(Event::SettingsUpdated(settings));
        self.ctx.request_repaint();
    }

    fn toast(&self, msg: impl Into<String>) {
        self.emit(Event::Toast(msg.into()));
    }

    pub(crate) fn update_diag(&self, f: impl FnOnce(&mut SysDiag)) {
        let mut guard = match self.diag.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard);
    }

    fn settings_snapshot(&self) -> Settings {
        match self.settings.lock() {
            Ok(s) => s.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn with_settings<R>(&self, f: impl FnOnce(&mut Settings) -> R) -> R {
        let mut guard = match self.settings.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }

    fn save_settings(&self) {
        let snapshot = self.settings_snapshot();
        if let Err(err) = config::save_settings(&snapshot) {
            logging::warn(&format!("保存设置失败：{err}"));
        }
    }

    // -----------------------------------------------------------------------
    // 剪贴板采集
    // -----------------------------------------------------------------------

    /// `WM_CLIPBOARDUPDATE`：读取 → 归一化 → `Event::Captured`。
    pub(crate) fn on_clipboard_update(&mut self) {
        self.refresh_temp_pause();
        let settings = self.settings_snapshot();
        if settings.pause_capture {
            logging::debug("剪贴板有变化，但已暂停记录，跳过");
            return;
        }
        if self.temp_pause.is_active() {
            logging::debug("剪贴板有变化，但处于临时暂停，跳过");
            return;
        }

        let raw = match clipboard::read_clipboard() {
            Ok(raw) => raw,
            Err(err) => {
                // 剪贴板被占用/读取失败：跳过本次，不影响后续
                logging::warn(&format!("读取剪贴板失败（跳过本次采集）：{err}"));
                return;
            }
        };
        if raw.ignore {
            logging::debug("剪贴板含忽略标记（Clipboard Viewer Ignore / 本程序回写），跳过");
            return;
        }

        let source_app = util::foreground_app_name();
        if let Some(app) = &source_app {
            if settings
                .ignore_apps
                .iter()
                .any(|name| name.eq_ignore_ascii_case(app))
            {
                logging::debug(&format!("来源应用 {app} 在忽略名单中，跳过"));
                return;
            }
        }

        let limits = clipboard::CaptureLimits::from_settings(&settings);
        let Some(mut input) = clipboard::normalize(raw, &limits) else {
            logging::debug("剪贴板内容为空/纯空白/超限，跳过");
            return;
        };
        input.source_app = source_app;
        input.source_title = util::foreground_window_title();
        input.captured_at = crate::model::now_ms();

        // 隐私：日志只记录类型/大小/hash 前 8 位，绝不记录内容
        let hash_prefix: String = input.hash.chars().take(8).collect();
        logging::log(&format!(
            "采集剪贴板：类型={} 字节={} hash={hash_prefix} 来源={:?}",
            input.kind.label(),
            input.byte_len,
            input.source_app
        ));
        self.emit(Event::Captured(input));
    }

    // -----------------------------------------------------------------------
    // 热键 / 托盘事件
    // -----------------------------------------------------------------------

    pub(crate) fn request_picker(&mut self, source: &str) {
        let mut target_hwnd = util::foreground_window_handle();
        let self_pid = unsafe { GetCurrentProcessId() };
        // 托盘菜单/面板已打开等场景，当前前台可能是本进程窗口（托盘隐藏窗口、Picker 本身），
        // 不能作为粘贴目标：退回最近一次记录的外部前台窗口。
        if target_hwnd == 0
            || util::window_pid(target_hwnd) == self_pid
            || util::is_clipnest_process(util::window_pid(target_hwnd))
        {
            target_hwnd = self.last_external_fg;
        }
        logging::log(&format!("呼出面板（{source}），target_hwnd={target_hwnd}"));
        self.emit(Event::PickerRequested { target_hwnd });
    }

    /// 管理台子进程请求唤出候选窗：显式指定粘贴目标（管理台打开前的前台窗口）。
    pub(crate) fn request_picker_with_target(&mut self, target: isize, source: &str) {
        let target_hwnd = if target != 0
            && util::window_pid(target) != 0
            && !util::is_clipnest_process(util::window_pid(target))
        {
            target
        } else if self.last_external_fg != 0 {
            self.last_external_fg
        } else {
            0
        };
        logging::log(&format!("呼出面板（{source}），target_hwnd={target_hwnd}"));
        self.emit(Event::PickerRequested { target_hwnd });
    }

    /// 周期记录外部前台窗口（由 `WM_TIMER` 每 300ms 调用）。
    pub(crate) fn update_foreground(&mut self) {
        // 借这个 300ms 的定时器顺带检查临时暂停是否到期（A3）。
        self.refresh_temp_pause();
        let fg = util::foreground_window_handle();
        if fg == 0 {
            return;
        }
        let self_pid = unsafe { GetCurrentProcessId() };
        let fg_pid = util::window_pid(fg);
        if fg_pid != self_pid && !util::is_clipnest_process(fg_pid) {
            self.last_external_fg = fg;
        }
    }

    /// 右键托盘图标：在 Sys 线程用 muda 弹出菜单（点击项会触发 MenuEvent → WM_APP_MENU）。
    pub(crate) fn on_tray_right_click(&mut self) {
        if let Some(tray) = &self.tray {
            tray.show_menu(self.hwnd);
        } else {
            logging::warn("托盘菜单不可用：托盘未创建");
        }
    }

    /// UI 子进程通过 WM_COPYDATA 请求守护进程退出。
    pub(crate) fn emit_quit_request(&mut self) {
        self.emit(Event::QuitRequested);
    }

    pub(crate) fn on_hotkey(&mut self, id: i32) {
        match id {
            HOTKEY_ID_PICKER => self.request_picker("备用快捷键"),
            _ => logging::debug(&format!("未知 WM_HOTKEY id={id}")),
        }
    }

    pub(crate) fn on_tray_menu(&mut self, code: u32) {
        match code {
            tray::MENU_OPEN => self.request_picker("托盘菜单"),
            tray::MENU_MANAGER => {
                logging::log("托盘：打开管理历史");
                self.emit(Event::ManagerRequested {
                    target_hwnd: self.last_external_fg,
                });
            }
            tray::MENU_PAUSE => self.toggle_pause(),
            tray::MENU_PAUSE_15 => self.set_temp_pause(TempPause::Timed {
                until_ms: now_ms() + 15 * 60_000,
            }),
            tray::MENU_PAUSE_60 => self.set_temp_pause(TempPause::Timed {
                until_ms: now_ms() + 60 * 60_000,
            }),
            tray::MENU_PAUSE_RESTART => self.set_temp_pause(TempPause::UntilRestart),
            tray::MENU_RESUME => self.resume_capture(),
            tray::MENU_CLEAR => {
                // 清空需要 UI 二次确认（spec §6.4）
                self.emit(Event::ManagerRequested {
                    target_hwnd: self.last_external_fg,
                });
                self.toast("清空历史请在管理台确认");
            }
            tray::MENU_SETTINGS => {
                logging::log("托盘：打开设置");
                self.emit(Event::SettingsRequested {
                    target_hwnd: self.last_external_fg,
                });
            }
            tray::MENU_AUTOSTART => self.toggle_autostart(),
            tray::MENU_QUIT => {
                logging::log("托盘：请求退出");
                self.emit(Event::QuitRequested);
            }
            other => logging::debug(&format!("未知托盘菜单动作 {other}")),
        }
    }

    pub(crate) fn on_tray_click(&mut self, code: u32) {
        if code == tray::TRAY_LEFT_CLICK {
            self.request_picker("托盘单击");
        }
    }

    fn toggle_pause(&mut self) {
        self.refresh_temp_pause();
        let permanent = self.settings_snapshot().pause_capture;
        let temp = self.temp_pause.is_active();
        if permanent || temp {
            // 托盘勾选项表示「当前是否暂停采集」：再次点击取消任何形式的暂停。
            // 这样临时暂停后点「暂停记录」也能立即恢复，而不是被临时暂停继续挡住。
            logging::log("暂停记录：关（清除永久/临时暂停）");
            self.resume_capture();
            return;
        }
        self.with_settings(|s| s.pause_capture = true);
        self.save_settings();
        // 设置页开关也要跟着托盘动作刷新（主面板是同一进程，但 UiState 不共享）。
        self.emit(Event::SettingsUpdated(Box::new(self.settings_snapshot())));
        self.sync_tray_pause_state();
        logging::log("暂停记录：开");
        self.toast("已暂停记录剪贴板");
    }

    // -----------------------------------------------------------------------
    // 临时暂停（A3：运行期生效，不写入 settings.json）
    // -----------------------------------------------------------------------

    /// 当前是否处于临时暂停（顺带处理到期恢复）。
    pub(crate) fn temp_pause_active(&mut self) -> bool {
        self.refresh_temp_pause();
        self.temp_pause.is_active()
    }

    /// 设置临时暂停；`Off` 表示恢复记录。
    pub(crate) fn set_temp_pause(&mut self, pause: TempPause) {
        self.temp_pause = pause;
        logging::log(&format!("临时暂停：{}", pause.label()));
        self.emit(Event::TempPauseChanged(pause));
        self.sync_tray_pause_state();
        if matches!(pause, TempPause::Off) {
            self.toast("已恢复记录剪贴板");
        } else {
            self.toast(format!("已暂停记录：{}", pause.label()));
        }
    }

    /// 到期自动恢复（托盘/设置页会同步刷新）。
    pub(crate) fn refresh_temp_pause(&mut self) {
        let TempPause::Timed { until_ms } = self.temp_pause else {
            return;
        };
        if now_ms() < until_ms {
            return;
        }
        self.temp_pause = TempPause::Off;
        logging::log("临时暂停已到期，恢复记录");
        self.emit(Event::TempPauseChanged(TempPause::Off));
        self.sync_tray_pause_state();
        self.toast("临时暂停已结束，已恢复记录剪贴板");
    }

    /// 恢复所有暂停（永久暂停 + 临时暂停）。
    fn resume_capture(&mut self) {
        let was_permanent = self.with_settings(|s| {
            let was = s.pause_capture;
            s.pause_capture = false;
            was
        });
        if was_permanent {
            self.save_settings();
            self.emit(Event::SettingsUpdated(Box::new(self.settings_snapshot())));
        }
        let had_temp = !matches!(self.temp_pause, TempPause::Off);
        self.temp_pause = TempPause::Off;
        if was_permanent {
            if let Some(tray) = &self.tray {
                tray.set_paused(false);
            }
        }
        self.sync_tray_pause_state();
        if was_permanent || had_temp {
            self.emit(Event::TempPauseChanged(TempPause::Off));
            self.toast("已恢复记录剪贴板");
        }
    }

    /// 把暂停状态同步到托盘（勾选/菜单文字/tooltip），并写运行期状态供管理台轮询。
    pub(crate) fn sync_tray_pause_state(&self) {
        let permanent = self.settings_snapshot().pause_capture;
        let runtime = RuntimeState::new(permanent, self.temp_pause);
        if let Some(tray) = &self.tray {
            // 勾选项表达「当前是否暂停采集」，临时暂停时也应显示为勾选。
            tray.set_paused(runtime.is_paused());
            tray.set_temp_pause(self.temp_pause, permanent);
            tray.set_pause_tooltip(self.temp_pause, permanent);
        }
        if let Err(err) = config::write_runtime_state(&runtime) {
            logging::debug(&format!("写运行期暂停状态失败：{err}"));
        }
    }

    fn toggle_autostart(&mut self) {
        let enabled = !autostart::get();
        match autostart::set(enabled) {
            Ok(()) => {
                self.with_settings(|s| s.autostart = enabled);
                self.save_settings();
                if let Some(tray) = &self.tray {
                    tray.set_autostart(enabled);
                }
                self.toast(if enabled {
                    "已开启开机自启"
                } else {
                    "已关闭开机自启"
                });
            }
            Err(err) => {
                logging::warn(&format!("切换开机自启失败：{err}"));
                self.toast(format!("设置开机自启失败：{err}"));
            }
        }
    }

    // -----------------------------------------------------------------------
    // SysCmd 处理
    // -----------------------------------------------------------------------

    /// 排空内部命令队列（`WM_APP_CMD` 处理入口；不直接读取 wparam）。
    pub(crate) fn drain_commands(&mut self) {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            self.handle_cmd(cmd);
        }
    }

    fn handle_cmd(&mut self, cmd: SysCmd) {
        match cmd {
            SysCmd::SetPause(paused) => {
                let permanent_changed = self.with_settings(|s| {
                    let changed = s.pause_capture != paused;
                    s.pause_capture = paused;
                    changed
                });
                // 关闭「暂停记录」等同于恢复采集：同时清掉运行期临时暂停，
                // 否则用户看到开关已关，剪贴板仍被临时暂停挡住。
                let had_temp = !matches!(self.temp_pause, TempPause::Off);
                if !paused && had_temp {
                    self.temp_pause = TempPause::Off;
                }
                if !permanent_changed && !(had_temp && !paused) {
                    return;
                }
                if permanent_changed {
                    self.save_settings();
                }
                self.sync_tray_pause_state();
                if had_temp && !paused {
                    self.emit(Event::TempPauseChanged(TempPause::Off));
                }
                logging::log(&format!("SysCmd::SetPause -> {paused}（临时暂停已清除={had_temp}）"));
                self.toast(if paused {
                    "已暂停记录剪贴板"
                } else {
                    "已恢复记录剪贴板"
                });
            }
            SysCmd::SetTempPause(pause) => self.set_temp_pause(pause),
            SysCmd::SetHotkeys { picker } => self.apply_hotkeys(picker),
            SysCmd::SetAutostart(enabled) => match autostart::set(enabled) {
                Ok(()) => {
                    self.with_settings(|s| s.autostart = enabled);
                    self.save_settings();
                    if let Some(tray) = &self.tray {
                        tray.set_autostart(enabled);
                    }
                    self.toast(if enabled {
                        "已开启开机自启"
                    } else {
                        "已关闭开机自启"
                    });
                }
                Err(err) => {
                    logging::warn(&format!("设置开机自启失败：{err}"));
                    self.toast(format!("设置开机自启失败：{err}"));
                }
            },
            SysCmd::WriteClipboard(payload) => {
                match clipboard::write_clipboard(&payload, &config::data_dir()) {
                    Ok(()) => logging::debug("已写剪贴板（不自动粘贴）"),
                    Err(err) => {
                        logging::warn(&format!("写剪贴板失败：{err}"));
                        self.toast(format!("写剪贴板失败：{err}"));
                    }
                }
            }
            SysCmd::Paste {
                payload,
                target_hwnd,
            } => self.handle_paste(payload, target_hwnd),
            SysCmd::Shutdown => {
                logging::log("收到 Shutdown，准备退出 Sys 线程");
                self.shutdown = true;
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
                }
            }
        }
    }

    fn apply_hotkeys(&mut self, picker: crate::model::HotkeyChord) {
        self.with_settings(|s| {
            s.hotkey_picker = picker;
        });

        // UI 任何设置变更都会发 SetHotkeys；这里按「请求值是否真的变化」去重，
        // 避免无谓地反复注销/注册热键、装卸 Win+V 钩子。
        let picker_changed = match &self.picker_hotkey {
            Some(existing) => existing.requested != picker,
            None => true,
        };
        if !picker_changed {
            logging::debug("SetHotkeys：请求值未变化，跳过重注册");
            return;
        }

        // 备用呼出键：注销旧的 → 按降级链重新注册
        let registration = hotkey::register_with_fallback(self.hwnd, HOTKEY_ID_PICKER, picker);
        let (ok, reason) = registration.status();
        logging::log(&format!(
            "热键重配：请求={} 生效={} ok={ok}",
            picker.label(),
            registration.effective.label()
        ));
        self.update_diag(|d| {
            d.hotkey_requested = Some(picker);
            d.hotkey_note = registration.note.clone();
            d.hotkey_effective = Some(registration.effective);
            d.hotkey_ok = ok;
        });
        self.picker_hotkey = Some(registration);
        self.emit(Event::HotkeyStatus { ok, reason });

    }

    fn handle_paste(&mut self, payload: crate::model::PastePayload, target_hwnd: isize) {
        logging::log(&format!("自动粘贴：开始 target={target_hwnd}"));
        // 1) 写剪贴板（失败不粘贴）
        if let Err(err) = clipboard::write_clipboard(&payload, &config::data_dir()) {
            logging::warn(&format!("自动粘贴：写剪贴板失败：{err}"));
            self.emit(Event::PasteResult {
                ok: false,
                reason: Some(format!("写剪贴板失败：{err}")),
            });
            return;
        }
        let paste_key = self.with_settings(|s| s.paste_key);
        // 2) 恢复前台 + SendInput
        match paste::restore_and_paste(target_hwnd, paste_key) {
            Ok(()) => {
                logging::log(&format!(
                    "自动粘贴完成：target={target_hwnd} 按键={}",
                    paste_key.label()
                ));
                self.emit(Event::PasteResult { ok: true, reason: None });
            }
            Err(err) => {
                logging::warn(&format!(
                    "自动粘贴失败：{err}（target={target_hwnd} 按键={}）",
                    paste_key.label()
                ));
                self.emit(Event::PasteResult {
                    ok: false,
                    reason: Some(err.reason().to_string()),
                });
            }
        }
    }

    // -----------------------------------------------------------------------
    // 退出清理
    // -----------------------------------------------------------------------

    /// 在 Sys 线程内释放所有 Win32 资源（顺序：监听/热键/钩子/托盘/窗口）。
    pub(crate) fn cleanup(&mut self) {
        logging::log("Sys 线程开始清理…");
        unsafe {
            windows_sys::Win32::System::DataExchange::RemoveClipboardFormatListener(self.hwnd);
        }
        if self.picker_hotkey.is_some() {
            hotkey::unregister(self.hwnd, HOTKEY_ID_PICKER);
            self.picker_hotkey = None;
        }
        // drop 托盘 → 从通知区移除图标
        self.tray = None;
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::KillTimer(
                self.hwnd,
                crate::sys::thread::FOREGROUND_POLL_TIMER_ID,
            );
        }
        logging::log("Sys 线程清理完成");
    }
}

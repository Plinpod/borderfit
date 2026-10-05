//! The engine thread: the only writer of foreign windows and taskbars while it is alive.
//!
//! States are `Idle` and `Fitted(Session)`. Fit and restore run synchronously on this thread,
//! so there are no in-between states and the hotkey is never busy. While fitted, a 150 ms tick
//! watches the process, re-derives the taskbar state and verifies the fitted rect; a move of the
//! tracked window (EVENT_OBJECT_LOCATIONCHANGE) is verified at once, because Chromium resizes a
//! fullscreen browser back to the whole monitor whenever another window on it is activated.
//!
//! Invariants (docs/ARCHITECTURE.md): the marker is on disk before the first mutation and deleted only
//! after a full restore; the taskbar is hidden only while fitted, the target is visible and
//! hide_taskbar is on; restore is idempotent and tolerates a missing window.

use crate::model::{
    AppError, FitProfile, FitState, FitStatus, NoEligibleReason, Notice, RestoreMarker,
    RestoreSummary, Settings, TaskbarMarker, TrackedWindow, WindowSummary, MARKER_SCHEMA_VERSION,
};
use crate::recovery::{self, Adopted, MarkerStore};
use crate::region;
use crate::taskbar;
use crate::win32::{
    self, ProcessHandle, ShowCmd, WindowInfo, WindowManager, ZOrder, DEFAULT_EX_STYLE_MASK,
    DEFAULT_STYLE_MASK, ERROR_ACCESS_DENIED, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE, WS_CAPTION, WS_EX_TOPMOST, WS_MAXIMIZE,
    WS_THICKFRAME,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Tick period while fitted.
pub const TICK: Duration = Duration::from_millis(150);
/// When the fitted rect is checked after a fit (ms after the fit or the last re-apply).
const VERIFY_DELAYS_MS: [u64; 3] = [150, 500, 2000];
/// Re-applies in one burst before the fit is reported as "size not accepted".
const MAX_REAPPLY: u8 = 4;
/// A window that held the target this long has stopped fighting: the next move starts a new
/// re-apply burst instead of counting against the old one. Games that enforce a size push back
/// within a few frames; a browser reset only follows the user switching windows.
const STABLE: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitSource {
    Hotkey,
    Tray,
    Ui,
}

pub enum Msg {
    /// Fit the foreground window when idle, restore when fitted (hotkey, tray).
    Toggle { src: FitSource },
    /// Fit a specific window, or the foreground/z-order pick when `hwnd` is None.
    Fit { hwnd: Option<isize>, src: FitSource },
    /// Restore now; `done` is acknowledged afterwards.
    Restore { done: Option<SyncSender<()>> },
    /// New settings; a changed region re-fits the tracked window.
    Settings(Box<Settings>),
    /// From the hook thread: the tracked window was destroyed, hidden or cloaked.
    WinEvent { gen: u64, event: u32, hwnd: isize },
    /// Explorer (re)created its taskbar; rediscover and reconcile.
    TaskbarCreated,
    /// Restore if fitted, acknowledge, and stop the thread.
    Shutdown { done: SyncSender<()> },
}

/// Where the engine reports to. The Tauri implementation only emits events and posts closures
/// to the main thread; it never blocks.
pub trait EventSink: Send {
    fn status(&self, status: &FitStatus);
    fn notice(&self, notice: &Notice);
    /// A fit request failed with zero side effects (or was rolled back).
    fn fit_failed(&self, error: &AppError, src: FitSource);
}

/// Pid-scoped WinEvent hooks (implemented by the hook thread).
pub trait HookControl: Send {
    fn track(&self, pid: u32, hwnd: isize, gen: u64);
    fn untrack(&self);
}

/// Hook control that does nothing (tests, non-Windows builds).
pub struct NoHooks;

impl HookControl for NoHooks {
    fn track(&self, _pid: u32, _hwnd: isize, _gen: u64) {}
    fn untrack(&self) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestoreReason {
    User,
    ProcessExited,
    WindowGone,
    Shutdown,
    /// The app switched itself out of fullscreen (Esc in a browser video) and restored its own
    /// window, so only the taskbar and BorderFit's own touches are undone.
    LeftFullscreen,
}

/// When the next verify check is due and how many re-applies the current burst spent.
#[derive(Clone, Copy, Debug)]
struct Verify {
    due: Option<Instant>,
    step: usize,
    attempts: u8,
    /// Since when the window has held the target (None until a check confirms it).
    held_since: Option<Instant>,
}

impl Verify {
    fn start(now: Instant) -> Self {
        Self { due: Some(now + ms(VERIFY_DELAYS_MS[0])), step: 0, attempts: 0, held_since: None }
    }

    fn idle() -> Self {
        Self { due: None, step: VERIFY_DELAYS_MS.len(), attempts: 0, held_since: None }
    }
}

struct Session {
    gen: u64,
    tracked: TrackedWindow,
    profile: FitProfile,
    /// Device of the monitor the target was resolved on.
    monitor: String,
    process: ProcessHandle,
    taskbar_hidden: bool,
    target_visible: bool,
    verify: Verify,
    degraded: Option<AppError>,
    missing_window_logged: bool,
    /// When the fit (or adoption) happened; move log lines are timed from here.
    fitted_at: Instant,
}

pub struct Engine {
    wm: Box<dyn WindowManager>,
    sink: Box<dyn EventSink>,
    marker: Box<dyn MarkerStore>,
    hooks: Box<dyn HookControl>,
    emergency: &'static AtomicBool,
    settings: Settings,
    session: Option<Session>,
    gen: u64,
    own_integrity: Option<u32>,
}

impl Engine {
    pub fn new(
        wm: Box<dyn WindowManager>,
        sink: Box<dyn EventSink>,
        marker: Box<dyn MarkerStore>,
        hooks: Box<dyn HookControl>,
        emergency: &'static AtomicBool,
        settings: Settings,
    ) -> Self {
        let own_integrity = wm.integrity_level(wm.own_pid());
        Self { wm, sink, marker, hooks, emergency, settings, session: None, gen: 0, own_integrity }
    }

    /// Starts the engine loop on its own thread.
    pub fn spawn(self, rx: Receiver<Msg>) -> std::io::Result<JoinHandle<()>> {
        std::thread::Builder::new().name("engine".into()).spawn(move || self.run(rx))
    }

    pub fn is_fitted(&self) -> bool {
        self.session.is_some()
    }

    /// The loop: block while idle; while fitted, wake at least every tick.
    pub fn run(mut self, rx: Receiver<Msg>) {
        let mut next_tick = Instant::now() + TICK;
        loop {
            let msg = if self.session.is_some() {
                let wait = next_tick.saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match rx.recv() {
                    Ok(msg) => Some(msg),
                    Err(_) => break,
                }
            };
            if let Some(msg) = msg {
                if !self.handle(msg) {
                    break;
                }
            }
            if self.session.is_some() && Instant::now() >= next_tick {
                self.tick();
                next_tick = Instant::now() + TICK;
            }
        }
        log::info!("engine: stopped");
        // Drop restores if a session is still active.
    }

    /// Handles one message; returns false when the loop should stop.
    pub fn handle(&mut self, msg: Msg) -> bool {
        self.stand_down_if_emergency();
        match msg {
            Msg::Toggle { src } => {
                if self.session.is_some() {
                    self.restore(RestoreReason::User);
                } else {
                    self.fit_and_report(None, src);
                }
            }
            Msg::Fit { hwnd, src } => match &self.session {
                Some(s) if hwnd == Some(s.tracked.hwnd) => {}
                Some(s) => {
                    let error = AppError::Busy { title: s.tracked.title.clone() };
                    self.sink.fit_failed(&error, src);
                }
                None => self.fit_and_report(hwnd, src),
            },
            Msg::Restore { done } => {
                if self.session.is_some() {
                    self.restore(RestoreReason::User);
                } else {
                    self.emit_status();
                }
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
            Msg::Settings(settings) => self.apply_settings(*settings),
            Msg::WinEvent { gen, event, hwnd } => self.on_win_event(gen, event, hwnd),
            Msg::TaskbarCreated => self.on_taskbar_created(),
            Msg::Shutdown { done } => {
                self.restore(RestoreReason::Shutdown);
                let _ = done.send(());
                return false;
            }
        }
        true
    }

    // -----------------------------------------------------------------------------------------
    // Fit
    // -----------------------------------------------------------------------------------------

    fn fit_and_report(&mut self, hwnd: Option<isize>, src: FitSource) {
        if let Err(error) = self.fit(hwnd, src) {
            log::warn!("fit ({src:?}) failed: {error:?}");
            self.sink.fit_failed(&error, src);
        }
    }

    /// The fit sequence. Errors before the marker is written have zero side effects; errors
    /// after it roll everything back.
    fn fit(&mut self, requested: Option<isize>, src: FitSource) -> Result<(), AppError> {
        let info = self.choose_target(requested, src)?;
        self.check_access(&info)?;
        let saved = self.capture(&info)?;

        let monitors = self.wm.monitors();
        let resolved = region::resolve_on(&monitors, &self.settings.profile.region)?;
        let degraded = dpi_warning(&info, &resolved.monitor);
        let process = self.wm.open_process(info.pid)?;

        let mut tracked = TrackedWindow {
            hwnd: info.hwnd,
            pid: info.pid,
            tid: info.tid,
            process_created: process.created,
            exe: info.exe.clone(),
            title: info.title.clone(),
            class: info.class.clone(),
            saved,
            target: resolved.rect,
        };
        let hide = self.settings.profile.taskbar.hide_taskbar;
        if let Err(e) = self.write_marker(&tracked, hide) {
            self.wm.close_process(&process);
            return Err(e);
        }

        // Past this point every failure rolls back through the restore sequence.
        if let Err(e) = self.normalize_show_state(&mut tracked, hide) {
            self.rollback(&tracked, &process);
            return Err(e);
        }
        self.gen += 1;
        self.hooks.track(tracked.pid, tracked.hwnd, self.gen);

        self.wm.set_foreground(tracked.hwnd);
        let taskbar = &self.settings.profile.taskbar;
        let taskbar_hidden =
            taskbar::reconcile(&*self.wm, taskbar.hide_taskbar, taskbar.hide_secondary_taskbars);
        self.wm.set_foreground(tracked.hwnd);

        let style_warning = match self.apply_styles_and_rect(&tracked) {
            Ok(warning) => warning,
            Err(e) => {
                self.rollback(&tracked, &process);
                return Err(e);
            }
        };
        if let Some(fallback) = resolved.fallback {
            self.sink.notice(&Notice::from_error(fallback));
        }
        log::info!(
            "fit: \"{}\" ({}) pid {} -> {} on {} ({src:?})",
            tracked.title,
            tracked.exe,
            tracked.pid,
            tracked.target,
            resolved.monitor.device
        );
        self.session = Some(Session {
            gen: self.gen,
            tracked,
            profile: self.settings.profile.clone(),
            monitor: resolved.monitor.device,
            process,
            taskbar_hidden,
            target_visible: true,
            verify: Verify::start(Instant::now()),
            degraded: style_warning.or(degraded),
            missing_window_logged: false,
            fitted_at: Instant::now(),
        });
        self.emit_status();
        Ok(())
    }

    /// Fit step 1: the window to fit. The hotkey fits the foreground window only; tray and UI
    /// fits (where BorderFit itself is in front) take the first fittable window in z-order.
    fn choose_target(
        &self,
        requested: Option<isize>,
        src: FitSource,
    ) -> Result<WindowInfo, AppError> {
        let not_fittable = AppError::NoEligibleWindow { reason: NoEligibleReason::NotFittable };
        if let Some(hwnd) = requested {
            let info = self.root_info(hwnd)?;
            return if self.is_fittable(&info) { Ok(info) } else { Err(not_fittable) };
        }
        match self.wm.foreground().and_then(|h| self.root_info(h).ok()) {
            Some(info) if self.is_fittable(&info) => return Ok(info),
            Some(info) if src == FitSource::Hotkey => {
                let reason = if info.pid == self.wm.own_pid() {
                    NoEligibleReason::OwnWindow
                } else if win32::is_shell_class(&info.class) {
                    NoEligibleReason::ShellWindow
                } else {
                    NoEligibleReason::NotFittable
                };
                return Err(AppError::NoEligibleWindow { reason });
            }
            None if src == FitSource::Hotkey => {
                return Err(AppError::NoEligibleWindow { reason: NoEligibleReason::NoneFound });
            }
            _ => {}
        }
        self.first_fittable_in_z_order()
            .ok_or(AppError::NoEligibleWindow { reason: NoEligibleReason::NoneFound })
    }

    fn root_info(&self, hwnd: isize) -> Result<WindowInfo, AppError> {
        let info = self.wm.info(hwnd)?;
        if info.root != 0 && info.root != hwnd {
            return self.wm.info(info.root);
        }
        Ok(info)
    }

    fn is_fittable(&self, info: &WindowInfo) -> bool {
        info.pid != self.wm.own_pid()
            && info.visible
            && !info.cloaked
            && !info.is_tool_window()
            && !win32::is_shell_class(&info.class)
            && info.class != win32::UWP_HOST_CLASS
    }

    /// The window the user was in before clicking the tray or BorderFit's window.
    fn first_fittable_in_z_order(&self) -> Option<WindowInfo> {
        self.wm.top_level_windows().into_iter().find_map(|hwnd| {
            let info = self.wm.info(hwnd).ok()?;
            let candidate = (info.root == 0 || info.root == hwnd)
                && self.is_fittable(&info)
                && !info.iconic
                && !info.title.is_empty();
            candidate.then_some(info)
        })
    }

    /// Fit step 2: an elevated target can't be moved (UIPI). Checked before any mutation.
    fn check_access(&self, info: &WindowInfo) -> Result<(), AppError> {
        let needs_admin =
            || AppError::NeedsAdmin { exe: info.exe.clone(), title: info.title.clone() };
        if let (Some(theirs), Some(ours)) = (self.wm.integrity_level(info.pid), self.own_integrity)
        {
            if theirs > ours {
                return Err(needs_admin());
            }
        }
        match self.wm.probe_access(info.hwnd) {
            Err(AppError::Win32 { code: ERROR_ACCESS_DENIED, .. }) => Err(needs_admin()),
            other => other,
        }
    }

    /// Fit step 3: everything needed for an exact restore.
    fn capture(&self, info: &WindowInfo) -> Result<crate::model::SavedState, AppError> {
        let hwnd = info.hwnd;
        let (style, ex_style) = self.wm.styles(hwnd)?;
        let placement = self.wm.placement(hwnd)?;
        let rect = self.wm.rect(hwnd)?;
        let rect_provisional = info.iconic || info.zoomed || info.arranged;
        log::info!(
            "capture: \"{}\" style {style:#x} ex {ex_style:#x} rect {rect} showCmd {} provisional {rect_provisional} dpi {} {:?}",
            info.title,
            placement.show_cmd,
            info.dpi,
            info.dpi_awareness
        );
        Ok(crate::model::SavedState {
            style,
            ex_style,
            topmost: ex_style & WS_EX_TOPMOST != 0,
            placement,
            rect,
            rect_provisional,
            monitor: info.monitor.clone(),
            dpi: info.dpi,
            dpi_awareness: info.dpi_awareness,
        })
    }

    /// Fit step 7: a minimized, maximized or snapped window is restored first, then its real
    /// rect and style words are re-read and the marker is rewritten.
    fn normalize_show_state(
        &self,
        tracked: &mut TrackedWindow,
        hide: bool,
    ) -> Result<(), AppError> {
        if !tracked.saved.rect_provisional {
            return Ok(());
        }
        let hwnd = tracked.hwnd;
        // A window minimized from maximized comes back maximized: restore twice at most.
        for _ in 0..2 {
            self.wm.show(hwnd, ShowCmd::Restore)?;
            if !self.wm.is_zoomed(hwnd) && !self.wm.is_iconic(hwnd) {
                break;
            }
        }
        // Restoring clears WS_MAXIMIZE/WS_MINIMIZE; keep the words as of now so the restore
        // write does not set a state bit behind the window's back (placement carries the state).
        let (style, ex_style) = self.wm.styles(hwnd)?;
        tracked.saved.style = style;
        tracked.saved.ex_style = ex_style;
        tracked.saved.rect = self.wm.rect(hwnd)?;
        tracked.saved.rect_provisional = false;
        self.write_marker(tracked, hide)
    }

    /// Fit step 10: clear the border styles, square the corners, one SetWindowPos.
    /// Returns a `StyleRefused` warning when the window kept its styles.
    fn apply_styles_and_rect(&self, tracked: &TrackedWindow) -> Result<Option<AppError>, AppError> {
        let hwnd = tracked.hwnd;
        let saved = &tracked.saved;
        let (want_style, want_ex) = borderless_styles(saved.style, saved.ex_style);
        let mut warning = None;
        if (want_style, want_ex) != (saved.style, saved.ex_style) {
            let (got_style, got_ex) = self.wm.set_styles(hwnd, want_style, want_ex)?;
            if (got_style, got_ex) == (saved.style, saved.ex_style) {
                log::warn!("fit: styles unchanged ({got_style:#x})");
                warning = Some(AppError::StyleRefused { wanted: want_style, got: got_style });
            }
        }
        self.wm.dwm_square(hwnd, true);
        self.wm.set_pos(hwnd, tracked.target, ZOrder::Keep, true)?;
        Ok(warning)
    }

    /// Undo a half-applied fit and forget it.
    fn rollback(&self, tracked: &TrackedWindow, process: &ProcessHandle) {
        log::warn!("fit: rolling back \"{}\"", tracked.title);
        self.hooks.untrack();
        let alive = recovery::window_still_ours(&*self.wm, tracked);
        let failed = recovery::restore_sequence(&*self.wm, Some(tracked), alive);
        self.wm.close_process(process);
        if failed.is_empty() {
            self.marker.delete();
        }
    }

    // -----------------------------------------------------------------------------------------
    // Restore
    // -----------------------------------------------------------------------------------------

    fn restore(&mut self, reason: RestoreReason) {
        let Some(session) = self.session.take() else { return };
        self.hooks.untrack();
        let tracked = &session.tracked;
        let window_alive = match reason {
            RestoreReason::ProcessExited | RestoreReason::WindowGone => false,
            RestoreReason::LeftFullscreen => {
                // The app already put its window back; restoring the captured fullscreen
                // geometry would undo that. Only the square corners are BorderFit's.
                self.wm.dwm_square(tracked.hwnd, false);
                false
            }
            RestoreReason::User | RestoreReason::Shutdown => {
                recovery::window_still_ours(&*self.wm, tracked)
            }
        };
        log::info!("restore: \"{}\" ({reason:?}, window alive: {window_alive})", tracked.title);
        let failed = recovery::restore_sequence(&*self.wm, Some(tracked), window_alive);
        self.wm.close_process(&session.process);
        if failed.is_empty() {
            self.marker.delete();
        }
        self.emit_status();
        if !failed.is_empty() {
            let error = AppError::TaskbarRestoreFailed { hwnds: failed };
            self.sink.notice(&Notice::from_error(error));
        }
    }

    /// Continues a session a crashed run left behind (see `recovery::run_at_startup`).
    pub fn adopt(&mut self, adopted: Adopted) {
        let tracked = adopted.tracked;
        let process = match self.wm.open_process(tracked.pid) {
            Ok(p) if p.created == tracked.process_created => p,
            other => {
                log::warn!(
                    "adopt: process {} is gone ({other:?}); restoring the taskbar",
                    tracked.pid
                );
                if let Ok(p) = other {
                    self.wm.close_process(&p);
                }
                recovery::restore_sequence(&*self.wm, None, false);
                self.marker.delete();
                return;
            }
        };
        self.gen += 1;
        let marker_written = self.write_marker(&tracked, adopted.taskbar_hidden);
        if let Err(e) = marker_written {
            log::error!("adopt: {e}");
        }
        self.hooks.track(tracked.pid, tracked.hwnd, self.gen);
        log::info!(
            "adopt: tracking \"{}\" pid {} at {}",
            tracked.title,
            tracked.pid,
            tracked.target
        );
        let (cx, cy) = tracked.target.center();
        let monitor = region::monitor_at(&self.wm.monitors(), cx, cy)
            .map(|m| m.device.clone())
            .unwrap_or_default();
        self.session = Some(Session {
            gen: self.gen,
            tracked,
            profile: self.settings.profile.clone(),
            monitor,
            process,
            taskbar_hidden: adopted.taskbar_hidden,
            target_visible: true,
            verify: Verify::idle(),
            degraded: None,
            missing_window_logged: false,
            fitted_at: Instant::now(),
        });
        // The persisted region may have changed since the crash.
        self.refit();
        self.tick();
        self.emit_status();
    }

    // -----------------------------------------------------------------------------------------
    // Tick, verify, events
    // -----------------------------------------------------------------------------------------

    /// One watchdog pass while fitted: process exit, visibility, taskbar, verify.
    pub fn tick(&mut self) {
        if self.stand_down_if_emergency() {
            return;
        }
        let Some(session) = self.session.as_ref() else { return };
        let hwnd = session.tracked.hwnd;

        if self.wm.process_exited(&session.process) {
            self.restore(RestoreReason::ProcessExited);
            return;
        }
        if !self.wm.is_window(hwnd) && self.window_confirmed_gone(hwnd) {
            self.restore(RestoreReason::WindowGone);
            return;
        }

        let Some(session) = self.session.as_ref() else { return };
        let visible = self.wm.is_visible(hwnd) && !self.wm.is_iconic(hwnd);
        let taskbar = &session.profile.taskbar;
        let hidden = taskbar::reconcile(
            &*self.wm,
            visible && taskbar.hide_taskbar,
            taskbar.hide_secondary_taskbars,
        );
        let changed = visible != session.target_visible || hidden != session.taskbar_hidden;
        if hidden != session.taskbar_hidden {
            let tracked = session.tracked.clone();
            if let Err(e) = self.write_marker(&tracked, hidden) {
                log::error!("tick: {e}");
            }
        }
        if let Some(session) = self.session.as_mut() {
            session.target_visible = visible;
            session.taskbar_hidden = hidden;
        }
        let verify_changed = self.verify_if_due();
        if changed || verify_changed {
            self.emit_status();
        }
    }

    /// A false `IsWindow` alone is not trusted (UIPI); corroborate with the rect read failing
    /// with ERROR_INVALID_WINDOW_HANDLE. Safe because the tracked window passed `probe_access`.
    fn window_confirmed_gone(&mut self, hwnd: isize) -> bool {
        if matches!(self.wm.rect(hwnd), Err(AppError::WindowGone)) {
            return true;
        }
        if let Some(session) = self.session.as_mut() {
            if !session.missing_window_logged {
                log::warn!("tick: IsWindow failed for {hwnd:#x} but the window still answers");
                session.missing_window_logged = true;
            }
        }
        false
    }

    /// Checks the fitted rect and styles when a verify step is due; re-applies on mismatch and
    /// gives up with `ResizeRefused` after `MAX_REAPPLY` attempts. Returns true if the visible
    /// status changed.
    fn verify_if_due(&mut self) -> bool {
        let now = Instant::now();
        let Some(session) = self.session.as_ref() else { return false };
        // A minimized or hidden window has no meaningful rect; check once it is back.
        if !session.target_visible || session.verify.due.is_none_or(|due| now < due) {
            return false;
        }
        let tracked = &session.tracked;
        let hwnd = tracked.hwnd;
        let Ok(rect) = self.wm.rect(hwnd) else { return false };
        let (want_style, want_ex) = borderless_styles(tracked.saved.style, tracked.saved.ex_style);
        // Styles the window refused at fit time are a separate warning, not a retry reason.
        let styles_refused = matches!(session.degraded, Some(AppError::StyleRefused { .. }));
        let live_style = self.wm.styles(hwnd).map(|(style, _)| style).ok();
        let styles_ok = styles_refused
            || live_style
                .is_none_or(|style| style & (DEFAULT_STYLE_MASK & tracked.saved.style) == 0);
        let target = tracked.target;
        let mut verify = session.verify;

        if rect == target && styles_ok {
            verify.held_since.get_or_insert(now);
            // Checks driven by moves can arrive after the schedule ended: nothing more is due.
            verify.step += 1;
            verify.due = VERIFY_DELAYS_MS
                .get(verify.step)
                .map(|delay| now + ms(delay - VERIFY_DELAYS_MS[verify.step - 1]));
            self.session.as_mut().expect("fitted").verify = verify;
            return false;
        }

        // Held long enough before this move: a fresh burst (e.g. a browser reset on activation).
        if verify.held_since.is_some_and(|since| now.duration_since(since) >= STABLE) {
            verify.attempts = 0;
        }
        verify.held_since = None;

        if verify.attempts < MAX_REAPPLY {
            verify.attempts += 1;
            log::info!(
                "verify: got {rect} (style {:#x}), want {target}; re-applying ({})",
                live_style.unwrap_or(0),
                verify.attempts
            );
            if !styles_ok {
                let _ = self.wm.set_styles(hwnd, want_style, want_ex);
            }
            if let Err(e) = self.wm.set_pos(hwnd, target, ZOrder::Keep, true) {
                log::warn!("verify: re-apply failed: {e}");
            }
            verify.step = 1;
            verify.due = Some(now + ms(VERIFY_DELAYS_MS[1]));
            self.session.as_mut().expect("fitted").verify = verify;
            return true;
        }

        log::warn!("verify: giving up; the window keeps {rect} instead of {target}");
        verify.due = None;
        let session = self.session.as_mut().expect("fitted");
        session.verify = verify;
        session.degraded =
            Some(AppError::ResizeRefused { wanted: target, got: rect, attempts: verify.attempts });
        true
    }

    fn on_win_event(&mut self, gen: u64, event: u32, hwnd: isize) {
        let Some(session) = self.session.as_ref() else { return };
        if gen != session.gen || hwnd != session.tracked.hwnd {
            return;
        }
        if event == EVENT_OBJECT_LOCATIONCHANGE {
            self.on_location_change();
            return;
        }
        if !matches!(event, EVENT_OBJECT_DESTROY | EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED) {
            return;
        }
        if self.wm.process_exited(&session.process) {
            self.restore(RestoreReason::ProcessExited);
        } else if event == EVENT_OBJECT_DESTROY && self.window_confirmed_gone(hwnd) {
            self.restore(RestoreReason::WindowGone);
        } else {
            // Hidden or cloaked: the tick re-derives the taskbar state right away.
            self.tick();
        }
    }

    /// The tracked window moved or resized. If the app left fullscreen by itself, let it go;
    /// otherwise check now and put it back (our own `set_pos` lands here too and is a no-op).
    fn on_location_change(&mut self) {
        let Some(session) = self.session.as_ref() else { return };
        let tracked = &session.tracked;
        self.log_move(session);
        if self.left_fullscreen(tracked) {
            log::info!("\"{}\" left fullscreen by itself; letting it go", tracked.title);
            self.restore(RestoreReason::LeftFullscreen);
            return;
        }
        if matches!(session.degraded, Some(AppError::ResizeRefused { .. })) {
            return;
        }
        if let Some(session) = self.session.as_mut() {
            session.verify.due = Some(Instant::now());
        }
        if self.verify_if_due() {
            self.emit_status();
        }
    }

    /// One line per move of the tracked window, with timing, focus and mouse state: shows what a
    /// browser does around a click or a focus change, for bug reports. BorderFit's own `set_pos`
    /// echoes back as a move onto the target, so each round trip is visible too.
    fn log_move(&self, session: &Session) {
        let hwnd = session.tracked.hwnd;
        let rect = self.wm.rect(hwnd).map_or_else(|_| "?".to_string(), |r| r.to_string());
        let style = self.wm.styles(hwnd).map_or(0, |(style, _)| style);
        let focus = match self.wm.foreground() {
            Some(fg) if fg == hwnd => "target".to_string(),
            Some(fg) => self.wm.info(fg).map_or_else(|_| format!("{fg:#x}"), |i| i.class),
            None => "none".to_string(),
        };
        let button = if self.wm.left_button_down() { "down" } else { "up" };
        log::info!(
            "move: +{}ms {rect} style {style:#x} focus {focus} lbutton {button}",
            session.fitted_at.elapsed().as_millis()
        );
    }

    /// Fitted while frameless (fullscreen) and now framed or maximized again: BorderFit never
    /// adds frame bits to a window it captured frameless, so the app switched back to windowed
    /// mode itself (a browser video after Esc). `WS_THICKFRAME` counts on its own because
    /// Chrome's custom-drawn window is resizable but may have no caption bit.
    fn left_fullscreen(&self, tracked: &TrackedWindow) -> bool {
        let frame = WS_CAPTION | WS_THICKFRAME;
        let was_frameless = tracked.saved.style & frame == 0;
        let windowed_again =
            self.wm.styles(tracked.hwnd).is_ok_and(|(style, _)| style & (frame | WS_MAXIMIZE) != 0);
        was_frameless && windowed_again
    }

    fn on_taskbar_created(&mut self) {
        log::info!("taskbar: TaskbarCreated (Explorer restart or DPI change)");
        if let Some(session) = self.session.as_mut() {
            // A DPI change can move the window too: verify again from scratch.
            session.verify = Verify::start(Instant::now());
        }
        self.tick();
    }

    fn apply_settings(&mut self, settings: Settings) {
        let region_changed = settings.profile.region != self.settings.profile.region;
        self.settings = settings;
        let Some(session) = self.session.as_mut() else { return };
        session.profile.taskbar = self.settings.profile.taskbar.clone();
        if region_changed {
            self.refit();
        }
        self.tick();
        self.emit_status();
    }

    /// Moves the tracked window to the region from the current settings (region edited while
    /// fitted, or adopted after a crash). Resets verification.
    fn refit(&mut self) {
        let monitors = self.wm.monitors();
        let resolved = match region::resolve_on(&monitors, &self.settings.profile.region) {
            Ok(resolved) => resolved,
            Err(e) => {
                log::warn!("refit: {e}");
                self.sink.notice(&Notice::from_error(e));
                return;
            }
        };
        let Some(session) = self.session.as_mut() else { return };
        session.profile.region = self.settings.profile.region.clone();
        session.monitor = resolved.monitor.device.clone();
        if session.tracked.target == resolved.rect {
            return;
        }
        log::info!("refit: {} -> {}", session.tracked.target, resolved.rect);
        session.tracked.target = resolved.rect;
        session.verify = Verify::start(Instant::now());
        if matches!(session.degraded, Some(AppError::ResizeRefused { .. })) {
            session.degraded = None;
        }
        let (tracked, hidden) = (session.tracked.clone(), session.taskbar_hidden);
        if let Err(e) = self.write_marker(&tracked, hidden) {
            log::error!("refit: {e}");
        }
        if let Err(e) = self.wm.set_pos(tracked.hwnd, tracked.target, ZOrder::Keep, true) {
            log::warn!("refit: {e}");
        }
    }

    /// If the emergency path ran behind our back, forget the session without touching
    /// anything (the emergency path already restored it) and clear the flag.
    fn stand_down_if_emergency(&mut self) -> bool {
        if !self.emergency.swap(false, Ordering::SeqCst) {
            return false;
        }
        log::warn!("engine: emergency restore ran; standing down");
        if let Some(session) = self.session.take() {
            self.wm.close_process(&session.process);
        }
        self.hooks.untrack();
        self.emit_status();
        true
    }

    // -----------------------------------------------------------------------------------------
    // Reporting and persistence helpers
    // -----------------------------------------------------------------------------------------

    fn write_marker(&self, tracked: &TrackedWindow, taskbar_hidden: bool) -> Result<(), AppError> {
        self.marker.write(&RestoreMarker {
            schema_version: MARKER_SCHEMA_VERSION,
            pid: self.wm.own_pid(),
            written_at: unix_now(),
            window: Some(tracked.clone()),
            taskbar: TaskbarMarker { taskbar_hidden },
        })
    }

    pub fn status(&self) -> FitStatus {
        let Some(s) = &self.session else { return FitStatus::default() };
        let saved = &s.tracked.saved;
        FitStatus {
            state: FitState::Fitted,
            window: Some(WindowSummary {
                hwnd: s.tracked.hwnd,
                pid: s.tracked.pid,
                exe: s.tracked.exe.clone(),
                title: s.tracked.title.clone(),
            }),
            target: Some(s.tracked.target),
            monitor: Some(s.monitor.clone()),
            restore: Some(RestoreSummary {
                rect: if saved.rect_provisional { saved.placement.normal } else { saved.rect },
                show: saved.placement.show_state(),
                had_borders: saved.style & WS_CAPTION != 0,
            }),
            taskbar_hidden: s.taskbar_hidden,
            target_visible: s.target_visible,
            verify_attempts: s.verify.attempts,
            degraded: s.degraded.clone(),
        }
    }

    fn emit_status(&self) {
        self.sink.status(&self.status());
    }
}

impl Drop for Engine {
    /// Last line of defence (thread exit, panic unwind): never leave a window or taskbar behind.
    fn drop(&mut self) {
        if self.session.is_some() {
            log::warn!("engine: dropped while fitted; restoring");
            self.restore(RestoreReason::Shutdown);
        }
    }
}

/// Style words with the default border mask cleared.
fn borderless_styles(style: u32, ex_style: u32) -> (u32, u32) {
    (style & !DEFAULT_STYLE_MASK, ex_style & !DEFAULT_EX_STYLE_MASK)
}

/// A DPI-unaware window on a scaled monitor gets bitmap-stretched: warn, don't fix.
fn dpi_warning(info: &WindowInfo, monitor: &crate::model::MonitorInfo) -> Option<AppError> {
    let unaware = info.dpi_awareness == crate::model::DpiAwareness::Unaware;
    (unaware && monitor.dpi != 96).then_some(AppError::DpiMismatch {
        awareness: info.dpi_awareness,
        monitor_dpi: monitor.dpi,
    })
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests;

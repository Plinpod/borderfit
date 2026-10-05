//! Crash safety: the restore marker, the restore sequence shared by the engine and the
//! emergency path, and what to do at startup when a previous run left a marker behind.

use crate::model::{
    AppError, Placement, RestoreMarker, ShowState, TrackedWindow, MARKER_SCHEMA_VERSION,
    SW_SHOWMINNOACTIVE,
};
use crate::settings::write_json_atomic;
use crate::taskbar;
use crate::win32::{ShowCmd, WindowManager, ZOrder, WS_EX_TOPMOST};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set while `emergency_restore_from_marker` runs (or has run) behind a hung engine. The engine
/// clears it when it notices and drops to Idle without touching any window.
pub static EMERGENCY: AtomicBool = AtomicBool::new(false);

/// Where the marker lives. A trait so tests can log writes into the fake's call log.
pub trait MarkerStore: Send + Sync {
    /// Writes and fsyncs the marker.
    fn write(&self, marker: &RestoreMarker) -> Result<(), AppError>;
    fn read(&self) -> Option<RestoreMarker>;
    fn delete(&self);
    fn present(&self) -> bool;
}

/// `restore-state.json` in the app's local data folder.
pub struct FileMarkerStore {
    path: PathBuf,
}

impl FileMarkerStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl MarkerStore for FileMarkerStore {
    fn write(&self, marker: &RestoreMarker) -> Result<(), AppError> {
        write_json_atomic(&self.path, marker).map_err(|e| {
            log::error!("marker: write {} failed: {e}", self.path.display());
            AppError::system(format!("Could not write {}: {e}", self.path.display()))
        })
    }

    fn read(&self) -> Option<RestoreMarker> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return None,
            Err(e) => {
                log::error!("marker: read failed: {e}");
                return Some(RestoreMarker::default());
            }
        };
        // A corrupt marker still means "something may be hidden": restore the taskbar.
        Some(serde_json::from_str(&text).unwrap_or_else(|e| {
            log::error!("marker: unparseable ({e}); restoring the taskbar only");
            RestoreMarker::default()
        }))
    }

    fn delete(&self) {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => log::error!("marker: delete failed: {e}"),
        }
    }

    fn present(&self) -> bool {
        self.path.exists()
    }
}

/// True when the tracked window still exists and still belongs to the same process instance.
pub fn window_still_ours(wm: &dyn WindowManager, window: &TrackedWindow) -> bool {
    wm.process_alive(window.pid, window.process_created)
        && wm.window_pid(window.hwnd) == Some(window.pid)
}

/// The restore sequence. Idempotent and total: every step is logged and the next one runs
/// regardless. Window steps are skipped when the window is gone. The taskbar comes back before
/// the window geometry, so a failure half-way leaves the less harmful state.
/// Returns the tray handles that could not be shown again.
pub fn restore_sequence(
    wm: &dyn WindowManager,
    window: Option<&TrackedWindow>,
    window_alive: bool,
) -> Vec<isize> {
    let window = window.filter(|_| window_alive);
    if let Some(w) = window {
        restore_topmost(wm, w);
        wm.dwm_square(w.hwnd, false);
    }
    let failed = taskbar::restore_all(wm);
    if let Some(w) = window {
        restore_styles(wm, w);
        restore_geometry(wm, w);
        log::info!("restore: \"{}\" back to {}", w.title, w.saved.rect);
    }
    failed
}

/// Puts the topmost bit back through z-order if it differs (never through GWL_EXSTYLE).
fn restore_topmost(wm: &dyn WindowManager, w: &TrackedWindow) {
    let Ok((_, ex_style)) = wm.styles(w.hwnd) else { return };
    let topmost_now = ex_style & WS_EX_TOPMOST != 0;
    if topmost_now == w.saved.topmost {
        return;
    }
    let z = if w.saved.topmost { ZOrder::TopMost } else { ZOrder::NoTopMost };
    if let Ok(rect) = wm.rect(w.hwnd) {
        log_err("restore z-order", wm.set_pos(w.hwnd, rect, z, true));
    }
}

/// Writes the saved style words verbatim (topmost masked out: z-order owns that bit).
fn restore_styles(wm: &dyn WindowManager, w: &TrackedWindow) {
    let ex_style = w.saved.ex_style & !WS_EX_TOPMOST;
    log_err("restore styles", wm.set_styles(w.hwnd, w.saved.style, ex_style).map(|_| ()));
}

/// Geometry by the saved show state. Screen coordinates (`saved.rect`) only ever go to
/// `set_pos`; workspace coordinates (`saved.placement`) only ever go to `set_placement`.
fn restore_geometry(wm: &dyn WindowManager, w: &TrackedWindow) {
    let saved = &w.saved;
    match saved.placement.show_state() {
        ShowState::Normal if !saved.rect_provisional => {
            log_err("restore rect", wm.set_pos(w.hwnd, saved.rect, ZOrder::Keep, true));
        }
        ShowState::Normal | ShowState::Maximized => {
            log_err("restore placement", wm.set_placement(w.hwnd, &saved.placement));
            refresh_frame(wm, w.hwnd);
        }
        ShowState::Minimized => {
            let placement = Placement { show_cmd: SW_SHOWMINNOACTIVE, ..saved.placement };
            log_err("restore placement", wm.set_placement(w.hwnd, &placement));
            log_err("restore minimized", wm.show(w.hwnd, ShowCmd::ShowMinNoActive));
        }
    }
}

/// A SetWindowPos with the current rect: forces a frame recalculation after the style write.
fn refresh_frame(wm: &dyn WindowManager, hwnd: isize) {
    if let Ok(rect) = wm.rect(hwnd) {
        log_err("restore frame", wm.set_pos(hwnd, rect, ZOrder::Keep, true));
    }
}

fn log_err(step: &str, result: Result<(), AppError>) {
    if let Err(e) = result {
        log::warn!("{step}: {e}");
    }
}

/// Runs the restore sequence from the marker file alone, for when the engine is dead or hung.
/// Re-entrant calls return immediately. Uses a fresh window manager and takes no locks.
pub fn emergency_restore_from_marker(
    wm: &dyn WindowManager,
    store: &dyn MarkerStore,
    emergency: &AtomicBool,
) {
    if emergency.swap(true, Ordering::SeqCst) {
        return;
    }
    log::warn!("emergency restore from marker");
    let window = store.read().and_then(|m| m.window);
    let alive = window.as_ref().is_some_and(|w| window_still_ours(wm, w));
    let failed = restore_sequence(wm, window.as_ref(), alive);
    if failed.is_empty() {
        store.delete();
    }
}

/// A window a crashed run left fitted, still open: BorderFit keeps tracking it.
#[derive(Clone, Debug, PartialEq)]
pub struct Adopted {
    pub tracked: TrackedWindow,
    pub taskbar_hidden: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StartupRecovery {
    /// Nothing to do.
    Clean,
    /// A marker was left behind but its window is gone: the taskbar was shown again.
    TaskbarRestored { failed: Vec<isize> },
    /// The fitted window is still open: adopt it instead of yanking it back under the player.
    Adopted(Adopted),
}

/// Called once at startup, before the engine runs. Only valid when no other BorderFit runs.
pub fn run_at_startup(wm: &dyn WindowManager, store: &dyn MarkerStore) -> StartupRecovery {
    let Some(marker) = store.read() else {
        // No marker, but "start BorderFit once" is the documented escape hatch for a taskbar
        // left hidden by anything: show explorer's trays if they are hidden.
        if wm.tray_windows().iter().any(|t| !t.visible) {
            log::warn!("startup: explorer taskbar hidden without a marker; showing it");
            let failed = taskbar::restore_all(wm);
            return StartupRecovery::TaskbarRestored { failed };
        }
        return StartupRecovery::Clean;
    };
    log::warn!(
        "startup: marker from pid {} (schema {}) found; window {:?}",
        marker.pid,
        marker.schema_version,
        marker.window.as_ref().map(|w| (&w.title, w.hwnd))
    );
    if marker.schema_version <= MARKER_SCHEMA_VERSION {
        if let Some(window) = marker.window.filter(|w| window_still_ours(wm, w)) {
            return StartupRecovery::Adopted(Adopted {
                tracked: window,
                taskbar_hidden: marker.taskbar.taskbar_hidden,
            });
        }
    }
    let failed = taskbar::restore_all(wm);
    if failed.is_empty() {
        store.delete();
    }
    StartupRecovery::TaskbarRestored { failed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Rect, TaskbarMarker};
    use crate::test_support::TempDir;
    use crate::win32::fake::{Call, FakeMarkerStore, FakeState, FakeWindow, FakeWindowManager};
    use crate::win32::DEFAULT_STYLE_MASK;

    /// A desktop where "Game" is fitted borderless and the primary taskbar is hidden.
    fn crashed_desktop() -> (FakeWindowManager, FakeMarkerStore, TrackedWindow) {
        let mut state = FakeState::empty();
        state.add_tray(10, "Shell_TrayWnd", "explorer.exe");
        let window = FakeWindow::normal(50, 77, "Game");
        state.add_window(window.clone());
        let created = state.processes[&77].created;
        let t = window.tracked(created, Rect::new(1280, 0, 2560, 1440));
        state.window_mut(50).style &= !DEFAULT_STYLE_MASK;
        state.window_mut(50).rect = t.target;
        state.trays[0].visible = false;
        let wm = FakeWindowManager::new(state);
        let store = FakeMarkerStore::new(&wm);
        store
            .write(&RestoreMarker {
                schema_version: 1,
                pid: 999,
                written_at: 0,
                window: Some(t.clone()),
                taskbar: TaskbarMarker { taskbar_hidden: true },
            })
            .unwrap();
        wm.lock().calls.clear();
        (wm, store, t)
    }

    #[test]
    fn startup_adopts_a_live_window() {
        let (wm, store, t) = crashed_desktop();
        let outcome = run_at_startup(&wm, &store);
        assert_eq!(outcome, StartupRecovery::Adopted(Adopted { tracked: t, taskbar_hidden: true }));
        // Adoption leaves the game alone; the engine takes it from here.
        assert!(wm.lock().calls.is_empty());
        assert!(store.present());
    }

    #[test]
    fn startup_restores_the_taskbar_when_the_window_is_gone() {
        let (wm, store, _) = crashed_desktop();
        wm.lock().kill_process(77);
        let outcome = run_at_startup(&wm, &store);
        assert_eq!(outcome, StartupRecovery::TaskbarRestored { failed: vec![] });
        assert!(wm.lock().tray(10).visible);
        assert!(!store.present());
    }

    #[test]
    fn startup_treats_a_recycled_pid_as_gone() {
        let (wm, store, _) = crashed_desktop();
        wm.lock().processes.get_mut(&77).unwrap().created += 1;
        assert!(matches!(run_at_startup(&wm, &store), StartupRecovery::TaskbarRestored { .. }));
    }

    #[test]
    fn startup_without_marker_still_shows_a_hidden_taskbar() {
        let (wm, store, _) = crashed_desktop();
        store.delete();
        assert!(matches!(run_at_startup(&wm, &store), StartupRecovery::TaskbarRestored { .. }));
        assert!(wm.lock().tray(10).visible);
        wm.lock().calls.clear();
        assert_eq!(run_at_startup(&wm, &store), StartupRecovery::Clean);
        assert!(wm.lock().calls.is_empty());
    }

    #[test]
    fn emergency_restores_window_and_taskbar_once() {
        let (wm, store, t) = crashed_desktop();
        let flag = AtomicBool::new(false);
        emergency_restore_from_marker(&wm, &store, &flag);
        {
            let s = wm.lock();
            assert!(s.tray(10).visible);
            assert_eq!(s.window(50).style, t.saved.style);
            assert_eq!(s.window(50).rect, t.saved.rect);
        }
        assert!(!store.present());
        assert!(flag.load(Ordering::SeqCst));
        // Re-entrant callers return without doing anything.
        wm.lock().calls.clear();
        emergency_restore_from_marker(&wm, &store, &flag);
        assert!(wm.lock().calls.is_empty());
    }

    #[test]
    fn emergency_skips_a_dead_window_but_shows_the_taskbar() {
        let (wm, store, _) = crashed_desktop();
        wm.lock().kill_process(77);
        emergency_restore_from_marker(&wm, &store, &AtomicBool::new(false));
        let s = wm.lock();
        assert!(s.tray(10).visible);
        assert!(s.calls.iter().all(|c| c.hwnd() == Some(10) || c.hwnd().is_none()));
    }

    #[test]
    fn unreadable_marker_file_still_restores_the_taskbar() {
        let (wm, _, _) = crashed_desktop();
        let dir = TempDir::new("marker");
        let path = dir.join("restore-state.json");
        fs::write(&path, "{ not json").unwrap();
        let store = FileMarkerStore::new(path.clone());

        let outcome = run_at_startup(&wm, &store);
        assert_eq!(outcome, StartupRecovery::TaskbarRestored { failed: vec![] });
        assert!(wm.lock().tray(10).visible);
        assert!(!path.exists());
    }

    #[test]
    fn marker_from_a_newer_version_is_not_adopted() {
        let (wm, store, t) = crashed_desktop();
        store
            .write(&RestoreMarker {
                schema_version: MARKER_SCHEMA_VERSION + 1,
                window: Some(t),
                taskbar: TaskbarMarker { taskbar_hidden: true },
                ..RestoreMarker::default()
            })
            .unwrap();
        assert!(matches!(run_at_startup(&wm, &store), StartupRecovery::TaskbarRestored { .. }));
        assert!(wm.lock().tray(10).visible);
    }

    #[test]
    fn taskbar_comes_back_before_the_window_geometry() {
        let (wm, _store, t) = crashed_desktop();
        restore_sequence(&wm, Some(&t), true);
        let calls = wm.lock().calls.clone();
        let position = |wanted: &dyn Fn(&Call) -> bool| {
            calls.iter().position(wanted).unwrap_or_else(|| panic!("missing call in {calls:?}"))
        };
        let tray = position(&|c| *c == Call::Show { hwnd: 10, cmd: ShowCmd::ShowNaAsync });
        let styles = position(&|c| matches!(c, Call::SetStyles { .. }));
        let geometry = position(&|c| matches!(c, Call::SetPos { .. }));
        assert!(tray < styles && styles < geometry, "{calls:?}");
    }
}

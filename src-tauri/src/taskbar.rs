//! Hiding and showing explorer's taskbars.
//!
//! Nothing is cached: trays are rediscovered (explorer-owned, class-verified) on every call, and
//! the desired state is derived by the caller each tick, so an Explorer restart or a stray
//! re-show is corrected within one tick. `SPI_SETWORKAREA` is never touched.

use crate::win32::{ShowCmd, TrayWindow, WindowManager};

/// Hides or shows trays so that exactly the desired set is hidden.
///
/// `hide` is the derived desire (fitted, target visible, hide_taskbar on). Secondary trays are
/// hidden only with `include_secondary`; otherwise they are shown if they were hidden earlier.
/// Returns true when any explorer tray is hidden afterwards (as far as the async calls go).
pub fn reconcile(wm: &dyn WindowManager, hide: bool, include_secondary: bool) -> bool {
    let trays = wm.tray_windows();
    let mut any_hidden = false;
    for tray in &trays {
        let want_hidden = hide && (tray.is_primary() || include_secondary);
        if want_hidden && tray.visible {
            apply(wm, tray, ShowCmd::HideAsync);
        } else if !want_hidden && !tray.visible {
            apply(wm, tray, ShowCmd::ShowNaAsync);
        }
        any_hidden |= want_hidden;
    }
    any_hidden
}

/// Shows every hidden explorer tray, re-checks, retries once, and returns the handles that are
/// still hidden (empty on success).
pub fn restore_all(wm: &dyn WindowManager) -> Vec<isize> {
    let mut hidden = hidden_trays(wm);
    for attempt in 0..2 {
        if hidden.is_empty() {
            break;
        }
        for tray in &hidden {
            apply(wm, tray, ShowCmd::ShowNaAsync);
        }
        wm.settle();
        hidden = hidden_trays(wm);
        if attempt == 0 && !hidden.is_empty() {
            log::warn!("taskbar: {} tray(s) still hidden, retrying", hidden.len());
        }
    }
    if !hidden.is_empty() {
        log::error!("taskbar: could not show {:?}", handles(&hidden));
    }
    handles(&hidden)
}

fn hidden_trays(wm: &dyn WindowManager) -> Vec<TrayWindow> {
    wm.tray_windows().into_iter().filter(|t| !t.visible).collect()
}

fn handles(trays: &[TrayWindow]) -> Vec<isize> {
    trays.iter().map(|t| t.hwnd).collect()
}

fn apply(wm: &dyn WindowManager, tray: &TrayWindow, cmd: ShowCmd) {
    log::info!("taskbar: {cmd:?} {} {:#x}", tray.class, tray.hwnd);
    if let Err(e) = wm.show(tray.hwnd, cmd) {
        log::warn!("taskbar: {cmd:?} {:#x} failed: {e}", tray.hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win32::fake::{FakeState, FakeWindowManager};

    fn desktop() -> FakeWindowManager {
        let mut state = FakeState::empty();
        state.add_tray(10, "Shell_TrayWnd", "explorer.exe");
        state.add_tray(11, "Shell_SecondaryTrayWnd", "Explorer.EXE");
        FakeWindowManager::new(state)
    }

    #[test]
    fn hides_only_the_primary_explorer_tray_by_default() {
        let wm = desktop();
        assert!(reconcile(&wm, true, false));
        let s = wm.lock();
        assert!(!s.tray(10).visible);
        assert!(s.tray(11).visible);
    }

    #[test]
    fn secondary_trays_follow_the_setting_both_ways() {
        let wm = desktop();
        reconcile(&wm, true, true);
        assert!(!wm.lock().tray(11).visible);
        reconcile(&wm, true, false);
        assert!(wm.lock().tray(11).visible);
        assert!(!wm.lock().tray(10).visible);
    }

    #[test]
    fn not_desired_shows_everything_again() {
        let wm = desktop();
        reconcile(&wm, true, true);
        assert!(!reconcile(&wm, false, true));
        let s = wm.lock();
        assert!(s.tray(10).visible && s.tray(11).visible);
    }

    #[test]
    fn restore_reports_trays_that_stay_hidden() {
        let wm = desktop();
        reconcile(&wm, true, true);
        wm.lock().trays[1].stuck_hidden = true;
        assert_eq!(restore_all(&wm), vec![11]);
        assert!(wm.lock().tray(10).visible);
    }
}

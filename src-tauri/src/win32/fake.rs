//! In-memory `WindowManager` for tests: windows, trays, processes and monitors live in a shared
//! `FakeState`, and every mutation is appended to a call log so tests can assert ordering.
//! Also serves as the stand-in platform on non-Windows builds (with an empty desktop).

use super::{
    ProcessHandle, ShowCmd, TrayWindow, WindowInfo, WindowManager, ZOrder, ERROR_ACCESS_DENIED,
    WS_EX_TOPMOST,
};
use crate::model::{
    AppError, DpiAwareness, MonitorInfo, Placement, Rect, RestoreMarker, SavedState, TrackedWindow,
    SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE,
};
use crate::recovery::MarkerStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

const WS_MAXIMIZE: u32 = 0x0100_0000;
const WS_MINIMIZE: u32 = 0x2000_0000;
const ERROR_INVALID_PARAMETER: u32 = 87;

/// One logged mutation (reads are not logged).
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    MarkerWrite { rect_provisional: bool, taskbar_hidden: bool },
    MarkerDelete,
    SetStyles { hwnd: isize, style: u32, ex_style: u32 },
    SetPlacement { hwnd: isize, show_cmd: u32 },
    SetPos { hwnd: isize, rect: Rect, z: ZOrder },
    Show { hwnd: isize, cmd: ShowCmd },
    SetForeground { hwnd: isize },
    DwmSquare { hwnd: isize, on: bool },
}

impl Call {
    /// The window (or tray) a mutation touched, if any.
    pub fn hwnd(&self) -> Option<isize> {
        match *self {
            Call::SetStyles { hwnd, .. }
            | Call::SetPlacement { hwnd, .. }
            | Call::SetPos { hwnd, .. }
            | Call::Show { hwnd, .. }
            | Call::SetForeground { hwnd }
            | Call::DwmSquare { hwnd, .. } => Some(hwnd),
            Call::MarkerWrite { .. } | Call::MarkerDelete => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FakeWindow {
    pub hwnd: isize,
    pub pid: u32,
    pub tid: u32,
    pub exe: String,
    pub title: String,
    pub class: String,
    pub style: u32,
    pub ex_style: u32,
    pub rect: Rect,
    pub placement: Placement,
    pub visible: bool,
    pub cloaked: bool,
    pub iconic: bool,
    pub zoomed: bool,
    pub arranged: bool,
    pub dpi: u32,
    pub dpi_awareness: DpiAwareness,
    /// Runs at high integrity: every write fails with ERROR_ACCESS_DENIED.
    pub elevated: bool,
    /// The game enforces this size: `set_pos` lands here instead of the requested size.
    pub enforced_size: Option<(i32, i32)>,
    /// `set_styles` changes nothing.
    pub refuse_styles: bool,
    /// `set_pos` to exactly this rect fails (the fit's own move, not the restore's).
    pub fail_set_pos_to: Option<Rect>,
    /// `show(Restore)` fails, so a minimized or maximized window can't be normalized.
    pub fail_restore: bool,
}

impl FakeWindow {
    /// A normal, bordered 1920x1080 window at (100, 140) whose workspace rect is (100, 100).
    pub fn normal(hwnd: isize, pid: u32, title: &str) -> Self {
        Self {
            hwnd,
            pid,
            tid: pid + 1,
            exe: format!("{}.exe", title.to_lowercase().replace(' ', "")),
            title: title.to_string(),
            class: "GameWindow".to_string(),
            style: 0x14CF_0000, // WS_VISIBLE | WS_OVERLAPPEDWINDOW | WS_CLIPSIBLINGS
            ex_style: 0x0000_0100,
            rect: Rect::new(100, 140, 1920, 1080),
            placement: Placement {
                show_cmd: 1,
                normal: Rect::new(100, 100, 1920, 1080),
                ..Placement::default()
            },
            visible: true,
            cloaked: false,
            iconic: false,
            zoomed: false,
            arranged: false,
            dpi: 96,
            dpi_awareness: DpiAwareness::PerMonitorV2,
            elevated: false,
            enforced_size: None,
            refuse_styles: false,
            fail_set_pos_to: None,
            fail_restore: false,
        }
    }

    /// What fitting this window, as it is now, would track: its words, placement and rect saved,
    /// `target` as the fitted rect. For tests that start from a fitted (or crashed) state.
    pub fn tracked(&self, process_created: u64, target: Rect) -> TrackedWindow {
        TrackedWindow {
            hwnd: self.hwnd,
            pid: self.pid,
            tid: self.tid,
            process_created,
            exe: self.exe.clone(),
            title: self.title.clone(),
            class: self.class.clone(),
            saved: SavedState {
                style: self.style,
                ex_style: self.ex_style,
                placement: self.placement,
                rect: self.rect,
                ..SavedState::default()
            },
            target,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FakeTray {
    pub hwnd: isize,
    pub class: String,
    pub exe: String,
    pub visible: bool,
    /// Ignores show requests (simulates a tray that won't come back).
    pub stuck_hidden: bool,
}

#[derive(Clone, Debug)]
pub struct FakeProcess {
    pub created: u64,
    pub alive: bool,
}

#[derive(Debug)]
pub struct FakeState {
    pub own_pid: u32,
    pub foreground: Option<isize>,
    /// Top-level windows in z-order, topmost first.
    pub z_order: Vec<isize>,
    pub windows: HashMap<isize, FakeWindow>,
    pub trays: Vec<FakeTray>,
    pub processes: HashMap<u32, FakeProcess>,
    pub monitors: Vec<MonitorInfo>,
    /// Origin of the primary work area; workspace coordinates are relative to it.
    pub work_origin: (i32, i32),
    pub calls: Vec<Call>,
    /// Marker contents as last written by `FakeMarkerStore`.
    pub marker: Option<RestoreMarker>,
    /// Makes `FakeMarkerStore::write` fail (disk full, permissions).
    pub fail_marker_writes: bool,
    pub integrity_levels: HashMap<u32, u32>,
}

impl FakeState {
    pub fn empty() -> Self {
        Self {
            own_pid: 1000,
            foreground: None,
            z_order: Vec::new(),
            windows: HashMap::new(),
            trays: Vec::new(),
            processes: HashMap::new(),
            monitors: Vec::new(),
            work_origin: (0, 0),
            calls: Vec::new(),
            marker: None,
            fail_marker_writes: false,
            integrity_levels: HashMap::new(),
        }
    }

    /// Adds a window (and its process) on top of the z-order.
    pub fn add_window(&mut self, window: FakeWindow) {
        self.processes
            .entry(window.pid)
            .or_insert(FakeProcess { created: u64::from(window.pid) * 10, alive: true });
        self.z_order.insert(0, window.hwnd);
        self.windows.insert(window.hwnd, window);
    }

    pub fn add_tray(&mut self, hwnd: isize, class: &str, exe: &str) {
        self.trays.push(FakeTray {
            hwnd,
            class: class.to_string(),
            exe: exe.to_string(),
            visible: true,
            stuck_hidden: false,
        });
    }

    pub fn kill_process(&mut self, pid: u32) {
        if let Some(p) = self.processes.get_mut(&pid) {
            p.alive = false;
        }
        self.windows.retain(|_, w| w.pid != pid);
        self.z_order.retain(|h| self.windows.contains_key(h));
    }

    pub fn tray(&self, hwnd: isize) -> &FakeTray {
        self.trays.iter().find(|t| t.hwnd == hwnd).expect("no such tray")
    }

    pub fn window(&self, hwnd: isize) -> &FakeWindow {
        &self.windows[&hwnd]
    }

    pub fn window_mut(&mut self, hwnd: isize) -> &mut FakeWindow {
        self.windows.get_mut(&hwnd).expect("no such window")
    }

    /// Converts a workspace rect (as in WINDOWPLACEMENT) to screen coordinates.
    fn to_screen(&self, r: Rect) -> Rect {
        Rect::new(r.x + self.work_origin.0, r.y + self.work_origin.1, r.w, r.h)
    }
}

/// A cloneable handle to shared fake state; clones see the same desktop.
#[derive(Clone)]
pub struct FakeWindowManager {
    pub state: Arc<Mutex<FakeState>>,
}

impl Default for FakeWindowManager {
    fn default() -> Self {
        Self::new(FakeState::empty())
    }
}

impl FakeWindowManager {
    pub fn new(state: FakeState) -> Self {
        Self { state: Arc::new(Mutex::new(state)) }
    }

    pub fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().expect("fake state poisoned")
    }

    fn with_window<T>(
        &self,
        hwnd: isize,
        f: impl FnOnce(&mut FakeWindow) -> T,
    ) -> Result<T, AppError> {
        let mut state = self.lock();
        let window = state.windows.get_mut(&hwnd).ok_or(AppError::WindowGone)?;
        Ok(f(window))
    }

    fn deny_if_elevated(&self, hwnd: isize, api: &str) -> Result<(), AppError> {
        let elevated = self.with_window(hwnd, |w| w.elevated)?;
        if elevated {
            return Err(AppError::win32(api, ERROR_ACCESS_DENIED));
        }
        Ok(())
    }

    fn log(&self, call: Call) {
        self.lock().calls.push(call);
    }
}

impl WindowManager for FakeWindowManager {
    fn own_pid(&self) -> u32 {
        self.lock().own_pid
    }

    fn foreground(&self) -> Option<isize> {
        self.lock().foreground
    }

    fn top_level_windows(&self) -> Vec<isize> {
        self.lock().z_order.clone()
    }

    fn info(&self, hwnd: isize) -> Result<WindowInfo, AppError> {
        let state = self.lock();
        let w = state.windows.get(&hwnd).ok_or(AppError::WindowGone)?;
        let monitor = state
            .monitors
            .iter()
            .find(|m| m.rect.contains_point(w.rect.center().0, w.rect.center().1))
            .or_else(|| state.monitors.first())
            .map(|m| m.device.clone())
            .unwrap_or_default();
        Ok(WindowInfo {
            hwnd,
            root: hwnd,
            pid: w.pid,
            tid: w.tid,
            exe: w.exe.clone(),
            title: w.title.clone(),
            class: w.class.clone(),
            visible: w.visible,
            cloaked: w.cloaked,
            iconic: w.iconic,
            zoomed: w.zoomed,
            arranged: w.arranged,
            ex_style: w.ex_style,
            dpi: w.dpi,
            dpi_awareness: w.dpi_awareness,
            monitor,
        })
    }

    fn integrity_level(&self, pid: u32) -> Option<u32> {
        self.lock().integrity_levels.get(&pid).copied()
    }

    fn probe_access(&self, hwnd: isize) -> Result<(), AppError> {
        self.deny_if_elevated(hwnd, "SetWindowPos")
    }

    fn styles(&self, hwnd: isize) -> Result<(u32, u32), AppError> {
        self.with_window(hwnd, |w| (w.style, w.ex_style))
    }

    fn set_styles(&self, hwnd: isize, style: u32, ex_style: u32) -> Result<(u32, u32), AppError> {
        self.deny_if_elevated(hwnd, "SetWindowLongPtrW")?;
        self.log(Call::SetStyles { hwnd, style, ex_style });
        self.with_window(hwnd, |w| {
            if !w.refuse_styles {
                w.style = style;
                // Topmost is z-order state; writing GWL_EXSTYLE cannot change it.
                w.ex_style = (ex_style & !WS_EX_TOPMOST) | (w.ex_style & WS_EX_TOPMOST);
            }
            (w.style, w.ex_style)
        })
    }

    fn placement(&self, hwnd: isize) -> Result<Placement, AppError> {
        self.with_window(hwnd, |w| w.placement)
    }

    fn set_placement(&self, hwnd: isize, placement: &Placement) -> Result<(), AppError> {
        self.deny_if_elevated(hwnd, "SetWindowPlacement")?;
        self.log(Call::SetPlacement { hwnd, show_cmd: placement.show_cmd });
        let mut state = self.lock();
        let screen = state.to_screen(placement.normal);
        let w = state.windows.get_mut(&hwnd).ok_or(AppError::WindowGone)?;
        w.placement = *placement;
        w.zoomed = placement.show_cmd == SW_SHOWMAXIMIZED;
        w.iconic = matches!(placement.show_cmd, SW_SHOWMINIMIZED | SW_SHOWMINNOACTIVE);
        w.arranged = false;
        w.style &= !(WS_MAXIMIZE | WS_MINIMIZE);
        if w.zoomed {
            w.style |= WS_MAXIMIZE;
        }
        if w.iconic {
            w.style |= WS_MINIMIZE;
        }
        if !w.zoomed && !w.iconic {
            w.rect = screen;
        }
        Ok(())
    }

    fn rect(&self, hwnd: isize) -> Result<Rect, AppError> {
        self.with_window(
            hwnd,
            |w| if w.iconic { Rect::new(-32000, -32000, 160, 28) } else { w.rect },
        )
    }

    fn set_pos(
        &self,
        hwnd: isize,
        rect: Rect,
        z: ZOrder,
        _async_pos: bool,
    ) -> Result<(), AppError> {
        self.deny_if_elevated(hwnd, "SetWindowPos")?;
        if self.with_window(hwnd, |w| w.fail_set_pos_to == Some(rect))? {
            return Err(AppError::win32("SetWindowPos", ERROR_INVALID_PARAMETER));
        }
        self.log(Call::SetPos { hwnd, rect, z });
        self.with_window(hwnd, |w| {
            w.rect = match w.enforced_size {
                Some((ew, eh)) => Rect::new(rect.x, rect.y, ew, eh),
                None => rect,
            };
            match z {
                ZOrder::TopMost => w.ex_style |= WS_EX_TOPMOST,
                ZOrder::NoTopMost => w.ex_style &= !WS_EX_TOPMOST,
                ZOrder::Keep => {}
            }
        })
    }

    fn show(&self, hwnd: isize, cmd: ShowCmd) -> Result<(), AppError> {
        self.log(Call::Show { hwnd, cmd });
        let mut state = self.lock();
        if let Some(tray) = state.trays.iter_mut().find(|t| t.hwnd == hwnd) {
            match cmd {
                ShowCmd::HideAsync => tray.visible = false,
                ShowCmd::ShowNaAsync if !tray.stuck_hidden => tray.visible = true,
                _ => {}
            }
            return Ok(());
        }
        let normal = state.windows.get(&hwnd).ok_or(AppError::WindowGone)?.placement.normal;
        let screen = state.to_screen(normal);
        let w = state.windows.get_mut(&hwnd).ok_or(AppError::WindowGone)?;
        if w.elevated {
            return Err(AppError::win32("ShowWindow", ERROR_ACCESS_DENIED));
        }
        if w.fail_restore && cmd == ShowCmd::Restore {
            return Err(AppError::win32("ShowWindow", ERROR_INVALID_PARAMETER));
        }
        match cmd {
            ShowCmd::Restore => {
                w.iconic = false;
                w.zoomed = false;
                w.arranged = false;
                w.style &= !(WS_MAXIMIZE | WS_MINIMIZE);
                w.rect = screen;
                w.placement.show_cmd = 1;
            }
            ShowCmd::ShowMinNoActive => {
                w.iconic = true;
                w.style |= WS_MINIMIZE;
            }
            ShowCmd::HideAsync => w.visible = false,
            ShowCmd::ShowNaAsync => w.visible = true,
        }
        Ok(())
    }

    fn set_foreground(&self, hwnd: isize) -> bool {
        self.log(Call::SetForeground { hwnd });
        let mut state = self.lock();
        let exists = state.windows.contains_key(&hwnd);
        if exists {
            state.foreground = Some(hwnd);
        }
        exists
    }

    fn dwm_square(&self, hwnd: isize, on: bool) {
        self.log(Call::DwmSquare { hwnd, on });
    }

    fn is_window(&self, hwnd: isize) -> bool {
        self.lock().windows.contains_key(&hwnd)
    }

    fn left_button_down(&self) -> bool {
        false
    }

    fn is_visible(&self, hwnd: isize) -> bool {
        self.lock().windows.get(&hwnd).is_some_and(|w| w.visible && !w.cloaked)
    }

    fn is_iconic(&self, hwnd: isize) -> bool {
        self.lock().windows.get(&hwnd).is_some_and(|w| w.iconic)
    }

    fn is_zoomed(&self, hwnd: isize) -> bool {
        self.lock().windows.get(&hwnd).is_some_and(|w| w.zoomed)
    }

    fn window_pid(&self, hwnd: isize) -> Option<u32> {
        self.lock().windows.get(&hwnd).map(|w| w.pid)
    }

    fn tray_windows(&self) -> Vec<TrayWindow> {
        self.lock()
            .trays
            .iter()
            .filter(|t| t.exe.eq_ignore_ascii_case("explorer.exe"))
            .map(|t| TrayWindow {
                hwnd: t.hwnd,
                class: t.class.clone(),
                exe: t.exe.clone(),
                visible: t.visible,
            })
            .collect()
    }

    fn open_process(&self, pid: u32) -> Result<ProcessHandle, AppError> {
        let state = self.lock();
        let process = state.processes.get(&pid).filter(|p| p.alive).ok_or(AppError::WindowGone)?;
        Ok(ProcessHandle { raw: pid as isize, pid, created: process.created })
    }

    fn process_exited(&self, handle: &ProcessHandle) -> bool {
        let state = self.lock();
        state.processes.get(&handle.pid).is_none_or(|p| !p.alive || p.created != handle.created)
    }

    fn close_process(&self, _handle: &ProcessHandle) {}

    fn process_alive(&self, pid: u32, created: u64) -> bool {
        let state = self.lock();
        state.processes.get(&pid).is_some_and(|p| p.alive && (created == 0 || p.created == created))
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        self.lock().monitors.clone()
    }

    fn settle(&self) {}
}

/// Marker store that keeps the marker in `FakeState` and logs writes in the same call log as
/// the window calls, so tests can assert "marker before mutation".
pub struct FakeMarkerStore {
    state: Arc<Mutex<FakeState>>,
}

impl FakeMarkerStore {
    pub fn new(wm: &FakeWindowManager) -> Self {
        Self { state: Arc::clone(&wm.state) }
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().expect("fake state poisoned")
    }
}

impl MarkerStore for FakeMarkerStore {
    fn write(&self, marker: &RestoreMarker) -> Result<(), AppError> {
        let mut state = self.lock();
        if state.fail_marker_writes {
            return Err(AppError::system("disk full"));
        }
        state.calls.push(Call::MarkerWrite {
            rect_provisional: marker.window.as_ref().is_some_and(|w| w.saved.rect_provisional),
            taskbar_hidden: marker.taskbar.taskbar_hidden,
        });
        state.marker = Some(marker.clone());
        Ok(())
    }

    fn read(&self) -> Option<RestoreMarker> {
        self.lock().marker.clone()
    }

    fn delete(&self) {
        let mut state = self.lock();
        if state.marker.take().is_some() {
            state.calls.push(Call::MarkerDelete);
        }
    }

    fn present(&self) -> bool {
        self.lock().marker.is_some()
    }
}

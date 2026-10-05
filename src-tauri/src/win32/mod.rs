//! The seam between BorderFit's logic and the Windows API.
//!
//! `WindowManager` is the only way the engine, taskbar and recovery code touch windows. Its
//! signatures use plain data (`isize` handles, `model` types): no `windows`-crate or Tauri
//! types, so the logic runs on Linux against [`fake::FakeWindowManager`] in `cargo test`.

use crate::model::{AppError, DpiAwareness, MonitorInfo, Placement, Rect};

pub mod fake;
#[cfg(windows)]
pub mod windows;

/// Classes that belong to the shell; never fit these.
pub const SHELL_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "Windows.UI.Core.CoreWindow",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "XamlExplorerHostIslandWindow",
    "MultitaskingViewFrame",
    "ForegroundStaging",
    "TaskListThumbnailWnd",
    "Shell_InputSwitchTopLevelWindow",
];

/// UWP apps live inside this host frame; resizing it fights the app model.
pub const UWP_HOST_CLASS: &str = "ApplicationFrameWindow";

pub const PRIMARY_TRAY_CLASS: &str = "Shell_TrayWnd";
pub const SECONDARY_TRAY_CLASS: &str = "Shell_SecondaryTrayWnd";

pub fn is_shell_class(class: &str) -> bool {
    SHELL_CLASSES.contains(&class)
}

// Window style bits the engine reasons about (values from WinUser.h).
pub const WS_CAPTION: u32 = 0x00C0_0000;
pub const WS_THICKFRAME: u32 = 0x0004_0000;
pub const WS_MAXIMIZE: u32 = 0x0100_0000;
pub const WS_SYSMENU: u32 = 0x0008_0000;
pub const WS_MINIMIZEBOX: u32 = 0x0002_0000;
pub const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
pub const WS_EX_DLGMODALFRAME: u32 = 0x0000_0001;
pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
pub const WS_EX_WINDOWEDGE: u32 = 0x0000_0100;
pub const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
pub const WS_EX_STATICEDGE: u32 = 0x0002_0000;
pub const WS_EX_APPWINDOW: u32 = 0x0004_0000;

/// Style bits cleared by the default mask: WS_CAPTION | WS_THICKFRAME (the AHK script's 0xC40000).
pub const DEFAULT_STYLE_MASK: u32 = WS_CAPTION | WS_THICKFRAME;
/// Ex-style bits cleared by the default mask (0x20301). LAYERED/APPWINDOW/TOOLWINDOW stay.
pub const DEFAULT_EX_STYLE_MASK: u32 =
    WS_EX_DLGMODALFRAME | WS_EX_WINDOWEDGE | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE;

// Win32 error codes the engine classifies.
pub const ERROR_ACCESS_DENIED: u32 = 5;
pub const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
pub const ERROR_CANCELLED: u32 = 1223;

// WinEvent ids delivered by the hook thread.
pub const EVENT_OBJECT_DESTROY: u32 = 0x8001;
pub const EVENT_OBJECT_HIDE: u32 = 0x8003;
pub const EVENT_OBJECT_CLOAKED: u32 = 0x8017;
pub const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;

/// A snapshot of one top-level window.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WindowInfo {
    pub hwnd: isize,
    /// `GetAncestor(GA_ROOT)`.
    pub root: isize,
    pub pid: u32,
    pub tid: u32,
    /// Executable file name, e.g. `game.exe` (empty when the process can't be queried).
    pub exe: String,
    pub title: String,
    pub class: String,
    pub visible: bool,
    pub cloaked: bool,
    pub iconic: bool,
    pub zoomed: bool,
    /// Snapped (`IsWindowArranged`).
    pub arranged: bool,
    pub ex_style: u32,
    pub dpi: u32,
    pub dpi_awareness: DpiAwareness,
    /// Device name of the monitor the window is on.
    pub monitor: String,
}

impl WindowInfo {
    /// Tool windows without WS_EX_APPWINDOW are palettes and popups, not app windows.
    pub fn is_tool_window(&self) -> bool {
        self.ex_style & WS_EX_TOOLWINDOW != 0 && self.ex_style & WS_EX_APPWINDOW == 0
    }
}

/// A taskbar window owned by explorer.exe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayWindow {
    pub hwnd: isize,
    pub class: String,
    pub exe: String,
    pub visible: bool,
}

impl TrayWindow {
    pub fn is_primary(&self) -> bool {
        self.class == PRIMARY_TRAY_CLASS
    }
}

/// An open process handle (`SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION`), closed with
/// [`WindowManager::close_process`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessHandle {
    pub raw: isize,
    pub pid: u32,
    /// Process creation time (FILETIME ticks); tells a recycled pid apart.
    pub created: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowCmd {
    /// `ShowWindow(SW_RESTORE)`, synchronous.
    Restore,
    /// `ShowWindow(SW_SHOWMINNOACTIVE)`, synchronous.
    ShowMinNoActive,
    /// `ShowWindowAsync(SW_HIDE)`, for trays.
    HideAsync,
    /// `ShowWindowAsync(SW_SHOWNA)`, for trays (SW_SHOW would steal focus).
    ShowNaAsync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZOrder {
    /// `SWP_NOZORDER`.
    Keep,
    TopMost,
    NoTopMost,
}

pub trait WindowManager: Send {
    fn own_pid(&self) -> u32;
    /// `GetForegroundWindow`.
    fn foreground(&self) -> Option<isize>;
    /// Top-level windows in z-order, topmost first.
    fn top_level_windows(&self) -> Vec<isize>;
    fn info(&self, hwnd: isize) -> Result<WindowInfo, AppError>;
    /// Integrity level of a process (e.g. 0x2000 medium, 0x3000 high), if it can be read.
    fn integrity_level(&self, pid: u32) -> Option<u32>;
    /// A no-op `SetWindowPos`: `ERROR_ACCESS_DENIED` means UIPI blocks us (`NeedsAdmin`).
    fn probe_access(&self, hwnd: isize) -> Result<(), AppError>;
    /// `(GWL_STYLE, GWL_EXSTYLE)`.
    fn styles(&self, hwnd: isize) -> Result<(u32, u32), AppError>;
    /// Writes both style words and returns what the window reports afterwards.
    fn set_styles(&self, hwnd: isize, style: u32, ex_style: u32) -> Result<(u32, u32), AppError>;
    fn placement(&self, hwnd: isize) -> Result<Placement, AppError>;
    fn set_placement(&self, hwnd: isize, placement: &Placement) -> Result<(), AppError>;
    /// `GetWindowRect`, screen coordinates.
    fn rect(&self, hwnd: isize) -> Result<Rect, AppError>;
    /// One `SetWindowPos` with FRAMECHANGED|NOACTIVATE|NOOWNERZORDER|NOSENDCHANGING|NOCOPYBITS
    /// (+ ASYNCWINDOWPOS when `async_pos`), then `InvalidateRect`.
    fn set_pos(&self, hwnd: isize, rect: Rect, z: ZOrder, async_pos: bool) -> Result<(), AppError>;
    fn show(&self, hwnd: isize, cmd: ShowCmd) -> Result<(), AppError>;
    /// `SetForegroundWindow`; false when Windows refused.
    fn set_foreground(&self, hwnd: isize) -> bool;
    /// Square corners and no DWM border while fitted; defaults when `on` is false.
    fn dwm_square(&self, hwnd: isize, on: bool);
    fn is_window(&self, hwnd: isize) -> bool;
    /// Left mouse button held right now (`GetAsyncKeyState`); diagnostics only.
    fn left_button_down(&self) -> bool;
    /// `IsWindowVisible` and not DWM-cloaked.
    fn is_visible(&self, hwnd: isize) -> bool;
    fn is_iconic(&self, hwnd: isize) -> bool;
    fn is_zoomed(&self, hwnd: isize) -> bool;
    /// Pid owning a window, if it exists.
    fn window_pid(&self, hwnd: isize) -> Option<u32>;
    /// Explorer-owned `Shell_TrayWnd`/`Shell_SecondaryTrayWnd`, class-verified, discovered fresh.
    fn tray_windows(&self) -> Vec<TrayWindow>;
    fn open_process(&self, pid: u32) -> Result<ProcessHandle, AppError>;
    /// `WaitForSingleObject(handle, 0)` signalled: the process is gone. Authoritative.
    fn process_exited(&self, handle: &ProcessHandle) -> bool;
    fn close_process(&self, handle: &ProcessHandle);
    /// True when a process with this pid and creation time is still running.
    fn process_alive(&self, pid: u32, created: u64) -> bool;
    fn monitors(&self) -> Vec<MonitorInfo>;
    /// Gives asynchronous window calls (ShowWindowAsync) a moment to land before a re-check.
    fn settle(&self);
}

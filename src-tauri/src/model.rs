//! Serde types shared by the engine, settings, the restore marker and IPC.
//!
//! Every persisted struct is `#[serde(default)]` so new fields are purely additive; enums are
//! tagged (`kind`) or snake_case strings. `src/types.ts` mirrors this file by hand; the
//! `json_shape` test at the bottom pins the wire format.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const MARKER_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+B";

// ---------------------------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------------------------

/// A rectangle in physical pixels: origin plus size.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Builds a rect from Win32-style edges.
    pub const fn from_edges(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { x: left, y: top, w: right - left, h: bottom - top }
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn area(&self) -> i64 {
        i64::from(self.w.max(0)) * i64::from(self.h.max(0))
    }

    /// Area shared with `other` (0 when they don't overlap).
    pub fn overlap_area(&self, other: &Rect) -> i64 {
        let w = self.right().min(other.right()) - self.x.max(other.x);
        let h = self.bottom().min(other.bottom()) - self.y.max(other.y);
        if w <= 0 || h <= 0 {
            return 0;
        }
        i64::from(w) * i64::from(h)
    }

    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}×{} at {}, {}", self.w, self.h, self.x, self.y)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// One display, in physical pixels (the process is Per-Monitor-V2 aware).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    /// GDI device name, e.g. `\\.\DISPLAY1`; the stable key stored in settings.
    pub device: String,
    /// "Display n", where n comes from the device name (matches Windows Settings numbering).
    pub name: String,
    pub rect: Rect,
    /// Work area: `rect` minus the taskbar.
    pub work: Rect,
    pub dpi: u32,
    pub primary: bool,
}

impl MonitorInfo {
    /// Display number parsed from `\\.\DISPLAYn` (0 when the name has another shape).
    pub fn number(&self) -> u32 {
        display_number(&self.device)
    }
}

pub fn display_number(device: &str) -> u32 {
    device.rsplit("DISPLAY").next().and_then(|digits| digits.parse().ok()).unwrap_or(0)
}

pub fn display_name(device: &str) -> String {
    match display_number(device) {
        0 => device.trim_start_matches(r"\\.\").to_string(),
        n => format!("Display {n}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    /// Normalized chord, e.g. `Ctrl+Alt+B` (see `hotkey::normalize`).
    pub hotkey: String,
    pub profile: FitProfile,
    pub launch_at_startup: bool,
    pub close_to_tray: bool,
    /// Set after the first successful fit; until then the status card shows first-run guidance.
    pub first_run_done: bool,
    /// False after importing the AHK script's F12 so the status card warns about it once.
    pub legacy_f12_ack: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            hotkey: DEFAULT_HOTKEY.to_string(),
            profile: FitProfile::default(),
            launch_at_startup: false,
            close_to_tray: true,
            first_run_done: false,
            legacy_f12_ack: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct FitProfile {
    pub region: RegionSpec,
    pub taskbar: TaskbarSettings,
}

/// Where the fitted window goes, relative to one monitor.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct RegionSpec {
    /// Device name of the monitor; empty means the primary monitor.
    pub monitor: String,
    pub preset: Preset,
    pub halign: HAlign,
    pub valign: VAlign,
    pub offset_x: i32,
    pub offset_y: i32,
    /// Hand-edited X/Y/W/H, monitor-relative (physical px from the monitor's top-left).
    /// Wins over preset and alignment; cleared by the next preset or align choice.
    pub override_rect: Option<Rect>,
}

impl Default for RegionSpec {
    fn default() -> Self {
        Self {
            monitor: String::new(),
            preset: Preset::Fixed { w: 2560, h: 1440 },
            halign: HAlign::Center,
            valign: VAlign::Center,
            offset_x: 0,
            offset_y: 0,
            override_rect: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Preset {
    /// Exact size in physical pixels.
    Fixed { w: i32, h: i32 },
    /// Full monitor height at this aspect ratio, capped at the monitor width.
    Aspect { num: u32, den: u32 },
    /// The whole monitor.
    Fill,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VAlign {
    Top,
    #[default]
    Center,
    Bottom,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TaskbarSettings {
    pub hide_taskbar: bool,
    pub hide_secondary_taskbars: bool,
}

impl Default for TaskbarSettings {
    fn default() -> Self {
        Self { hide_taskbar: true, hide_secondary_taskbars: false }
    }
}

// ---------------------------------------------------------------------------------------------
// Tracked window and restore marker
// ---------------------------------------------------------------------------------------------

/// Mirror of `WINDOWPLACEMENT` (workspace coordinates; only ever fed back to
/// `SetWindowPlacement`, never to `SetWindowPos`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Placement {
    pub show_cmd: u32,
    pub flags: u32,
    pub min_pos: Point,
    pub max_pos: Point,
    pub normal: Rect,
}

pub const SW_SHOWNORMAL: u32 = 1;
pub const SW_SHOWMINIMIZED: u32 = 2;
pub const SW_SHOWMAXIMIZED: u32 = 3;
pub const SW_MINIMIZE: u32 = 6;
pub const SW_SHOWMINNOACTIVE: u32 = 7;

impl Placement {
    pub fn show_state(&self) -> ShowState {
        match self.show_cmd {
            SW_SHOWMAXIMIZED => ShowState::Maximized,
            SW_SHOWMINIMIZED | SW_MINIMIZE | SW_SHOWMINNOACTIVE => ShowState::Minimized,
            _ => ShowState::Normal,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShowState {
    #[default]
    Normal,
    Maximized,
    Minimized,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DpiAwareness {
    Unaware,
    System,
    PerMonitor,
    PerMonitorV2,
    #[default]
    Unknown,
}

/// Everything needed to put a window back exactly as it was.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct SavedState {
    pub style: u32,
    pub ex_style: u32,
    pub topmost: bool,
    pub placement: Placement,
    /// `GetWindowRect` in screen coordinates: the only input to `set_pos` on restore.
    pub rect: Rect,
    /// True while the window was minimized/maximized/snapped at capture and `rect` has not been
    /// re-read after `SW_RESTORE`; restore then goes through `set_placement` instead.
    pub rect_provisional: bool,
    pub monitor: String,
    pub dpi: u32,
    pub dpi_awareness: DpiAwareness,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct TrackedWindow {
    pub hwnd: isize,
    pub pid: u32,
    pub tid: u32,
    /// Creation time of the process, so a recycled pid is never mistaken for the game.
    pub process_created: u64,
    pub exe: String,
    pub title: String,
    pub class: String,
    pub saved: SavedState,
    /// Absolute rect the window was fitted to.
    pub target: Rect,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct TaskbarMarker {
    pub taskbar_hidden: bool,
}

/// Written (and fsync'd) before the first mutation of a foreign window or tray; deleted only
/// after a full restore. Lets the next launch undo what a crashed BorderFit left behind.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct RestoreMarker {
    pub schema_version: u32,
    /// BorderFit's own pid when the marker was written.
    pub pid: u32,
    /// Unix seconds.
    pub written_at: u64,
    pub window: Option<TrackedWindow>,
    pub taskbar: TaskbarMarker,
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoEligibleReason {
    /// BorderFit itself was the foreground window.
    OwnWindow,
    /// The desktop, taskbar or another shell surface was the foreground window.
    ShellWindow,
    /// The foreground window can't be fitted (hidden, cloaked, a tool window, a UWP host).
    NotFittable,
    /// Tray/UI fit found no fittable window in the z-order.
    NoneFound,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppError {
    NeedsAdmin {
        exe: String,
        title: String,
    },
    WindowGone,
    NoEligibleWindow {
        reason: NoEligibleReason,
    },
    HotkeyInUse {
        chord: String,
    },
    HotkeyInvalid {
        chord: String,
        reason: String,
    },
    ResizeRefused {
        wanted: Rect,
        got: Rect,
        attempts: u8,
    },
    StyleRefused {
        wanted: u32,
        got: u32,
    },
    TaskbarRestoreFailed {
        hwnds: Vec<isize>,
    },
    RegionOffscreen {
        rect: Rect,
        monitor: String,
    },
    MonitorGone {
        device: String,
    },
    DpiMismatch {
        awareness: DpiAwareness,
        monitor_dpi: u32,
    },
    /// Invalid settings or an import that can't be used.
    Settings {
        message: String,
    },
    /// A file, process or runtime operation failed (marker or settings write, reading a file).
    System {
        message: String,
    },
    Win32 {
        api: String,
        code: u32,
    },
    Busy {
        title: String,
    },
    Timeout,
}

impl AppError {
    pub fn win32(api: &str, code: u32) -> Self {
        Self::Win32 { api: api.to_string(), code }
    }

    pub fn settings(message: impl Into<String>) -> Self {
        Self::Settings { message: message.into() }
    }

    pub fn system(message: impl Into<String>) -> Self {
        Self::System { message: message.into() }
    }

    /// Warnings leave the window fitted; everything else is a failure.
    pub fn is_warning(&self) -> bool {
        matches!(
            self,
            Self::ResizeRefused { .. }
                | Self::StyleRefused { .. }
                | Self::MonitorGone { .. }
                | Self::DpiMismatch { .. }
        )
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsAdmin { title, .. } => write!(
                f,
                "\"{title}\" runs as administrator, so Windows blocks BorderFit from moving it."
            ),
            Self::WindowGone => write!(f, "The window closed."),
            Self::NoEligibleWindow { reason } => match reason {
                NoEligibleReason::OwnWindow => write!(f, "Focus the game first, then press the hotkey."),
                NoEligibleReason::ShellWindow => {
                    write!(f, "The desktop or taskbar was active; focus the game first.")
                }
                NoEligibleReason::NotFittable => write!(f, "The active window can't be fitted."),
                NoEligibleReason::NoneFound => write!(f, "No window to fit was found."),
            },
            Self::HotkeyInUse { chord } => write!(f, "{chord} is already used by another app."),
            Self::HotkeyInvalid { reason, .. } => write!(f, "{reason}"),
            Self::ResizeRefused { wanted, got, .. } => write!(
                f,
                "Asked {}×{}, got {}×{}. The game enforces its own size.",
                wanted.w, wanted.h, got.w, got.h
            ),
            Self::StyleRefused { .. } => write!(f, "The window kept its borders."),
            Self::TaskbarRestoreFailed { .. } => write!(
                f,
                "The taskbar could not be shown again. Restart Explorer from Task Manager, then start BorderFit once."
            ),
            Self::RegionOffscreen { monitor, .. } => {
                write!(f, "Extends beyond {}.", display_name(monitor))
            }
            Self::MonitorGone { device } => {
                write!(f, "{} not found, using the primary display.", display_name(device))
            }
            Self::DpiMismatch { .. } => write!(
                f,
                "This game isn't DPI-aware; on a scaled display its size may be off."
            ),
            Self::Settings { message } | Self::System { message } => write!(f, "{message}"),
            Self::Win32 { api, code } => write!(f, "{api} failed (Windows error {code})."),
            Self::Busy { title } => write!(f, "\"{title}\" is fitted; restore it first."),
            Self::Timeout => write!(f, "BorderFit did not respond in time."),
        }
    }
}

impl std::error::Error for AppError {}

// ---------------------------------------------------------------------------------------------
// Status, notices, IPC payloads
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FitState {
    #[default]
    Idle,
    Fitted,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WindowSummary {
    pub hwnd: isize,
    pub pid: u32,
    pub exe: String,
    pub title: String,
}

/// What a restore will put back, for the status card's "Restores to …" line.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RestoreSummary {
    pub rect: Rect,
    pub show: ShowState,
    pub had_borders: bool,
}

/// The engine's state as the UI and tray see it; emitted as the `status` event.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct FitStatus {
    pub state: FitState,
    pub window: Option<WindowSummary>,
    /// Absolute target rect.
    pub target: Option<Rect>,
    /// Device name of the monitor the window was fitted on.
    pub monitor: Option<String>,
    pub restore: Option<RestoreSummary>,
    pub taskbar_hidden: bool,
    pub target_visible: bool,
    pub verify_attempts: u8,
    /// Set while fitted with a warning (size not accepted, DPI mismatch, borders kept).
    pub degraded: Option<AppError>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

/// A one-off message for the status card; emitted as the `notice` event.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Notice {
    pub level: NoticeLevel,
    pub error: Option<AppError>,
    pub message: String,
}

impl Notice {
    pub fn info(message: impl Into<String>) -> Self {
        Self { level: NoticeLevel::Info, error: None, message: message.into() }
    }

    pub fn from_error(error: AppError) -> Self {
        let level = if error.is_warning() { NoticeLevel::Warn } else { NoticeLevel::Error };
        Self { level, message: error.to_string(), error: Some(error) }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct HotkeyStatus {
    pub chord: String,
    /// The chord is live: registered, or heard through the keyboard hook.
    pub registered: bool,
    /// Heard through the keyboard hook because Windows refused to register it (e.g. F12).
    #[serde(default)]
    pub hooked: bool,
    pub error: Option<AppError>,
}

impl HotkeyStatus {
    /// A live chord with no error.
    pub fn active(chord: &str, binding: crate::hotkey::Binding) -> Self {
        Self {
            chord: chord.to_string(),
            registered: true,
            hooked: binding == crate::hotkey::Binding::Hook,
            error: None,
        }
    }

    /// A failed change: `chord` is the one that should still be live, `restored` says whether
    /// (and how) it is heard after the failure.
    pub fn failed(chord: &str, error: AppError, restored: Option<crate::hotkey::Binding>) -> Self {
        Self {
            chord: chord.to_string(),
            registered: restored.is_some(),
            hooked: restored == Some(crate::hotkey::Binding::Hook),
            error: Some(error),
        }
    }
}

/// Everything the UI needs on load (`get_app_state`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AppState {
    pub settings: Settings,
    pub status: FitStatus,
    pub monitors: Vec<MonitorInfo>,
    pub hotkey: HotkeyStatus,
    pub is_elevated: bool,
    pub os_build: String,
    pub log_dir: String,
    pub version: String,
    /// Startup notices (crash recovery, broken settings, …), returned once.
    pub notices: Vec<Notice>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ImportReport {
    pub settings: Settings,
    pub warnings: Vec<String>,
    /// One-line summary for the confirm prompt, e.g. "2560×1440 at 1280, 0, taskbar hidden, hotkey F12".
    pub summary: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TrayWindowInfo {
    pub hwnd: isize,
    pub class: String,
    pub exe: String,
    pub visible: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Diagnostics {
    pub version: String,
    pub dpi_context: String,
    pub elevated: bool,
    pub os_build: String,
    pub tray_windows: Vec<TrayWindowInfo>,
    pub marker_present: bool,
    pub hotkey: HotkeyStatus,
    pub monitors: Vec<MonitorInfo>,
    pub log_tail: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rect_overlap_and_edges() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        assert_eq!(a.overlap_area(&b), 2500);
        assert_eq!(a.overlap_area(&Rect::new(100, 0, 10, 10)), 0);
        assert_eq!(Rect::from_edges(-2560, 0, 0, 1440), Rect::new(-2560, 0, 2560, 1440));
        assert!(a.contains_point(99, 0));
        assert!(!a.contains_point(100, 0));
    }

    #[test]
    fn display_names_come_from_the_device() {
        assert_eq!(display_number(r"\\.\DISPLAY2"), 2);
        assert_eq!(display_name(r"\\.\DISPLAY12"), "Display 12");
        assert_eq!(display_name("odd"), "odd");
    }

    /// Pins the wire format that `src/types.ts` mirrors. Change both together.
    #[test]
    fn json_shape() {
        let settings = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(
            settings,
            json!({
                "schema_version": 1,
                "hotkey": "Ctrl+Alt+B",
                "profile": {
                    "region": {
                        "monitor": "",
                        "preset": { "kind": "fixed", "w": 2560, "h": 1440 },
                        "halign": "center",
                        "valign": "center",
                        "offset_x": 0,
                        "offset_y": 0,
                        "override_rect": null
                    },
                    "taskbar": {
                        "hide_taskbar": true,
                        "hide_secondary_taskbars": false
                    }
                },
                "launch_at_startup": false,
                "close_to_tray": true,
                "first_run_done": false,
                "legacy_f12_ack": false
            })
        );

        let status = FitStatus {
            state: FitState::Fitted,
            window: Some(WindowSummary {
                hwnd: 42,
                pid: 7,
                exe: "game.exe".into(),
                title: "Game".into(),
            }),
            target: Some(Rect::new(1280, 0, 2560, 1440)),
            monitor: Some(r"\\.\DISPLAY1".into()),
            restore: Some(RestoreSummary {
                rect: Rect::new(100, 100, 1920, 1080),
                show: ShowState::Normal,
                had_borders: true,
            }),
            taskbar_hidden: true,
            target_visible: true,
            verify_attempts: 0,
            degraded: Some(AppError::ResizeRefused {
                wanted: Rect::new(1280, 0, 2560, 1440),
                got: Rect::new(1280, 0, 2560, 1417),
                attempts: 4,
            }),
        };
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({
                "state": "fitted",
                "window": { "hwnd": 42, "pid": 7, "exe": "game.exe", "title": "Game" },
                "target": { "x": 1280, "y": 0, "w": 2560, "h": 1440 },
                "monitor": "\\\\.\\DISPLAY1",
                "restore": {
                    "rect": { "x": 100, "y": 100, "w": 1920, "h": 1080 },
                    "show": "normal",
                    "had_borders": true
                },
                "taskbar_hidden": true,
                "target_visible": true,
                "verify_attempts": 0,
                "degraded": {
                    "kind": "resize_refused",
                    "wanted": { "x": 1280, "y": 0, "w": 2560, "h": 1440 },
                    "got": { "x": 1280, "y": 0, "w": 2560, "h": 1417 },
                    "attempts": 4
                }
            })
        );

        assert_eq!(
            serde_json::to_value(AppError::NoEligibleWindow {
                reason: NoEligibleReason::OwnWindow
            })
            .unwrap(),
            json!({ "kind": "no_eligible_window", "reason": "own_window" })
        );
        assert_eq!(serde_json::to_value(AppError::Timeout).unwrap(), json!({ "kind": "timeout" }));
        assert_eq!(
            serde_json::to_value(Preset::Aspect { num: 16, den: 9 }).unwrap(),
            json!({ "kind": "aspect", "num": 16, "den": 9 })
        );
        assert_eq!(serde_json::to_value(Preset::Fill).unwrap(), json!({ "kind": "fill" }));
    }

    #[test]
    fn unknown_and_missing_fields_fall_back_to_defaults() {
        let parsed: Settings =
            serde_json::from_value(json!({ "hotkey": "F9", "future_field": 1 })).unwrap();
        assert_eq!(parsed.hotkey, "F9");
        assert_eq!(parsed.profile, FitProfile::default());
        assert!(parsed.close_to_tray);
    }
}

// Hand-written mirror of src-tauri/src/model.rs (snake_case fields, `kind`-tagged enums).
// The `json_shape` test in model.rs pins the wire format; change both files together.

export interface Rect {
    x: number
    y: number
    w: number
    h: number
}

export interface MonitorInfo {
    /** GDI device name, e.g. `\\.\DISPLAY1`. */
    device: string
    /** "Display n". */
    name: string
    rect: Rect
    /** Work area: rect minus the taskbar. */
    work: Rect
    dpi: number
    primary: boolean
}

export type Preset =
    | { kind: 'fixed'; w: number; h: number }
    | { kind: 'aspect'; num: number; den: number }
    | { kind: 'fill' }

export type HAlign = 'left' | 'center' | 'right'
export type VAlign = 'top' | 'center' | 'bottom'

export interface RegionSpec {
    /** Device name; empty = primary monitor. */
    monitor: string
    preset: Preset
    halign: HAlign
    valign: VAlign
    offset_x: number
    offset_y: number
    /** Hand-edited rect, monitor-relative. Wins over preset and alignment. */
    override_rect: Rect | null
}

export interface TaskbarSettings {
    hide_taskbar: boolean
    hide_secondary_taskbars: boolean
}

export interface FitProfile {
    region: RegionSpec
    taskbar: TaskbarSettings
}

export interface Settings {
    schema_version: number
    hotkey: string
    profile: FitProfile
    launch_at_startup: boolean
    close_to_tray: boolean
    first_run_done: boolean
    legacy_f12_ack: boolean
}

export type ShowState = 'normal' | 'maximized' | 'minimized'
export type DpiAwareness = 'unaware' | 'system' | 'per_monitor' | 'per_monitor_v2' | 'unknown'
export type NoEligibleReason = 'own_window' | 'shell_window' | 'not_fittable' | 'none_found'

export type AppError =
    | { kind: 'needs_admin'; exe: string; title: string }
    | { kind: 'window_gone' }
    | { kind: 'no_eligible_window'; reason: NoEligibleReason }
    | { kind: 'hotkey_in_use'; chord: string }
    | { kind: 'hotkey_invalid'; chord: string; reason: string }
    | { kind: 'resize_refused'; wanted: Rect; got: Rect; attempts: number }
    | { kind: 'style_refused'; wanted: number; got: number }
    | { kind: 'taskbar_restore_failed'; hwnds: number[] }
    | { kind: 'region_offscreen'; rect: Rect; monitor: string }
    | { kind: 'monitor_gone'; device: string }
    | { kind: 'dpi_mismatch'; awareness: DpiAwareness; monitor_dpi: number }
    | { kind: 'settings'; message: string }
    | { kind: 'system'; message: string }
    | { kind: 'win32'; api: string; code: number }
    | { kind: 'busy'; title: string }
    | { kind: 'timeout' }

export type FitState = 'idle' | 'fitted'

export interface WindowSummary {
    hwnd: number
    pid: number
    exe: string
    title: string
}

export interface RestoreSummary {
    rect: Rect
    show: ShowState
    had_borders: boolean
}

export interface FitStatus {
    state: FitState
    window: WindowSummary | null
    /** Absolute target rect. */
    target: Rect | null
    /** Device of the monitor the window was fitted on. */
    monitor: string | null
    restore: RestoreSummary | null
    taskbar_hidden: boolean
    target_visible: boolean
    verify_attempts: number
    degraded: AppError | null
}

export type NoticeLevel = 'info' | 'warn' | 'error'

export interface Notice {
    level: NoticeLevel
    error: AppError | null
    message: string
}

export interface HotkeyStatus {
    chord: string
    /** The chord is live: registered, or heard through the keyboard hook. */
    registered: boolean
    /** Windows refused to register it (e.g. F12), so a keyboard hook listens instead. */
    hooked: boolean
    error: AppError | null
}

export interface AppState {
    settings: Settings
    status: FitStatus
    monitors: MonitorInfo[]
    hotkey: HotkeyStatus
    is_elevated: boolean
    os_build: string
    log_dir: string
    version: string
    /** Startup notices (crash recovery, broken settings, ...), delivered once. */
    notices: Notice[]
}

export interface ImportReport {
    settings: Settings
    warnings: string[]
    summary: string
}

export interface TrayWindowInfo {
    hwnd: number
    class: string
    exe: string
    visible: boolean
}

export interface Diagnostics {
    version: string
    dpi_context: string
    elevated: boolean
    os_build: string
    tray_windows: TrayWindowInfo[]
    marker_present: boolean
    hotkey: HotkeyStatus
    monitors: MonitorInfo[]
    log_tail: string[]
}

export type LinkTarget = 'repo' | 'issues' | 'logs'

export const DEFAULT_HOTKEY = 'Ctrl+Alt+B'

/** Mirrors `Settings::default()` in model.rs. */
export function defaultSettings(): Settings {
    return {
        schema_version: 1,
        hotkey: DEFAULT_HOTKEY,
        profile: {
            region: {
                monitor: '',
                preset: { kind: 'fixed', w: 2560, h: 1440 },
                halign: 'center',
                valign: 'center',
                offset_x: 0,
                offset_y: 0,
                override_rect: null,
            },
            taskbar: { hide_taskbar: true, hide_secondary_taskbars: false },
        },
        launch_at_startup: false,
        close_to_tray: true,
        first_run_done: false,
        legacy_f12_ack: false,
    }
}

/** Mirrors `FitStatus::default()` in model.rs: nothing fitted. */
export function idleStatus(): FitStatus {
    return {
        state: 'idle',
        window: null,
        target: null,
        monitor: null,
        restore: null,
        taskbar_hidden: false,
        target_visible: false,
        verify_attempts: 0,
        degraded: null,
    }
}

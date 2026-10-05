// The whole UI state: one module-level reactive object, filled from `get_app_state` and kept
// current by the `status`, `notice`, `hotkey` and `settings` events. Rust owns the truth; this module only
// mirrors it, edits settings (debounced save) and asks Rust to preview the region.

import { computed, reactive } from 'vue'
import { api, on, onFileDrop, onWindowFocus, toNotice } from './api'
import { chipFor, monitorFor, presetFits, presetsFor, relativeTo, type SizeChip } from './geometry'
import {
    DEFAULT_HOTKEY,
    defaultSettings,
    idleStatus,
    type AppError,
    type AppState,
    type FitStatus,
    type HAlign,
    type HotkeyStatus,
    type ImportReport,
    type MonitorInfo,
    type Notice,
    type NoticeLevel,
    type Rect,
    type RegionSpec,
    type Settings,
} from './types'

/** A one-line message under the status card. */
export interface LineNotice {
    id: number
    level: NoticeLevel
    message: string
}

export interface UiState {
    ready: boolean
    loadError: string | null
    settings: Settings
    status: FitStatus
    monitors: MonitorInfo[]
    hotkey: HotkeyStatus
    isElevated: boolean
    osBuild: string
    version: string
    /** One-line notices, newest first. */
    notices: LineNotice[]
    /** An actionable error that takes over the status card (needs admin, hotkey conflict). */
    alert: Notice | null
    /** "Focus the game first" hint after pressing the hotkey inside BorderFit (3 s). */
    focusHint: boolean
    /** The region resolved by Rust (absolute), or the offending rect when it is offscreen. */
    preview: Rect | null
    previewError: AppError | null
    pendingImport: ImportReport | null
    /** Bumped to ask the hotkey recorder to start recording ("Change hotkey"). */
    recordRequest: number
}

const SAVE_DEBOUNCE_MS = 200
const FOCUS_HINT_MS = 3000
const INFO_NOTICE_MS = 8000

export const state = reactive<UiState>({
    ready: false,
    loadError: null,
    // Placeholders until `get_app_state` answers; nothing renders before `ready`.
    settings: defaultSettings(),
    status: idleStatus(),
    monitors: [],
    hotkey: { chord: DEFAULT_HOTKEY, registered: true, hooked: false, error: null },
    isElevated: false,
    osBuild: '',
    version: '',
    notices: [],
    alert: null,
    focusHint: false,
    preview: null,
    previewError: null,
    pendingImport: null,
    recordRequest: 0,
})

// ---------------------------------------------------------------------------------------------
// Derived values
// ---------------------------------------------------------------------------------------------

export const region = computed(() => state.settings.profile.region)

/** The monitor the region points at; the primary when the configured one is missing. */
export const selectedMonitor = computed<MonitorInfo | null>(
    () => monitorFor(state.monitors, region.value.monitor) ?? monitorFor(state.monitors, '') ?? null
)

/** True when the configured monitor is not connected (the fit falls back to the primary). */
export const monitorMissing = computed(
    () =>
        state.monitors.length > 0 &&
        region.value.monitor !== '' &&
        monitorFor(state.monitors, region.value.monitor) === null
)

export const chips = computed<SizeChip[]>(() =>
    selectedMonitor.value ? presetsFor(selectedMonitor.value) : []
)

/** The chip for the stored preset; none while the fields were hand-edited. */
export const selectedChip = computed<SizeChip | null>(() => {
    const monitor = selectedMonitor.value
    if (!monitor || region.value.override_rect) {
        return null
    }
    return chipFor(chips.value, monitor.rect, region.value.preset)
})

/** The preview rect relative to the selected monitor (what the X/Y/W/H fields show). */
export const relativePreview = computed<Rect | null>(() => {
    const rect = state.preview
    const monitor = selectedMonitor.value
    return rect && monitor ? relativeTo(rect, monitor) : null
})

// ---------------------------------------------------------------------------------------------
// Notices
// ---------------------------------------------------------------------------------------------

let nextNoticeId = 1
let focusHintTimer: ReturnType<typeof setTimeout> | undefined

export function pushNotice(level: NoticeLevel, message: string, expires = level === 'info'): void {
    const id = nextNoticeId++
    state.notices = [{ id, level, message }, ...state.notices.filter((n) => n.message !== message)]
    state.notices.splice(3)
    if (expires) {
        setTimeout(() => dismissNotice(id), INFO_NOTICE_MS)
    }
}

export function dismissNotice(id: number): void {
    state.notices = state.notices.filter((n) => n.id !== id)
}

export function dismissAlert(): void {
    state.alert = null
}

function showFocusHint(): void {
    state.focusHint = true
    clearTimeout(focusHintTimer)
    focusHintTimer = setTimeout(() => (state.focusHint = false), FOCUS_HINT_MS)
}

/** Routes a notice to the right surface: the focus hint, the alert card, or a line. */
function handleNotice(notice: Notice): void {
    const error = notice.error
    if (error?.kind === 'no_eligible_window' && error.reason === 'own_window') {
        showFocusHint()
        return
    }
    const activeHotkeyLost = error?.kind === 'hotkey_in_use' && !state.hotkey.registered
    if (error?.kind === 'needs_admin' || activeHotkeyLost) {
        state.alert = notice
        return
    }
    pushNotice(notice.level, notice.message)
}

/** Shows a failed call as a notice line. */
export function reportError(error: unknown): void {
    const notice = toNotice(error)
    handleNotice(notice)
}

// ---------------------------------------------------------------------------------------------
// Events from Rust
// ---------------------------------------------------------------------------------------------

function handleStatus(status: FitStatus): void {
    state.status = status
    if (status.state === 'fitted' && state.alert?.error?.kind === 'needs_admin') {
        state.alert = null
    }
    void refreshMonitors()
}

function handleHotkey(hotkey: HotkeyStatus): void {
    state.hotkey = hotkey
    state.settings.hotkey = hotkey.chord
    // Only a conflict on the *active* chord takes over the card; a failed change keeps the old
    // chord working and is explained inline by the recorder.
    if (hotkey.error?.kind === 'hotkey_in_use' && !hotkey.registered) {
        state.alert = {
            level: 'error',
            error: hotkey.error,
            message: `${hotkey.error.chord} is already used by another app.`,
        }
    } else if (state.alert?.error?.kind === 'hotkey_in_use') {
        state.alert = null
    }
}

/**
 * Rust changed settings on its own (a recorded hotkey, the first fit). Only the fields Rust
 * writes are copied: replacing the whole object would drop local edits still waiting for the
 * debounced save, and keeping the old values would save them back over Rust's change.
 */
function handleSettings(saved: Settings): void {
    state.settings.hotkey = saved.hotkey
    state.settings.first_run_done = saved.first_run_done
    state.settings.legacy_f12_ack = saved.legacy_f12_ack
}

// ---------------------------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------------------------

function applyAppState(app: AppState): void {
    state.settings = app.settings
    state.status = app.status
    state.monitors = app.monitors
    state.isElevated = app.is_elevated
    state.osBuild = app.os_build
    state.version = app.version
    handleHotkey(app.hotkey)
    for (const notice of app.notices) {
        pushNotice(notice.level, notice.message, false)
    }
}

/**
 * Events that arrive before the `get_app_state` snapshot is applied. They can be newer than the
 * snapshot, so they are replayed after it instead of being overwritten by it.
 */
let earlyEvents: (() => void)[] | null = []

/** Wraps an event handler so it waits for the startup snapshot. */
function afterSnapshot<T>(handler: (payload: T) => void): (payload: T) => void {
    return (payload) => {
        if (earlyEvents) {
            earlyEvents.push(() => handler(payload))
        } else {
            handler(payload)
        }
    }
}

function replayEarlyEvents(): void {
    const events = earlyEvents ?? []
    earlyEvents = null
    events.forEach((replay) => replay())
}

/** Loads the state and subscribes to everything; call once from main.ts. */
export async function init(): Promise<void> {
    await Promise.all([
        on('status', afterSnapshot(handleStatus)),
        on('notice', afterSnapshot(handleNotice)),
        on('hotkey', afterSnapshot(handleHotkey)),
        on('settings', afterSnapshot(handleSettings)),
        onWindowFocus(() => void refreshMonitors()),
        onFileDrop((paths) => void importIni(paths)),
    ])
    try {
        applyAppState(await api.getAppState())
        replayEarlyEvents()
        state.ready = true
        await refreshPreview()
    } catch (error) {
        replayEarlyEvents()
        state.loadError = toNotice(error).message
    }
}

export async function refreshMonitors(): Promise<void> {
    try {
        state.monitors = await api.listMonitors()
        await refreshPreview()
    } catch (error) {
        reportError(error)
    }
}

// ---------------------------------------------------------------------------------------------
// Region preview and settings
// ---------------------------------------------------------------------------------------------

let previewSeq = 0

/** Asks Rust where the current region lands; keeps only the newest answer. */
export async function refreshPreview(): Promise<void> {
    const seq = ++previewSeq
    const spec: RegionSpec = clone(region.value)
    try {
        const rect = await api.previewRegion(spec)
        if (seq === previewSeq) {
            state.preview = rect
            state.previewError = null
        }
    } catch (error) {
        if (seq !== previewSeq) return
        const notice = toNotice(error)
        state.previewError = notice.error
        state.preview = notice.error?.kind === 'region_offscreen' ? notice.error.rect : null
    }
}

let saveTimer: ReturnType<typeof setTimeout> | undefined
let editRevision = 0

/** Applies a local edit and saves it (debounced). Region edits re-run the preview at once. */
export function updateSettings(edit: (settings: Settings) => void): void {
    const before = JSON.stringify(region.value)
    edit(state.settings)
    editRevision++
    if (JSON.stringify(region.value) !== before) {
        void refreshPreview()
    }
    clearTimeout(saveTimer)
    saveTimer = setTimeout(() => void saveNow(), SAVE_DEBOUNCE_MS)
}

/**
 * Saves the current settings; adopts Rust's normalized copy unless newer edits exist. Returns
 * false (after reporting the error) when the save failed.
 */
export async function saveNow(): Promise<boolean> {
    clearTimeout(saveTimer)
    const revision = editRevision
    try {
        const saved = await api.saveSettings(clone(state.settings))
        if (revision === editRevision) {
            state.settings = saved
        }
        return true
    } catch (error) {
        reportError(error)
        return false
    }
}

export function selectMonitor(monitor: MonitorInfo): void {
    updateSettings((s) => {
        const r = s.profile.region
        r.monitor = monitor.primary ? '' : monitor.device
        r.override_rect = null
        if (!presetFits(monitor.rect, r.preset)) {
            r.preset = { kind: 'fill' }
        }
    })
}

export function selectChip(chip: SizeChip): void {
    updateSettings((s) => {
        s.profile.region.preset = chip.preset
        s.profile.region.override_rect = null
    })
}

export function selectAlign(halign: HAlign): void {
    updateSettings((s) => {
        s.profile.region.halign = halign
        s.profile.region.override_rect = null
    })
}

/** A hand-edited X/Y/W/H field: becomes a monitor-relative override of the shown rect. */
export function setField(field: keyof Rect, value: number): void {
    const current = relativePreview.value
    if (!current || current[field] === value) {
        return
    }
    updateSettings((s) => {
        s.profile.region.override_rect = { ...current, [field]: value }
    })
}

// ---------------------------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------------------------

export async function restore(): Promise<void> {
    try {
        await api.restoreWindow()
    } catch (error) {
        reportError(error)
    }
}

export async function relaunchElevated(): Promise<void> {
    try {
        await api.relaunchElevated()
    } catch (error) {
        reportError(error)
    }
}

export function requestHotkeyChange(): void {
    state.recordRequest++
}

export function acknowledgeF12(): void {
    updateSettings((s) => (s.legacy_f12_ack = true))
}

async function importIni(paths: string[]): Promise<void> {
    const path = paths.find((p) => p.toLowerCase().endsWith('.ini'))
    if (!path) {
        if (paths.length > 0) pushNotice('info', 'Drop borderless_config.ini to import it.')
        return
    }
    try {
        state.pendingImport = await api.importAhkIni(path)
    } catch (error) {
        reportError(error)
    }
}

export async function confirmImport(): Promise<void> {
    const report = state.pendingImport
    if (!report) return
    state.pendingImport = null
    state.settings = clone(report.settings)
    editRevision++
    const saved = await saveNow()
    await refreshPreview()
    if (saved) {
        pushNotice('info', 'Imported the AutoHotkey settings.')
    }
}

export function cancelImport(): void {
    state.pendingImport = null
}

/** A plain deep copy (drops the reactive proxy) for sending over IPC. */
function clone<T>(value: T): T {
    return JSON.parse(JSON.stringify(value)) as T
}

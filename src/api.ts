// Typed wrappers around the Rust commands and events. In a plain browser (`pnpm dev` without
// Tauri) everything is routed to the in-memory mock in lib/ipc.mock.ts.

import { invoke as tauriInvoke } from '@tauri-apps/api/core'
import { listen as tauriListen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type {
    AppState,
    Diagnostics,
    FitStatus,
    HotkeyStatus,
    ImportReport,
    LinkTarget,
    MonitorInfo,
    Notice,
    Rect,
    RegionSpec,
    Settings,
} from './types'

export const isTauri = '__TAURI_INTERNALS__' in window

type MockModule = typeof import('./lib/ipc.mock')
let mockModule: Promise<MockModule> | null = null

/**
 * The mock backs `pnpm dev` in a plain browser. Production builds replace `DEV` with false, so
 * the import below is dropped and the mock never ships.
 */
function mock(): Promise<MockModule> {
    if (!import.meta.env.DEV) {
        return Promise.reject(new Error('BorderFit must run inside its own window.'))
    }
    mockModule ??= import('./lib/ipc.mock')
    return mockModule
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    try {
        if (isTauri) {
            return await tauriInvoke<T>(command, args)
        }
        return await (await mock()).invoke<T>(command, args)
    } catch (error) {
        throw toNotice(error)
    }
}

/** Commands reject with a serialized `Notice`; anything else (IPC failures) is wrapped. */
export function toNotice(error: unknown): Notice {
    if (error && typeof error === 'object' && 'message' in error && 'level' in error) {
        return error as Notice
    }
    const message = error instanceof Error ? error.message : String(error)
    return { level: 'error', error: null, message }
}

export interface EventMap {
    status: FitStatus
    notice: Notice
    hotkey: HotkeyStatus
    settings: Settings
}

export async function on<K extends keyof EventMap>(
    event: K,
    handler: (payload: EventMap[K]) => void
): Promise<UnlistenFn> {
    if (isTauri) {
        return tauriListen<EventMap[K]>(event, (e) => handler(e.payload))
    }
    return (await mock()).listen(event, handler)
}

/** Calls `handler` whenever the BorderFit window gains focus. */
export async function onWindowFocus(handler: () => void): Promise<UnlistenFn> {
    if (isTauri) {
        return getCurrentWindow().onFocusChanged(({ payload: focused }) => {
            if (focused) handler()
        })
    }
    window.addEventListener('focus', handler)
    return () => window.removeEventListener('focus', handler)
}

/** Calls `handler` with the paths of files dropped on the window (Tauri only). */
export async function onFileDrop(handler: (paths: string[]) => void): Promise<UnlistenFn> {
    if (!isTauri) {
        return () => {}
    }
    return getCurrentWebview().onDragDropEvent(({ payload }) => {
        if (payload.type === 'drop') handler(payload.paths)
    })
}

export const api = {
    getAppState: () => call<AppState>('get_app_state'),
    saveSettings: (settings: Settings) => call<Settings>('save_settings', { settings }),
    previewRegion: (spec: RegionSpec) => call<Rect>('preview_region', { spec }),
    listMonitors: () => call<MonitorInfo[]>('list_monitors'),
    restoreWindow: () => call<null>('restore_window'),
    validateHotkey: (chord: string) => call<string>('validate_hotkey', { chord }),
    beginHotkeyCapture: () => call<HotkeyStatus>('begin_hotkey_capture'),
    endHotkeyCapture: (chord: string | null) => call<HotkeyStatus>('end_hotkey_capture', { chord }),
    importAhkIni: (path: string) => call<ImportReport>('import_ahk_ini', { path }),
    relaunchElevated: () => call<null>('relaunch_elevated'),
    openLink: (target: LinkTarget) => call<null>('open_link', { target }),
    getDiagnostics: () => call<Diagnostics>('get_diagnostics'),
}

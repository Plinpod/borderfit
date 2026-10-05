// In-memory stand-in for the Rust side, used when the UI runs in a plain browser (`pnpm dev`).
// Fixture: 5120x1440 primary + 1920x1080 secondary on its left. `?state=` picks a scenario:
// firstrun | fitted | degraded | admin | conflict | recovered | f12 | elevated | display2 |
// offscreen.

import type {
    AppError,
    AppState,
    Diagnostics,
    FitStatus,
    HotkeyStatus,
    MonitorInfo,
    Notice,
    Rect,
    RegionSpec,
    Settings,
} from '../types'
import { defaultSettings, idleStatus } from '../types'
import type { EventMap } from '../api'
import { presetSize } from '../geometry'

type Handler = (payload: never) => void

const scenario = new URLSearchParams(window.location.search).get('state') ?? ''
const handlers = new Map<string, Set<Handler>>()

const monitors: MonitorInfo[] = [
    {
        device: '\\\\.\\DISPLAY1',
        name: 'Display 1',
        rect: { x: 0, y: 0, w: 5120, h: 1440 },
        work: { x: 0, y: 0, w: 5120, h: 1392 },
        dpi: 96,
        primary: true,
    },
    {
        device: '\\\\.\\DISPLAY2',
        name: 'Display 2',
        rect: { x: -1920, y: 0, w: 1920, h: 1080 },
        work: { x: -1920, y: 0, w: 1920, h: 1032 },
        dpi: 144,
        primary: false,
    },
]

const settings: Settings = {
    ...defaultSettings(),
    hotkey: scenario === 'f12' ? 'F12' : 'Ctrl+Alt+B',
    first_run_done: scenario !== 'firstrun',
}

let hotkey: HotkeyStatus = {
    chord: settings.hotkey,
    registered: scenario !== 'conflict',
    hooked: scenario === 'f12',
    error: scenario === 'conflict' ? { kind: 'hotkey_in_use', chord: settings.hotkey } : null,
}

let status: FitStatus = idleStatus()
let startupNotices: Notice[] = []

function fittedStatus(degraded: AppError | null = null): FitStatus {
    const target = resolve(settings.profile.region)
    return {
        state: 'fitted',
        window: { hwnd: 0x1a2b, pid: 4242, exe: 'eldenring.exe', title: 'Elden Ring' },
        target,
        monitor: pickMonitor(settings.profile.region.monitor).device,
        restore: {
            rect: { x: 100, y: 100, w: 1920, h: 1080 },
            show: 'normal',
            had_borders: true,
        },
        taskbar_hidden: settings.profile.taskbar.hide_taskbar,
        target_visible: true,
        verify_attempts: degraded ? 4 : 0,
        degraded,
    }
}

function applyScenario(): void {
    if (scenario === 'display2') {
        settings.profile.region.monitor = monitors[1].device
        settings.profile.region.preset = { kind: 'aspect', num: 4, den: 3 }
        settings.profile.taskbar.hide_secondary_taskbars = true
    }
    if (scenario === 'offscreen') {
        settings.profile.region.override_rect = { x: 4600, y: 0, w: 2560, h: 1440 }
    }
    if (scenario === 'fitted') {
        status = fittedStatus()
    }
    if (scenario === 'degraded') {
        const wanted = resolve(settings.profile.region)
        status = fittedStatus({
            kind: 'resize_refused',
            wanted,
            got: { ...wanted, h: wanted.h - 23 },
            attempts: 4,
        })
    }
    if (scenario === 'recovered') {
        status = fittedStatus()
        startupNotices = [
            {
                level: 'warn',
                error: null,
                message:
                    'BorderFit exited unexpectedly while "Elden Ring" was fitted. It is still tracked; press the hotkey or click Restore.',
            },
        ]
    }
    if (scenario === 'admin') {
        setTimeout(
            () =>
                emit('notice', {
                    level: 'error',
                    error: { kind: 'needs_admin', exe: 'eldenring.exe', title: 'Elden Ring' },
                    message:
                        '"Elden Ring" runs as administrator, so Windows blocks BorderFit from moving it.',
                }),
            300
        )
    }
}
applyScenario()

function emit<K extends keyof EventMap>(event: K, payload: EventMap[K]): void {
    for (const handler of handlers.get(event) ?? []) {
        ;(handler as (p: EventMap[K]) => void)(payload)
    }
}

export async function listen<K extends keyof EventMap>(
    event: K,
    handler: (payload: EventMap[K]) => void
): Promise<() => void> {
    const set = handlers.get(event) ?? new Set<Handler>()
    set.add(handler as Handler)
    handlers.set(event, set)
    return () => set.delete(handler as Handler)
}

function pickMonitor(device: string): MonitorInfo {
    const primary = monitors.find((m) => m.primary) ?? monitors[0]
    return device ? (monitors.find((m) => m.device === device) ?? primary) : primary
}

/** Port of region.rs `resolve` + `validate`, for the mock only. */
function resolve(spec: RegionSpec): Rect {
    const mon = pickMonitor(spec.monitor).rect
    let rect: Rect
    if (spec.override_rect) {
        const o = spec.override_rect
        rect = { x: mon.x + o.x, y: mon.y + o.y, w: o.w, h: o.h }
    } else {
        const [w, h] = presetSize(mon, spec.preset)
        const room = (total: number, size: number, align: string) =>
            align === 'left' || align === 'top'
                ? 0
                : align === 'right' || align === 'bottom'
                  ? total - size
                  : Math.floor((total - size) / 2)
        rect = {
            x: mon.x + room(mon.w, w, spec.halign) + spec.offset_x,
            y: mon.y + room(mon.h, h, spec.valign) + spec.offset_y,
            w,
            h,
        }
    }
    const overlapW = Math.min(rect.x + rect.w, mon.x + mon.w) - Math.max(rect.x, mon.x)
    const overlapH = Math.min(rect.y + rect.h, mon.y + mon.h) - Math.max(rect.y, mon.y)
    const overlap = Math.max(0, overlapW) * Math.max(0, overlapH)
    if (overlap * 4 < rect.w * rect.h) {
        const monitor = pickMonitor(spec.monitor).device
        throw notice(
            'warn',
            { kind: 'region_offscreen', rect, monitor },
            'Extends beyond the display.'
        )
    }
    return rect
}

function notice(level: Notice['level'], error: AppError | null, message: string): Notice {
    return { level, error, message }
}

const BARE_KEYS = /^(F([1-9]|1\d|2[0-4])|Pause|ScrollLock)$/

/** A rough port of hotkey::normalize, enough to exercise the recorder. */
function normalize(chord: string): string {
    const parts = chord.split('+').map((p) => p.trim())
    const mods = ['Ctrl', 'Alt', 'Shift', 'Super'].filter((m) => parts.includes(m))
    const keys = parts.filter((p) => !['Ctrl', 'Alt', 'Shift', 'Super'].includes(p))
    const invalid = (reason: string) =>
        notice('warn', { kind: 'hotkey_invalid', chord, reason }, reason)
    if (keys.length !== 1) throw invalid('Add a key to the modifiers.')
    const key = keys[0].replace(/^Key([A-Z])$/, '$1').replace(/^Digit(\d)$/, '$1')
    if (/^(Control|Alt|Shift|Meta|OS)(Left|Right)$/.test(key) || key === 'ContextMenu') {
        throw invalid("That key can't be used as a hotkey.")
    }
    const realModifier = mods.some((m) => m !== 'Shift')
    if (!realModifier && !BARE_KEYS.test(key)) {
        throw invalid("Add Ctrl, Alt or Win so typing isn't affected.")
    }
    return [...mods, key].join('+')
}

const delay = (ms: number) => new Promise((done) => setTimeout(done, ms))

async function toggleFit(): Promise<null> {
    await delay(150)
    if (status.state === 'fitted') {
        status = idleStatus()
    } else {
        status = fittedStatus()
    }
    emit('status', status)
    if (status.state === 'fitted' && !settings.first_run_done) {
        settings.first_run_done = true
        emit('settings', structuredClone(settings))
    }
    return null
}

function diagnostics(): Diagnostics {
    return {
        version: '0.1.0',
        dpi_context: 'per_monitor_aware_v2',
        elevated: false,
        os_build: '26200.6584',
        tray_windows: [
            { hwnd: 0x10096, class: 'Shell_TrayWnd', exe: 'explorer.exe', visible: true },
            { hwnd: 0x200a4, class: 'Shell_SecondaryTrayWnd', exe: 'explorer.exe', visible: true },
        ],
        marker_present: status.state === 'fitted',
        hotkey,
        monitors,
        log_tail: [
            '[2026-09-22][21:52:01][INFO] BorderFit 0.1.0 on Windows 26200.6584 (mock)',
            '[2026-09-22][21:52:01][INFO] dpi context: per_monitor_aware_v2, elevated: false',
            '[2026-09-22][21:52:01][INFO] hotkey: registered Ctrl+Alt+B',
        ],
    }
}

const commands: Record<string, (args: Record<string, unknown>) => unknown> = {
    get_app_state: (): AppState => {
        const state: AppState = {
            settings: structuredClone(settings),
            status,
            monitors,
            hotkey,
            is_elevated: scenario === 'elevated',
            os_build: '26200.6584',
            log_dir: 'C:\\Users\\you\\AppData\\Local\\app.borderfit\\logs',
            version: '0.1.0',
            notices: startupNotices,
        }
        startupNotices = []
        return state
    },
    save_settings: (args) => {
        const next = args.settings as Settings
        Object.assign(settings, structuredClone(next))
        if (settings.hotkey !== hotkey.chord) {
            hotkey = { chord: settings.hotkey, registered: true, hooked: false, error: null }
            emit('hotkey', hotkey)
        }
        if (status.state === 'fitted') {
            status = { ...status, target: resolve(settings.profile.region) }
            emit('status', status)
        }
        return structuredClone(settings)
    },
    preview_region: (args) => resolve(args.spec as RegionSpec),
    list_monitors: () => monitors,
    restore_window: () => (status.state === 'fitted' ? toggleFit() : null),
    validate_hotkey: (args) => normalize(args.chord as string),
    begin_hotkey_capture: () => ({ ...hotkey, registered: false }),
    end_hotkey_capture: (args) => {
        const chord = args.chord as string | null
        if (chord === null) {
            return hotkey
        }
        const normalized = normalize(chord)
        if (normalized === 'Ctrl+Alt+F') {
            // Pretend Windows refuses this one, like F12: heard through the keyboard hook.
            settings.hotkey = normalized
            hotkey = { chord: normalized, registered: true, hooked: true, error: null }
        } else {
            settings.hotkey = normalized
            hotkey = { chord: normalized, registered: true, hooked: false, error: null }
        }
        if (normalized === 'F12') {
            settings.legacy_f12_ack = true
        }
        emit('hotkey', hotkey)
        emit('settings', structuredClone(settings))
        return hotkey
    },
    import_ahk_ini: () => ({
        settings: { ...structuredClone(settings), hotkey: 'F12', legacy_f12_ack: false },
        warnings: [
            "F12 is Steam's screenshot key and reserved by Windows for debuggers; consider another key.",
        ],
        summary: '2560×1440 at 1280,0, taskbar hidden, hotkey F12',
    }),
    relaunch_elevated: () => null,
    open_link: (args) => {
        console.info('open_link', args.target)
        return null
    },
    get_diagnostics: () => diagnostics(),
}

export async function invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
    const handler = commands[command]
    if (!handler) {
        throw notice('error', null, `mock: unknown command ${command}`)
    }
    return (await handler(args)) as T
}

/**
 * Tiny automation for screenshot checks in a headless browser: `?click=sel1|sel2` clicks each
 * selector in turn and `?input=sel=value` types into a field and commits it (blur).
 */
function runAutomation(): void {
    const params = new URLSearchParams(window.location.search)
    const steps: (() => void)[] = []
    for (const selector of (params.get('click') ?? '').split('|').filter(Boolean)) {
        steps.push(() => document.querySelector<HTMLElement>(selector)?.click())
    }
    const input = params.get('input')
    if (input) {
        const cut = input.lastIndexOf('=')
        const [selector, value] = [input.slice(0, cut), input.slice(cut + 1)]
        steps.push(() => {
            const field = document.querySelector<HTMLInputElement>(selector)
            if (!field) return
            field.value = value
            field.dispatchEvent(new Event('input'))
            field.dispatchEvent(new Event('blur'))
        })
    }
    steps.forEach((step, index) => setTimeout(step, 600 + index * 400))
}
runAutomation()

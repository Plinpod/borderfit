// Pure helpers for the monitor map and the size chips. Placement math (alignment, offsets,
// validation) stays in Rust's `preview_region`; only preset sizes are computed here so the chip
// row can be built per monitor.

import type { MonitorInfo, Preset, Rect } from './types'

export interface SizeChip {
    /** Stable key for v-for and the chip group. */
    key: string
    preset: Preset
    w: number
    h: number
    /** Resolution-first label, e.g. "2560×1440". */
    label: string
    /** Muted suffix: "Full" chip has none, aspect rows show their ratio, e.g. "16:9". */
    ratio: string | null
}

export interface ViewBox {
    x: number
    y: number
    w: number
    h: number
}

const MAX_CHIPS = 7

/** Candidates in display order; the first seven that fit (deduplicated by size) become chips. */
const CANDIDATES: Preset[] = [
    { kind: 'fill' },
    { kind: 'aspect', num: 16, den: 9 },
    { kind: 'fixed', w: 3440, h: 1440 },
    { kind: 'fixed', w: 2560, h: 1440 },
    { kind: 'fixed', w: 2560, h: 1080 },
    { kind: 'fixed', w: 1920, h: 1080 },
    { kind: 'aspect', num: 21, den: 9 },
    { kind: 'aspect', num: 16, den: 10 },
    { kind: 'aspect', num: 4, den: 3 },
    { kind: 'fixed', w: 1920, h: 1200 },
    { kind: 'fixed', w: 3840, h: 2160 },
]

/** Integer division rounding half up (matches region.rs). */
function roundDiv(numerator: number, denominator: number): number {
    return Math.floor((2 * numerator + denominator) / (2 * denominator))
}

/** Full monitor height at num:den, capped at the monitor width (same rule as region.rs). */
function aspectSize(mon: Rect, num: number, den: number): [number, number] {
    let h = mon.h
    let w = roundDiv(h * num, den)
    if (w > mon.w) {
        w = mon.w
        h = roundDiv(w * den, num)
    }
    return [w, h]
}

/** Width and height a preset asks for on a monitor. */
export function presetSize(mon: Rect, preset: Preset): [number, number] {
    switch (preset.kind) {
        case 'fixed':
            return [preset.w, preset.h]
        case 'aspect':
            return aspectSize(mon, preset.num, preset.den)
        case 'fill':
            return [mon.w, mon.h]
    }
}

export function presetFits(mon: Rect, preset: Preset): boolean {
    const [w, h] = presetSize(mon, preset)
    return w <= mon.w && h <= mon.h
}

/** "2560×1440" */
export function sizeLabel(size: { w: number; h: number }): string {
    return `${size.w}×${size.h}`
}

/** The size chips for one monitor (docs/ARCHITECTURE.md, "UI"). */
export function presetsFor(monitor: MonitorInfo): SizeChip[] {
    const mon = monitor.rect
    const fixed3440Fits = mon.w >= 3440 && mon.h >= 1440
    const chips: SizeChip[] = []

    for (const preset of CANDIDATES) {
        if (preset.kind === 'aspect' && preset.num === 21 && fixed3440Fits) {
            // Avoid a 3360×1440 "21:9" chip next to the real 3440×1440 one.
            continue
        }
        const [w, h] = presetSize(mon, preset)
        if (w > mon.w || h > mon.h) {
            continue
        }
        const ratio = preset.kind === 'aspect' ? `${preset.num}:${preset.den}` : null
        const existing = chips.find((c) => c.w === w && c.h === h)
        if (existing) {
            mergeInto(existing, preset, ratio)
            continue
        }
        chips.push({
            key: preset.kind === 'fill' ? 'full' : `${w}x${h}`,
            preset,
            w,
            h,
            label: preset.kind === 'fill' ? 'Full' : sizeLabel({ w, h }),
            ratio,
        })
    }
    return chips.slice(0, MAX_CHIPS)
}

/**
 * Folds a candidate into a chip of the same size. Sizes equal to Full disappear into Full; an
 * aspect row equal to a fixed size becomes one chip stored as the exact size, labelled with
 * the ratio ("2560×1440 · 16:9").
 */
function mergeInto(chip: SizeChip, candidate: Preset, ratio: string | null): void {
    if (chip.preset.kind === 'fill') {
        return
    }
    if (candidate.kind === 'fixed') {
        chip.preset = { kind: 'fixed', w: candidate.w, h: candidate.h }
    }
    if (ratio && !chip.ratio) {
        chip.ratio = ratio
    }
}

/** The chip matching a stored preset on this monitor (by resulting size), if any. */
export function chipFor(chips: SizeChip[], mon: Rect, preset: Preset): SizeChip | null {
    if (preset.kind === 'fill') {
        return chips.find((c) => c.preset.kind === 'fill') ?? null
    }
    const [w, h] = presetSize(mon, preset)
    return chips.find((c) => c.w === w && c.h === h) ?? null
}

/** Bounding box of all monitors plus a small margin, in physical pixels. */
export function viewBoxFor(monitors: MonitorInfo[]): ViewBox {
    if (monitors.length === 0) {
        return { x: 0, y: 0, w: 1920, h: 1080 }
    }
    const left = Math.min(...monitors.map((m) => m.rect.x))
    const top = Math.min(...monitors.map((m) => m.rect.y))
    const right = Math.max(...monitors.map((m) => m.rect.x + m.rect.w))
    const bottom = Math.max(...monitors.map((m) => m.rect.y + m.rect.h))
    const pad = Math.round(Math.max(right - left, bottom - top) * 0.012)
    return { x: left - pad, y: top - pad, w: right - left + 2 * pad, h: bottom - top + 2 * pad }
}

/** Monitors left to right, as the map draws them. */
export function sortedMonitors(monitors: MonitorInfo[]): MonitorInfo[] {
    return [...monitors].sort((a, b) => a.rect.x - b.rect.x || a.rect.y - b.rect.y)
}

/** n from `\\.\DISPLAYn` (0 when the device name has another shape). */
export function displayNumber(device: string): number {
    const match = /DISPLAY(\d+)$/i.exec(device)
    return match ? Number(match[1]) : 0
}

export function displayName(device: string): string {
    const n = displayNumber(device)
    return n ? `Display ${n}` : device.replace(/^\\\\\.\\/, '')
}

/** The monitor a region spec points at: its device, else the primary. */
export function monitorFor(monitors: MonitorInfo[], device: string): MonitorInfo | null {
    const primary = monitors.find((m) => m.primary) ?? monitors[0] ?? null
    if (!device) {
        return primary
    }
    return monitors.find((m) => m.device.toLowerCase() === device.toLowerCase()) ?? null
}

/** The taskbar strips of a monitor: its rect minus its work area (usually one edge). */
export function taskbarBands(monitor: MonitorInfo): Rect[] {
    const { rect: r, work: w } = monitor
    const bands: Rect[] = []
    if (w.y > r.y) bands.push({ x: r.x, y: r.y, w: r.w, h: w.y - r.y })
    if (w.y + w.h < r.y + r.h)
        bands.push({ x: r.x, y: w.y + w.h, w: r.w, h: r.y + r.h - w.y - w.h })
    if (w.x > r.x) bands.push({ x: r.x, y: r.y, w: w.x - r.x, h: r.h })
    if (w.x + w.w < r.x + r.w)
        bands.push({ x: w.x + w.w, y: r.y, w: r.x + r.w - w.x - w.w, h: r.h })
    return bands
}

/** `rect` with its origin measured from `monitor`'s top-left (what the X/Y fields show). */
export function relativeTo(rect: Rect, monitor: MonitorInfo): Rect {
    return { ...rect, x: rect.x - monitor.rect.x, y: rect.y - monitor.rect.y }
}

/** "2560×1440 at 1280, 0" */
export function describeRect(rect: Rect): string {
    return `${sizeLabel(rect)} at ${rect.x}, ${rect.y}`
}

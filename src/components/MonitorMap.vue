<script setup lang="ts">
// The monitor picker and fit preview: every display drawn to scale (physical px), its taskbar
// band hatched, and the resolved region as an accent rectangle. Click a display to pick it.
// A legend underneath lists each display's resolution (and which one is primary).
import { computed, onBeforeUnmount, onMounted, ref, useId, watch } from 'vue'
import { displayNumber, sizeLabel, sortedMonitors, taskbarBands, viewBoxFor } from '../geometry'
import type { MonitorInfo, Rect } from '../types'

const props = defineProps<{
    monitors: MonitorInfo[]
    /** Device of the selected monitor. */
    selected: string | null
    /** Absolute rect of the region, or null while unknown. */
    rect: Rect | null
    /** Draw the region in the warning colour (it extends beyond the display). */
    warn?: boolean
    hideTaskbar: boolean
    hideSecondary: boolean
}>()

const emit = defineEmits<{ select: [monitor: MonitorInfo] }>()

const MAX_HEIGHT_PX = 130
const TWEEN_MS = 150

const patternId = `bf-hatch-${useId()}`
const sorted = computed(() => sortedMonitors(props.monitors))
const box = computed(() => viewBoxFor(props.monitors))
const interactive = computed(() => props.monitors.length > 1)
const aspect = computed(() => box.value.w / box.value.h)

// Rendered width, to convert on-screen pixels into viewBox units for radii and the hatch.
const frame = ref<HTMLElement>()
const renderedWidth = ref(420)
let observer: ResizeObserver | undefined
onMounted(() => {
    observer = new ResizeObserver(
        ([entry]) => (renderedWidth.value = entry.contentRect.width || 420)
    )
    if (frame.value) observer.observe(frame.value)
})
onBeforeUnmount(() => observer?.disconnect())
const unit = computed(() => box.value.w / renderedWidth.value)

function percentX(x: number): string {
    return `${((x - box.value.x) / box.value.w) * 100}%`
}

function percentY(y: number): string {
    return `${((y - box.value.y) / box.value.h) * 100}%`
}

function bandHidden(monitor: MonitorInfo): boolean {
    return props.hideTaskbar && (monitor.primary || props.hideSecondary)
}

function caption(monitor: MonitorInfo): string {
    const size = sizeLabel(monitor.rect)
    return monitor.primary && interactive.value ? `${size} · primary` : size
}

// The region rectangle glides to its new place (150 ms) unless reduced motion is on.
const shown = ref<Rect | null>(props.rect)
let frameRequest = 0
watch(
    () => props.rect,
    (to) => {
        cancelAnimationFrame(frameRequest)
        const from = shown.value
        const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches
        if (!to || !from || still) {
            shown.value = to
            return
        }
        const start = performance.now()
        const tick = (now: number) => {
            const t = Math.min(1, (now - start) / TWEEN_MS)
            const ease = 1 - (1 - t) ** 3
            const mix = (a: number, b: number) => a + (b - a) * ease
            shown.value = {
                x: mix(from.x, to.x),
                y: mix(from.y, to.y),
                w: mix(from.w, to.w),
                h: mix(from.h, to.h),
            }
            if (t < 1) frameRequest = requestAnimationFrame(tick)
        }
        frameRequest = requestAnimationFrame(tick)
    }
)
onBeforeUnmount(() => cancelAnimationFrame(frameRequest))
</script>

<template>
    <div role="group" aria-label="Displays">
        <div
            ref="frame"
            class="relative mx-auto"
            :style="{
                width: `min(100%, ${MAX_HEIGHT_PX * aspect}px)`,
                aspectRatio: String(aspect),
            }"
        >
            <svg
                class="absolute inset-0 block size-full"
                :viewBox="`${box.x} ${box.y} ${box.w} ${box.h}`"
                aria-hidden="true"
            >
                <defs>
                    <pattern
                        :id="patternId"
                        patternUnits="userSpaceOnUse"
                        :width="5 * unit"
                        :height="5 * unit"
                        patternTransform="rotate(45)"
                    >
                        <rect :width="1.6 * unit" :height="5 * unit" class="fill-band" />
                    </pattern>
                </defs>
                <g v-for="monitor in sorted" :key="monitor.device">
                    <rect
                        :x="monitor.rect.x"
                        :y="monitor.rect.y"
                        :width="monitor.rect.w"
                        :height="monitor.rect.h"
                        :rx="4 * unit"
                        class="fill-surface-2 transition-[stroke] duration-100"
                        :class="monitor.device === selected ? 'stroke-accent' : 'stroke-line'"
                        :stroke-width="monitor.device === selected ? 1.5 : 1"
                        vector-effect="non-scaling-stroke"
                    />
                    <rect
                        v-for="(band, index) in taskbarBands(monitor)"
                        :key="index"
                        :x="band.x"
                        :y="band.y"
                        :width="band.w"
                        :height="band.h"
                        :fill="`url(#${patternId})`"
                        class="transition-opacity duration-150"
                        :opacity="bandHidden(monitor) ? 0 : 0.9"
                    />
                </g>
                <rect
                    v-if="shown"
                    :x="shown.x"
                    :y="shown.y"
                    :width="Math.max(shown.w, 0)"
                    :height="Math.max(shown.h, 0)"
                    :rx="2 * unit"
                    :class="warn ? 'fill-warn/30 stroke-warn' : 'fill-accent/30 stroke-accent'"
                    stroke-width="1"
                    vector-effect="non-scaling-stroke"
                />
            </svg>

            <!-- Click targets and number badges sit over the drawing, positioned in % of the box. -->
            <template v-if="interactive">
                <button
                    v-for="monitor in sorted"
                    :key="monitor.device"
                    type="button"
                    class="group absolute rounded-[4px]"
                    :style="{
                        left: percentX(monitor.rect.x),
                        top: percentY(monitor.rect.y),
                        width: `${(monitor.rect.w / box.w) * 100}%`,
                        height: `${(monitor.rect.h / box.h) * 100}%`,
                    }"
                    :aria-label="`${monitor.name}, ${caption(monitor)}`"
                    :aria-pressed="monitor.device === selected"
                    @click="emit('select', monitor)"
                >
                    <span
                        class="absolute top-1 left-1 flex size-4 items-center justify-center rounded-full text-[10px] leading-none font-semibold tabular-nums transition-colors duration-100"
                        :class="
                            monitor.device === selected
                                ? 'bg-accent text-accent-fg'
                                : 'bg-line text-muted group-hover:text-fg'
                        "
                        >{{ displayNumber(monitor.device) || '?' }}</span
                    >
                </button>
            </template>
        </div>

        <!-- A wrapping legend rather than labels under each display: centred labels collide
             when displays are narrow on the map or stacked vertically. -->
        <ul class="mt-1.5 flex flex-wrap justify-center gap-x-3 gap-y-0.5 text-[11px] leading-4">
            <li
                v-for="monitor in sorted"
                :key="monitor.device"
                class="flex items-center gap-1 whitespace-nowrap tabular-nums"
                :class="monitor.device === selected && interactive ? 'text-fg' : 'text-muted'"
            >
                <span
                    v-if="interactive"
                    class="flex size-3.5 items-center justify-center rounded-full text-[9px] leading-none font-semibold"
                    :class="monitor.device === selected ? 'bg-accent text-accent-fg' : 'bg-line'"
                    aria-hidden="true"
                    >{{ displayNumber(monitor.device) || '?' }}</span
                >
                {{ caption(monitor) }}
            </li>
        </ul>
    </div>
</template>

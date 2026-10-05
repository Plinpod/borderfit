<script setup lang="ts">
// The single feedback surface: what BorderFit is doing, what went wrong and the one thing to do
// about it. Priority: import confirmation > alert (needs admin, hotkey lost) > fitted > first run
// > ready. One-line notices sit underneath.
import { computed } from 'vue'
import { describeRect, displayName, monitorFor, relativeTo, sizeLabel } from '../geometry'
import { Icon } from '../icons'
import {
    acknowledgeF12,
    cancelImport,
    confirmImport,
    dismissAlert,
    dismissNotice,
    monitorMissing,
    relaunchElevated,
    requestHotkeyChange,
    restore,
    selectedMonitor,
    state,
} from '../state'
import Keycap from './Keycap.vue'

type View = 'import' | 'admin' | 'hotkey' | 'fitted' | 'first-run' | 'ready'

const view = computed<View>(() => {
    if (state.pendingImport) return 'import'
    const alert = state.alert?.error?.kind
    if (alert === 'needs_admin') return 'admin'
    if (alert === 'hotkey_in_use') return 'hotkey'
    if (state.status.state === 'fitted') return 'fitted'
    return state.settings.first_run_done ? 'ready' : 'first-run'
})

const degraded = computed(() => (view.value === 'fitted' ? state.status.degraded : null))

const dot = computed(() => {
    switch (view.value) {
        case 'admin':
        case 'hotkey':
            return 'bg-danger'
        case 'import':
            return 'bg-accent'
        case 'fitted':
            return degraded.value ? 'bg-warn' : 'bg-ok'
        default:
            return 'border-[1.5px] border-muted'
    }
})

const hotkey = computed(() => state.hotkey.chord)
const windowTitle = computed(() => state.status.window?.title ?? 'the window')

/** "2560×1440 on Display 1": where the next fit will land. */
const nextFit = computed(() => {
    const rect = state.previewError ? null : state.preview
    const monitor = selectedMonitor.value
    if (!rect || !monitor) return 'the region below'
    return `${sizeLabel(rect)} on ${displayName(monitor.device)}`
})

/** The fitted rect relative to its display, as the fields show it. */
const fittedLine = computed(() => {
    const target = state.status.target
    if (!target) return ''
    const device = state.status.monitor ?? ''
    const monitor = monitorFor(state.monitors, device)
    const relative = monitor ? relativeTo(target, monitor) : target
    const taskbar = !state.status.target_visible
        ? 'game minimized, taskbar shown'
        : state.status.taskbar_hidden
          ? 'taskbar hidden'
          : 'taskbar visible'
    return `${describeRect(relative)} · ${device ? displayName(device) : 'primary display'} · ${taskbar}`
})

const restoreLine = computed(() => {
    const restoreTo = state.status.restore
    if (!restoreTo) return ''
    const borders = restoreTo.had_borders ? 'with borders' : 'without borders'
    if (restoreTo.show === 'maximized') return `Restores maximized, ${borders}.`
    if (restoreTo.show === 'minimized') return 'Restores minimized.'
    const r = restoreTo.rect
    return `Restores to ${describeRect(r)}, ${borders}.`
})

const title = computed(() => {
    switch (view.value) {
        case 'import':
            return 'Import AutoHotkey settings?'
        case 'admin': {
            const error = state.alert?.error
            return `Couldn't fit "${error?.kind === 'needs_admin' ? error.title : 'the window'}"`
        }
        case 'hotkey':
            return `${hotkey.value} is already used by another app`
        case 'fitted':
            return degraded.value?.kind === 'resize_refused'
                ? `Fitted · ${windowTitle.value} · size not accepted`
                : `Fitted · ${windowTitle.value}`
        default:
            return 'Ready'
    }
})

const showF12Nag = computed(
    () =>
        state.settings.hotkey === 'F12' && !state.settings.legacy_f12_ack && view.value !== 'import'
)

const missingDisplayLine = computed(() => {
    if (!monitorMissing.value) return ''
    const primary = monitorFor(state.monitors, '')
    const fallback = primary ? displayName(primary.device) : 'the primary display'
    return `${displayName(state.settings.profile.region.monitor)} not found, using ${fallback}.`
})
</script>

<template>
    <section
        class="rounded-card border bg-surface p-3.5"
        :class="view === 'admin' || view === 'hotkey' ? 'border-danger/50' : 'border-line'"
        aria-live="polite"
    >
        <div class="flex items-start gap-2.5">
            <span class="mt-[6px] size-2 shrink-0 rounded-full" :class="dot" aria-hidden="true" />
            <div class="min-w-0 flex-1">
                <div class="flex items-start justify-between gap-3">
                    <h1 class="min-w-0 text-[15px] leading-snug font-semibold break-words">
                        {{ title }}
                    </h1>
                    <button v-if="view === 'fitted'" type="button" class="btn" @click="restore">
                        Restore
                    </button>
                    <button
                        v-else-if="view === 'admin' || view === 'hotkey'"
                        type="button"
                        class="-mt-0.5 -mr-1 flex size-6 items-center justify-center rounded-control text-muted hover:bg-surface-2 hover:text-fg"
                        aria-label="Dismiss"
                        @click="dismissAlert"
                    >
                        <Icon name="x" :size="14" />
                    </button>
                </div>

                <!-- Body per view -->
                <div class="mt-1 text-muted">
                    <template v-if="view === 'import' && state.pendingImport">
                        <p>Import {{ state.pendingImport.summary }}?</p>
                        <p
                            v-for="warning in state.pendingImport.warnings"
                            :key="warning"
                            class="mt-1 text-xs text-warn"
                        >
                            {{ warning }}
                        </p>
                        <div class="mt-2.5 flex gap-2">
                            <button type="button" class="btn btn-primary" @click="confirmImport">
                                Import
                            </button>
                            <button type="button" class="btn" @click="cancelImport">Cancel</button>
                        </div>
                    </template>

                    <template v-else-if="view === 'admin'">
                        <p>It runs as administrator, so Windows blocks BorderFit from moving it.</p>
                        <div class="mt-2.5 flex justify-end">
                            <button type="button" class="btn btn-primary" @click="relaunchElevated">
                                <Icon name="shield" :size="13" />
                                Restart BorderFit as admin
                            </button>
                        </div>
                    </template>

                    <template v-else-if="view === 'hotkey'">
                        <p>BorderFit can't react to it. Choose another key.</p>
                        <div class="mt-2.5 flex justify-end">
                            <button
                                type="button"
                                class="btn btn-primary"
                                @click="requestHotkeyChange"
                            >
                                Change hotkey
                            </button>
                        </div>
                    </template>

                    <template v-else-if="view === 'fitted'">
                        <template v-if="degraded?.kind === 'resize_refused'">
                            <p>
                                Asked {{ sizeLabel(degraded.wanted) }}, got
                                {{ sizeLabel(degraded.got) }}. The game enforces its own size: set
                                it to Windowed {{ sizeLabel(degraded.wanted) }} in its settings,
                                then press <Keycap :chord="hotkey" /> twice.
                            </p>
                        </template>
                        <template v-else>
                            <p class="tabular-nums">{{ fittedLine }}</p>
                            <p class="tabular-nums">{{ restoreLine }}</p>
                            <p class="mt-1">
                                Press <Keycap :chord="hotkey" />, click Restore, or close the game.
                            </p>
                        </template>
                        <p v-if="degraded?.kind === 'dpi_mismatch'" class="mt-1 text-warn">
                            This game isn't DPI-aware; on a scaled display its size may be off.
                        </p>
                        <p v-if="degraded?.kind === 'style_refused'" class="mt-1 text-warn">
                            The window kept its borders; the game draws its own frame.
                        </p>
                    </template>

                    <template v-else-if="view === 'first-run'">
                        <p>
                            Press <Keycap :chord="hotkey" /> in a game to fit it to {{ nextFit }}.
                            Press again to restore.
                        </p>
                        <p class="mt-1">
                            Closing this window keeps BorderFit in the tray; turn on Launch at
                            startup to keep it ready.
                        </p>
                    </template>

                    <template v-else>
                        <p>
                            Press <Keycap :chord="hotkey" /> in a game to fit it to {{ nextFit }}.
                            Press again to restore.
                        </p>
                    </template>

                    <p
                        v-if="state.focusHint && view !== 'fitted'"
                        class="mt-1 font-semibold text-accent"
                    >
                        Focus the game first, then press <Keycap :chord="hotkey" />.
                    </p>
                </div>
            </div>
        </div>

        <!-- One-line notices -->
        <ul
            v-if="state.notices.length || showF12Nag || missingDisplayLine"
            class="mt-3 flex flex-col gap-1.5 border-t border-line pt-2.5 text-xs"
        >
            <li v-if="missingDisplayLine" class="text-muted">{{ missingDisplayLine }}</li>
            <li v-if="showF12Nag" class="flex items-start gap-2 text-warn">
                <Icon name="triangle-alert" :size="13" class="mt-px" />
                <span class="flex-1">
                    F12 is Steam's screenshot key and reserved by Windows for debuggers.
                </span>
                <button type="button" class="link font-semibold" @click="requestHotkeyChange">
                    Change
                </button>
                <button type="button" class="link" @click="acknowledgeF12">Keep F12</button>
            </li>
            <li
                v-for="notice in state.notices"
                :key="notice.id"
                class="flex items-start gap-2"
                :class="{
                    'text-muted': notice.level === 'info',
                    'text-warn': notice.level === 'warn',
                    'text-danger': notice.level === 'error',
                }"
            >
                <Icon
                    v-if="notice.level !== 'info'"
                    name="triangle-alert"
                    :size="13"
                    class="mt-px"
                />
                <span class="flex-1">{{ notice.message }}</span>
                <button
                    type="button"
                    class="-my-0.5 flex size-5 items-center justify-center rounded text-muted hover:text-fg"
                    aria-label="Dismiss"
                    @click="dismissNotice(notice.id)"
                >
                    <Icon name="x" :size="12" />
                </button>
            </li>
        </ul>
    </section>
</template>

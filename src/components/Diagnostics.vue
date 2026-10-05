<script setup lang="ts">
// Collapsed diagnostics: what BorderFit sees (DPI mode, elevation, taskbar windows, marker,
// hotkey), the last log lines, and the three failure modes users hit most. Loads on open.
import { computed, ref } from 'vue'
import { api, toNotice } from '../api'
import { Icon } from '../icons'
import { state } from '../state'
import type { Diagnostics } from '../types'

const HELP_LINES = [
    'Hotkey not firing? If Win+D also does nothing in this game, the game blocks app hotkeys; use the tray’s Fit active window.',
    'BorderFit cannot help games that only offer exclusive fullscreen; switch them to windowed or borderless first.',
    'If the taskbar ever stays hidden, restart Explorer from Task Manager, then start BorderFit once.',
]

const info = ref<Diagnostics | null>(null)
const error = ref('')
const copied = ref(false)

async function load(): Promise<void> {
    try {
        info.value = await api.getDiagnostics()
        error.value = ''
    } catch (e) {
        error.value = toNotice(e).message
    }
}

function onToggle(event: Event): void {
    if ((event.target as HTMLDetailsElement).open) {
        void load()
    }
}

function hex(value: number): string {
    return `0x${value.toString(16)}`
}

/** Key/value rows shown in the table and copied as plain text. */
const rows = computed<[string, string][]>(() => {
    const d = info.value
    if (!d) return []
    const trays = d.tray_windows.length
        ? d.tray_windows
              .map((t) => `${t.class} · ${t.exe} · ${hex(t.hwnd)}${t.visible ? '' : ' (hidden)'}`)
              .join('\n')
        : 'none found'
    return [
        ['Version', d.version],
        ['Windows build', d.os_build],
        ['DPI awareness', d.dpi_context],
        ['Elevated', d.elevated ? 'yes' : 'no'],
        ['Taskbar windows', trays],
        ['Restore marker', d.marker_present ? 'present' : 'none'],
        ['Hotkey', `${d.hotkey.chord} (${d.hotkey.registered ? 'registered' : 'not registered'})`],
        [
            'Displays',
            d.monitors
                .map(
                    (m) =>
                        `${m.name} ${m.rect.w}×${m.rect.h} at ${m.rect.x},${m.rect.y} ${m.dpi} dpi`
                )
                .join('\n'),
        ],
    ]
})

async function copy(): Promise<void> {
    if (!info.value) await load()
    const text = [
        'BorderFit diagnostics',
        ...rows.value.map(([key, value]) => `${key}: ${value.replace(/\n/g, '; ')}`),
        '',
        'Log:',
        ...(info.value?.log_tail ?? []),
    ].join('\n')
    try {
        await navigator.clipboard.writeText(text)
        copied.value = true
        setTimeout(() => (copied.value = false), 2000)
    } catch (e) {
        error.value = `Could not copy: ${toNotice(e).message}`
    }
}

function openLogs(): void {
    api.openLink('logs').catch((e) => (error.value = toNotice(e).message))
}
</script>

<template>
    <details class="group" @toggle="onToggle">
        <summary
            class="flex h-7 list-none items-center gap-1.5 text-muted hover:text-fg [&::-webkit-details-marker]:hidden"
        >
            <span class="inline-block transition-transform duration-100 group-open:rotate-90"
                >▸</span
            >
            Diagnostics
        </summary>

        <div
            class="mt-1 flex flex-col gap-3 rounded-card border border-line bg-surface p-3 text-xs"
        >
            <p v-if="error" class="text-danger">{{ error }}</p>
            <dl v-if="info" class="grid grid-cols-[104px_1fr] gap-x-3 gap-y-1">
                <template v-for="[key, value] in rows" :key="key">
                    <dt class="text-muted">{{ key }}</dt>
                    <dd class="break-words whitespace-pre-line tabular-nums select-text">
                        {{ value }}
                    </dd>
                </template>
            </dl>
            <p v-else-if="!error" class="text-muted">Loading…</p>

            <div v-if="info">
                <div class="section-label mb-1">Log</div>
                <pre
                    class="max-h-32 overflow-auto rounded-control bg-surface-2 p-2 font-mono text-[11px] leading-snug whitespace-pre-wrap text-muted select-text"
                    >{{ info.log_tail.join('\n') || 'No log lines yet.' }}</pre>
            </div>

            <div class="flex gap-2">
                <button type="button" class="btn" @click="openLogs">Open logs folder</button>
                <button type="button" class="btn" @click="copy">
                    {{ copied ? 'Copied' : 'Copy diagnostics' }}
                </button>
            </div>
            <p class="text-muted">
                Logs stay on this PC. They include window titles and file paths, so check them
                before posting.
            </p>

            <ul class="flex flex-col gap-1.5 text-muted">
                <li v-for="line in HELP_LINES" :key="line" class="flex gap-1.5">
                    <Icon name="triangle-alert" :size="12" class="mt-0.5 text-muted" />
                    <span>{{ line }}</span>
                </li>
            </ul>
            <p v-if="state.isElevated" class="text-muted">Running as administrator.</p>
        </div>
    </details>
</template>

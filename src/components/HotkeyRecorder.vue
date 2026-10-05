<script setup lang="ts">
// The hotkey row. Click (or Enter) to record: Rust unregisters the live chord so the webview
// sees the keys, the first non-modifier key completes the chord, Rust validates, registers and
// persists it. Esc or leaving the field cancels and puts the old chord back. A chord Windows
// refuses (F12) works through Rust's keyboard hook and gets a small warning line.
import { computed, nextTick, ref, watch } from 'vue'
import { api, toNotice } from '../api'
import { Icon } from '../icons'
import { state } from '../state'
import { DEFAULT_HOTKEY, type HotkeyStatus } from '../types'
import Keycap from './Keycap.vue'

const MODIFIER_CODES = /^(Control|Alt|Shift|Meta|OS)(Left|Right)$/

const recording = ref(false)
// True while a pressed chord is being validated: a second key or a blur must not start another
// `finish`, or two end-captures would race in Rust.
let validating = false
const partial = ref('')
const message = ref<{ level: 'warn' | 'error'; text: string } | null>(null)
const button = ref<HTMLButtonElement>()

const chord = computed(() => state.hotkey.chord)
// Windows refused the chord, so Rust hears it through a keyboard hook instead.
const hooked = computed(() => state.hotkey.hooked && !recording.value)
/** A safer pick for the hooked chord: F12 has Ctrl+F12, which Windows lets apps register. */
const saferPick = computed(() => (chord.value.endsWith('F12') ? 'Ctrl+F12' : 'another key'))

/** Modifier names held during a key event, in canonical order. */
function modifiers(event: KeyboardEvent): string[] {
    const held: string[] = []
    if (event.ctrlKey) held.push('Ctrl')
    if (event.altKey) held.push('Alt')
    if (event.shiftKey) held.push('Shift')
    if (event.metaKey) held.push('Super')
    return held
}

async function start(): Promise<void> {
    if (recording.value) return
    message.value = null
    try {
        await api.beginHotkeyCapture()
    } catch (error) {
        message.value = { level: 'error', text: toNotice(error).message }
        return
    }
    recording.value = true
    partial.value = ''
    await nextTick()
    button.value?.focus()
}

/** Ends recording with a new chord, or `null` to cancel and restore the old one. */
async function finish(next: string | null): Promise<void> {
    recording.value = false
    partial.value = ''
    try {
        applyStatus(await api.endHotkeyCapture(next))
    } catch (error) {
        message.value = { level: 'error', text: toNotice(error).message }
    }
}

/** Explains a failed change inline; `state` itself follows Rust's `hotkey` event. */
function applyStatus(status: HotkeyStatus): void {
    if (status.error?.kind === 'hotkey_in_use' && status.error.chord !== status.chord) {
        message.value = {
            level: 'warn',
            text: `${status.error.chord} is used by another app. ${status.chord} stays active.`,
        }
    } else if (status.error?.kind === 'hotkey_invalid') {
        message.value = { level: 'warn', text: status.error.reason }
    } else if (status.error?.kind === 'win32') {
        message.value = {
            level: 'error',
            text: `Windows refused the hotkey (error ${status.error.code}).`,
        }
    }
}

async function onKeydown(event: KeyboardEvent): Promise<void> {
    if (!recording.value) return
    event.preventDefault()
    event.stopPropagation()
    if (validating || event.repeat) return
    const held = modifiers(event)
    if (event.code === 'Escape' && held.length === 0) {
        await finish(null)
        return
    }
    if (MODIFIER_CODES.test(event.code)) {
        partial.value = held.join(' + ')
        return
    }
    let normalized: string
    validating = true
    try {
        normalized = await api.validateHotkey([...held, event.code].join('+'))
    } catch (error) {
        message.value = { level: 'warn', text: toNotice(error).message }
        partial.value = ''
        // Stay in recording mode so another combination can be tried right away, unless focus
        // left while validating (that blur was ignored above).
        if (document.activeElement !== button.value) {
            await finish(null)
        }
        return
    } finally {
        validating = false
    }
    message.value = null
    await finish(normalized)
}

function onKeyup(event: KeyboardEvent): void {
    if (recording.value) {
        partial.value = modifiers(event).join(' + ')
    }
}

function onBlur(): void {
    if (recording.value && !validating) {
        void finish(null)
    }
}

/**
 * Back to the default chord through the same begin/end capture path as recording one, so the
 * register, persist and fall-back-to-the-old-chord rules live in one Rust command.
 */
async function reset(): Promise<void> {
    message.value = null
    try {
        await api.beginHotkeyCapture()
    } catch (error) {
        message.value = { level: 'error', text: toNotice(error).message }
        return
    }
    await finish(DEFAULT_HOTKEY)
}

// "Change hotkey" in the status card asks the recorder to start.
watch(
    () => state.recordRequest,
    () => void start()
)
</script>

<template>
    <div class="py-0.5">
        <div class="flex min-h-8 items-center justify-between gap-4">
            <span>Hotkey</span>
            <div class="flex items-center gap-1">
                <button
                    ref="button"
                    type="button"
                    class="flex h-7 min-w-[88px] items-center justify-center rounded-control border px-2 transition-colors duration-100"
                    :class="
                        recording
                            ? 'recording border-accent bg-accent/8 text-accent'
                            : 'border-line bg-surface hover:border-muted'
                    "
                    :aria-label="
                        recording ? 'Recording hotkey' : `Hotkey ${chord}, press to change`
                    "
                    @click="start"
                    @keydown="onKeydown"
                    @keyup="onKeyup"
                    @blur="onBlur"
                >
                    <span v-if="recording" class="text-xs font-semibold whitespace-nowrap">
                        {{ partial ? `${partial} + …` : 'Press keys… Esc to cancel' }}
                    </span>
                    <Keycap v-else :chord="chord" />
                </button>
                <button
                    v-if="chord !== DEFAULT_HOTKEY && !recording"
                    type="button"
                    class="flex size-7 items-center justify-center rounded-control text-muted hover:bg-surface-2 hover:text-fg"
                    :title="`Reset to ${DEFAULT_HOTKEY}`"
                    :aria-label="`Reset hotkey to ${DEFAULT_HOTKEY}`"
                    @click="reset"
                >
                    <Icon name="rotate-ccw" :size="14" />
                </button>
            </div>
        </div>
        <p
            v-if="message"
            class="text-xs"
            :class="message.level === 'error' ? 'text-danger' : 'text-warn'"
            role="status"
        >
            {{ message.text }}
        </p>
        <p v-else-if="hooked" class="text-xs text-warn">
            {{ chord }} may conflict with other programs. If it misbehaves, try {{ saferPick }}.
        </p>
    </div>
</template>

<style scoped>
.recording {
    animation: pulse 1.2s ease-in-out infinite;
}

@keyframes pulse {
    50% {
        box-shadow: 0 0 0 3px color-mix(in oklab, var(--bf-accent) 25%, transparent);
    }
}
</style>

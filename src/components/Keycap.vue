<script setup lang="ts">
// A hotkey drawn as keycaps, e.g. [Ctrl] + [Alt] + [B]. Used inline in status copy so the
// instructions always show the live chord.
import { computed } from 'vue'

const props = defineProps<{ chord: string }>()

/** Short, familiar labels for canonical key names. */
const KEY_LABELS: Record<string, string> = {
    Super: 'Win',
    ArrowUp: '↑',
    ArrowDown: '↓',
    ArrowLeft: '←',
    ArrowRight: '→',
    PageUp: 'PgUp',
    PageDown: 'PgDn',
    Escape: 'Esc',
    Delete: 'Del',
    Insert: 'Ins',
    PrintScreen: 'PrtSc',
    ScrollLock: 'ScrLk',
    Backquote: '`',
    Minus: '-',
    Equal: '=',
    BracketLeft: '[',
    BracketRight: ']',
    Backslash: '\\',
    Semicolon: ';',
    Quote: "'",
    Comma: ',',
    Period: '.',
    Slash: '/',
    NumpadAdd: 'Num +',
    NumpadSubtract: 'Num -',
    NumpadMultiply: 'Num *',
    NumpadDivide: 'Num /',
    NumpadDecimal: 'Num .',
}

function label(part: string): string {
    return KEY_LABELS[part] ?? part.replace(/^Numpad(\d)$/, 'Num $1')
}

const parts = computed(() => props.chord.split('+').filter(Boolean).map(label))
</script>

<template>
    <span class="inline-flex items-center gap-0.5 align-middle" :aria-label="chord">
        <template v-for="(part, index) in parts" :key="index">
            <span v-if="index > 0" class="text-[11px] text-muted" aria-hidden="true">+</span>
            <kbd
                class="inline-flex h-5 min-w-5 items-center justify-center rounded border border-b-2 border-line bg-surface-2 px-1.5 font-sans text-xs leading-none font-semibold text-fg"
                aria-hidden="true"
                >{{ part }}</kbd
            >
        </template>
    </span>
</template>

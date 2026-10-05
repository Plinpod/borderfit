<script setup lang="ts">
// An integer field with an inline label. Commits on blur or Enter; arrows step ±1 (Shift ±10).
// Empty or out-of-range input reverts to the last value instead of being clamped.
import { ref, watch } from 'vue'

const props = defineProps<{
    label: string
    modelValue: number | null
    min: number
    max: number
}>()

const emit = defineEmits<{ 'update:modelValue': [value: number] }>()

const draft = ref(format(props.modelValue))

watch(
    () => props.modelValue,
    (value) => (draft.value = format(value))
)

function format(value: number | null): string {
    return value === null ? '' : String(value)
}

function commit(): void {
    const value = Number(draft.value.trim())
    const valid =
        draft.value.trim() !== '' &&
        Number.isInteger(value) &&
        value >= props.min &&
        value <= props.max
    if (!valid) {
        draft.value = format(props.modelValue)
        return
    }
    if (value !== props.modelValue) {
        emit('update:modelValue', value)
    }
}

function step(event: KeyboardEvent, direction: 1 | -1): void {
    event.preventDefault()
    const base = Number(draft.value) || props.modelValue || 0
    const next = Math.min(
        props.max,
        Math.max(props.min, base + direction * (event.shiftKey ? 10 : 1))
    )
    draft.value = String(next)
    commit()
}
</script>

<template>
    <label
        class="flex h-7 min-w-0 flex-1 items-center gap-1.5 rounded-control border border-line bg-surface px-2 focus-within:border-accent"
    >
        <span class="text-xs font-semibold text-muted">{{ label }}</span>
        <input
            v-model="draft"
            class="w-full min-w-0 bg-transparent text-right tabular-nums outline-none"
            inputmode="numeric"
            spellcheck="false"
            :aria-label="label"
            :disabled="modelValue === null"
            @blur="commit"
            @keydown.enter.prevent="commit"
            @keydown.up="step($event, 1)"
            @keydown.down="step($event, -1)"
        />
    </label>
</template>

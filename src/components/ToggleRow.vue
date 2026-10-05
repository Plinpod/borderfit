<script setup lang="ts">
// A settings row: label (+ optional hint) on the left, a switch on the right.
const props = defineProps<{
    label: string
    hint?: string
    disabled?: boolean
}>()

const model = defineModel<boolean>({ required: true })

function toggle(): void {
    if (!props.disabled) {
        model.value = !model.value
    }
}
</script>

<template>
    <div class="flex min-h-8 items-center justify-between gap-4 py-0.5">
        <div class="min-w-0" :class="{ 'opacity-60': disabled }">
            <div>{{ label }}</div>
            <div v-if="hint" class="text-xs text-muted">{{ hint }}</div>
        </div>
        <button
            type="button"
            role="switch"
            :aria-checked="model"
            :aria-label="label"
            :disabled="disabled"
            class="relative h-[18px] w-8 shrink-0 rounded-full border transition-colors duration-[120ms] disabled:opacity-50"
            :class="model ? 'border-accent bg-accent' : 'border-muted/60 bg-surface-2'"
            @click="toggle"
        >
            <span
                class="absolute top-1/2 size-3 -translate-y-1/2 rounded-full transition-[left,background-color] duration-[120ms]"
                :class="model ? 'left-[16px] bg-accent-fg' : 'left-[2px] bg-muted'"
            />
        </button>
    </div>
</template>

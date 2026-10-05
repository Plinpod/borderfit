<script setup lang="ts">
// A single-choice group drawn as wrapping chips or as a joined segmented control. `null`
// selects nothing (e.g. while the fields were hand-edited). Arrow keys move between options.
export interface Choice {
    value: string
    label: string
    /** Muted suffix, e.g. the aspect ratio of a size chip. */
    sub?: string | null
}

const props = withDefaults(
    defineProps<{
        options: Choice[]
        modelValue: string | null
        variant?: 'chips' | 'segmented'
        label: string
    }>(),
    { variant: 'chips' }
)

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

/** The option buttons by index (a function ref keeps the order, unlike a `v-for` ref array). */
const buttons: (HTMLButtonElement | null)[] = []

function select(value: string): void {
    if (value !== props.modelValue) {
        emit('update:modelValue', value)
    }
}

/** Roving focus: arrows move to the previous/next option and select it. */
function onKeydown(event: KeyboardEvent, index: number): void {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key]
    if (!step) return
    event.preventDefault()
    const next = (index + step + props.options.length) % props.options.length
    buttons[next]?.focus()
    select(props.options[next].value)
}

/** The selected option is the tab stop; with nothing selected, the first one is. */
function tabIndex(value: string, index: number): number {
    const hasSelection = props.options.some((o) => o.value === props.modelValue)
    return value === props.modelValue || (!hasSelection && index === 0) ? 0 : -1
}
</script>

<template>
    <div
        role="radiogroup"
        :aria-label="label"
        :class="
            variant === 'chips'
                ? 'flex flex-wrap gap-1.5'
                : 'inline-flex overflow-hidden rounded-control border border-line bg-surface'
        "
    >
        <button
            v-for="(option, index) in options"
            :key="option.value"
            :ref="(el) => (buttons[index] = el as HTMLButtonElement | null)"
            type="button"
            role="radio"
            :aria-checked="option.value === modelValue"
            :tabindex="tabIndex(option.value, index)"
            :class="[
                variant === 'chips'
                    ? 'h-7 rounded-full border px-2.5 tabular-nums transition-colors duration-100'
                    : 'h-7 min-w-[64px] px-3 transition-colors duration-100 not-first:border-l not-first:border-line',
                option.value === modelValue
                    ? variant === 'chips'
                        ? 'border-accent bg-accent/12 font-semibold text-accent'
                        : 'bg-accent/12 font-semibold text-accent'
                    : variant === 'chips'
                      ? 'border-line bg-surface text-fg hover:border-muted'
                      : 'text-fg hover:bg-surface-2',
            ]"
            @click="select(option.value)"
            @keydown="onKeydown($event, index)"
        >
            {{ option.label
            }}<span
                v-if="option.sub"
                class="ml-1 font-normal"
                :class="option.value === modelValue ? 'text-accent/80' : 'text-muted'"
                >{{ option.sub }}</span
            >
        </button>
        <!-- Trailing content that flows with the options, e.g. the "custom" tag. -->
        <slot />
    </div>
</template>

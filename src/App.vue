<script setup lang="ts">
// The one screen: status card, where the window goes, hotkey and toggles, diagnostics, footer.
// No navigation and no Save button: every change is saved as it is made.
import { computed } from 'vue'
import { api } from './api'
import Diagnostics from './components/Diagnostics.vue'
import ChoiceGroup, { type Choice } from './components/ChoiceGroup.vue'
import HotkeyRecorder from './components/HotkeyRecorder.vue'
import MonitorMap from './components/MonitorMap.vue'
import NumberField from './components/NumberField.vue'
import StatusCard from './components/StatusCard.vue'
import ToggleRow from './components/ToggleRow.vue'
import { displayName } from './geometry'
import { Icon } from './icons'
import {
    chips,
    region,
    relativePreview,
    reportError,
    selectAlign,
    selectChip,
    selectedChip,
    selectedMonitor,
    selectMonitor,
    setField,
    state,
    updateSettings,
} from './state'
import type { HAlign, LinkTarget, Rect } from './types'

const MAX_COORD = 16384
const MIN_SIZE = 160

const chipChoices = computed<Choice[]>(() =>
    chips.value.map((chip) => ({ value: chip.key, label: chip.label, sub: chip.ratio }))
)

const ALIGN_CHOICES: Choice[] = [
    { value: 'left', label: 'Left' },
    { value: 'center', label: 'Center' },
    { value: 'right', label: 'Right' },
]

const custom = computed(() => region.value.override_rect !== null)
const alignValue = computed(() => (custom.value ? null : region.value.halign))

function onChip(key: string): void {
    const chip = chips.value.find((c) => c.key === key)
    if (chip) selectChip(chip)
}

const monitorLabel = computed(() =>
    selectedMonitor.value ? displayName(selectedMonitor.value.device) : 'the display'
)

const offscreen = computed(() => state.previewError?.kind === 'region_offscreen')

const fields: { key: keyof Rect; label: string; min: number }[] = [
    { key: 'x', label: 'X', min: -MAX_COORD },
    { key: 'y', label: 'Y', min: -MAX_COORD },
    { key: 'w', label: 'W', min: MIN_SIZE },
    { key: 'h', label: 'H', min: MIN_SIZE },
]

const taskbar = computed(() => state.settings.profile.taskbar)

function open(target: LinkTarget): void {
    api.openLink(target).catch(reportError)
}
</script>

<template>
    <main
        v-if="state.ready"
        class="mx-auto flex min-h-full max-w-[460px] flex-col gap-3 px-5 pt-3 pb-2.5"
    >
        <StatusCard />

        <section class="flex flex-col gap-2" aria-labelledby="fit-heading">
            <h2 id="fit-heading" class="section-label">Fit window to</h2>
            <MonitorMap
                :monitors="state.monitors"
                :selected="selectedMonitor?.device ?? null"
                :rect="state.preview"
                :warn="offscreen"
                :hide-taskbar="taskbar.hide_taskbar"
                :hide-secondary="taskbar.hide_secondary_taskbars"
                @select="selectMonitor"
            />

            <div class="grid grid-cols-[44px_1fr] items-start gap-x-3 gap-y-2">
                <span class="pt-1 text-muted">Size</span>
                <ChoiceGroup
                    label="Size"
                    :options="chipChoices"
                    :model-value="selectedChip?.key ?? null"
                    @update:model-value="onChip"
                >
                    <span v-if="custom" class="self-center px-1 text-xs text-muted italic">
                        custom
                    </span>
                </ChoiceGroup>

                <span class="pt-1 text-muted">Align</span>
                <ChoiceGroup
                    label="Horizontal alignment"
                    variant="segmented"
                    :options="ALIGN_CHOICES"
                    :model-value="alignValue"
                    class="justify-self-start"
                    @update:model-value="(value) => selectAlign(value as HAlign)"
                />

                <span />
                <div class="flex flex-col gap-1">
                    <div class="flex gap-1.5">
                        <NumberField
                            v-for="field in fields"
                            :key="field.key"
                            :label="field.label"
                            :model-value="relativePreview ? relativePreview[field.key] : null"
                            :min="field.min"
                            :max="MAX_COORD"
                            @update:model-value="(value) => setField(field.key, value)"
                        />
                    </div>
                    <p v-if="offscreen" class="flex items-center gap-1 text-xs text-warn">
                        <Icon name="triangle-alert" :size="12" />
                        Extends beyond {{ monitorLabel }}
                    </p>
                    <p v-else class="text-xs text-muted">
                        Pixels from the top-left of {{ monitorLabel }}
                    </p>
                </div>
            </div>
        </section>

        <section class="flex flex-col border-t border-line pt-1.5" aria-label="Settings">
            <HotkeyRecorder />
            <ToggleRow
                label="Hide taskbar while fitted"
                :model-value="taskbar.hide_taskbar"
                @update:model-value="
                    (on) => updateSettings((s) => (s.profile.taskbar.hide_taskbar = on))
                "
            />
            <ToggleRow
                label="Close to tray"
                :model-value="state.settings.close_to_tray"
                @update:model-value="(on) => updateSettings((s) => (s.close_to_tray = on))"
            />
            <ToggleRow
                label="Launch at startup"
                :hint="
                    state.isElevated
                        ? 'Not available while running as administrator'
                        : 'Starts hidden in the tray'
                "
                :disabled="state.isElevated"
                :model-value="state.settings.launch_at_startup"
                @update:model-value="(on) => updateSettings((s) => (s.launch_at_startup = on))"
            />
            <Diagnostics />
        </section>

        <footer class="mt-auto flex items-center justify-between gap-3 text-xs text-muted">
            <span class="flex items-center gap-2">
                <span class="tabular-nums">v{{ state.version }}</span>
                <span
                    v-if="state.isElevated"
                    class="inline-flex items-center gap-1 rounded-full border border-line px-1.5 py-px"
                >
                    <Icon name="shield" :size="11" />
                    Running as administrator
                </span>
            </span>
            <span class="flex items-center gap-1.5">
                <button
                    type="button"
                    class="link inline-flex items-center gap-0.5"
                    @click="open('repo')"
                >
                    GitHub <Icon name="external-link" :size="11" />
                </button>
                <span aria-hidden="true">·</span>
                <button
                    type="button"
                    class="link inline-flex items-center gap-0.5"
                    @click="open('issues')"
                >
                    Report <Icon name="external-link" :size="11" />
                </button>
                <span aria-hidden="true">·</span>
                <span>MIT</span>
            </span>
        </footer>
    </main>

    <main v-else class="flex min-h-full items-center justify-center p-5 text-muted">
        <p v-if="state.loadError" class="text-danger">
            BorderFit could not start: {{ state.loadError }}
        </p>
        <p v-else>Loading…</p>
    </main>
</template>

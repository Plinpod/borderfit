// Five vendored Lucide icons (ISC license, lucide.dev), drawn as 24x24 strokes. No icon package.

import { h, type FunctionalComponent } from 'vue'

const ICONS = {
    'rotate-ccw': ['M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8', 'M3 3v5h5'],
    'external-link': [
        'M15 3h6v6',
        'M10 14 21 3',
        'M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6',
    ],
    x: ['M18 6 6 18', 'm6 6 12 12'],
    shield: [
        'M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z',
    ],
    'triangle-alert': [
        'm21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3',
        'M12 9v4',
        'M12 17h.01',
    ],
} as const

export type IconName = keyof typeof ICONS

interface IconProps {
    name: IconName
    /** Rendered size in px. */
    size?: number
}

/** `<Icon name="x" :size="14" />`: an inline, decorative SVG that inherits `currentColor`. */
export const Icon: FunctionalComponent<IconProps> = (props) =>
    h(
        'svg',
        {
            xmlns: 'http://www.w3.org/2000/svg',
            viewBox: '0 0 24 24',
            width: props.size ?? 14,
            height: props.size ?? 14,
            fill: 'none',
            stroke: 'currentColor',
            'stroke-width': 2,
            'stroke-linecap': 'round',
            'stroke-linejoin': 'round',
            'aria-hidden': 'true',
            class: 'shrink-0',
        },
        ICONS[props.name].map((d) => h('path', { d }))
    )

Icon.props = ['name', 'size']

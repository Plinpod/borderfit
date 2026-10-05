import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'

// Set by `tauri dev` when the webview runs on another host (e.g. a phone); unused on desktop.
const host = process.env.TAURI_DEV_HOST

// https://vite.dev/config/
export default defineConfig({
    plugins: [vue(), tailwindcss()],
    // Keep Rust errors visible in the `tauri dev` terminal.
    clearScreen: false,
    envPrefix: ['VITE_', 'TAURI_ENV_*'],
    server: {
        // Tauri expects a fixed port; fail instead of picking another one.
        port: 1420,
        strictPort: true,
        host: host || false,
        hmr: host ? { protocol: 'ws', host, port: 1421 } : undefined,
        watch: { ignored: ['**/src-tauri/**'] },
    },
    build: {
        target: 'es2022',
    },
})

import { createApp } from 'vue'
import App from './App.vue'
import './assets/main.css'
import { init } from './state'

createApp(App).mount('#app')
void init()

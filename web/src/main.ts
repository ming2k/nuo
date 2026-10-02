import { mount } from 'svelte'
import { resolveInitialTheme, applyTheme } from './lib/theme.js'
import { initLocale } from './lib/i18n.svelte.js'
import 'highlight.js/styles/github-dark.css'
import './app.css'
import App from './App.svelte'

// Resolve the paper and the tongue before first paint — no flash of the
// wrong theme, no English-first flash for Chinese readers.
applyTheme(resolveInitialTheme())
initLocale()

const app = mount(App, {
  target: document.getElementById('app')!,
})

export default app

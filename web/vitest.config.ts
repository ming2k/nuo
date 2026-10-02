import { defineConfig, mergeConfig } from 'vitest/config'
import viteConfig from './vite.config.ts'

// test runner config, layered over the vite config so the svelte plugin and
// the __MUTA_CLIENT_VERSION__ define apply to tests exactly as to builds.
export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      // happy-dom gives the store a window/localStorage-shaped DOM; tests stub
      // WebSocket itself with a scripted fake. markdown.test.ts opts into jsdom
      // per-file (DOMPurify needs real DOM semantics).
      environment: 'happy-dom',
      include: ['src/**/*.test.ts'],
    },
  }),
)

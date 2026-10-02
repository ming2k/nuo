import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

// The client version IS the package version: the daemon's ADR-0100 handshake
// refuses any client whose `Select.version` does not exactly equal its own
// workspace `CARGO_PKG_VERSION`, so this value must never drift from the Rust
// workspace version (the CI web job asserts the match).
const { version } = JSON.parse(
  readFileSync(fileURLToPath(new URL('./package.json', import.meta.url)), 'utf8'),
) as { version: string }

// https://vite.dev/config/
// The vitest `test` block lives in vitest.config.ts (vitest/config's own
// defineConfig types it; mixing it in here breaks `tsc -p tsconfig.node.json`).
export default defineConfig({
  plugins: [svelte()],
  define: {
    __MUTA_CLIENT_VERSION__: JSON.stringify(version),
  },
})

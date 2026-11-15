#!/usr/bin/env node
// `beforeDevCommand`: build the backend modules, rebuild them on change,
// type-check the frontend on change, and run the Vite dev server.
//
// The plugin rewrites src/lib/wasm/*.ts whenever a module is (re)loaded, so
// changing a module's exports (e.g. a parameter type in its WIT) shows up
// here as a type error at every frontend call site that no longer fits.
import { spawn } from 'node:child_process'
import { buildAll, watchAll } from './build-wasm.mjs'

await buildAll()
watchAll()

const children = [
  spawn('vite', process.argv.slice(2), { stdio: 'inherit', shell: true }),
  // stdout: `tauri dev` does not show the stderr of `beforeDevCommand`.
  spawn('svelte-check', ['--watch', '--preserveWatchOutput', '--output', 'human'], {
    stdio: ['ignore', 'inherit', 'inherit'],
    shell: true
  })
]
children[1].on('exit', (code) => {
  if (code) console.log(`[check] svelte-check exited with ${code}: frontend type errors won't be reported (run \`pnpm install\`?)`)
})
children[0].on('exit', (code) => {
  children[1].kill()
  process.exit(code ?? 0)
})
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => children.forEach((c) => c.kill(signal)))
}

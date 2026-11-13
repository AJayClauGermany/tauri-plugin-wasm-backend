#!/usr/bin/env node
// `beforeDevCommand`: build the backend modules, rebuild them on change, and
// run the Vite dev server.
import { spawn } from 'node:child_process'
import { buildAll, watchAll } from './build-wasm.mjs'

await buildAll()
watchAll()

const vite = spawn('vite', process.argv.slice(2), { stdio: 'inherit', shell: true })
vite.on('exit', (code) => process.exit(code ?? 0))
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => vite.kill(signal))
}

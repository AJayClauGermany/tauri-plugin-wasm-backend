#!/usr/bin/env node
// Builds the backend modules in ./wasm into ./wasm/dist.
//
//   node scripts/build-wasm.mjs           build every module once
//   node scripts/build-wasm.mjs --watch   rebuild modules when they change
//
// A module is a directory in ./wasm containing either:
//   - index.js + wit/   JavaScript compiled to a WIT component with
//                       componentize-qjs (QuickJS: small and fast to load,
//                       which keeps hot reloads quick).
//   - Cargo.toml        Rust, built for wasm32-unknown-unknown when it depends
//                       on extism-pdk (Extism plugin), wasm32-wasip2 otherwise
//                       (WIT component).
//
// The Rust app picks up new builds by itself (hot reload); it is never
// recompiled because of a backend module change.

import { spawn } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, watch, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { basename, dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const wasmDir = join(root, 'wasm')
const distDir = join(wasmDir, 'dist')
const require = createRequire(import.meta.url)

/** The plugin's WIT package (`tauri:backend`), shipped in its npm package. */
function backendWit() {
  const pkg = require.resolve('tauri-plugin-wasm-backend-api/package.json')
  return join(dirname(pkg), 'wit', 'backend.wit')
}

function modules() {
  return readdirSync(wasmDir, { withFileTypes: true })
    .filter((d) => d.isDirectory() && d.name !== 'dist')
    .map((d) => join(wasmDir, d.name))
    .filter((dir) => existsSync(join(dir, 'Cargo.toml')) || existsSync(join(dir, 'index.js')))
}

/** Write atomically so the app never loads a half-written file. */
function writeOutput(name, bytes) {
  mkdirSync(distDir, { recursive: true })
  const out = join(distDir, `${name}.wasm`)
  const tmp = `${out}.tmp`
  writeFileSync(tmp, bytes)
  renameSync(tmp, out)
  return out
}

function run(cmd, args, cwd) {
  return new Promise((ok, fail) => {
    const child = spawn(cmd, args, { cwd, stdio: 'inherit', shell: process.platform === 'win32' })
    child.on('exit', (code) => (code === 0 ? ok() : fail(new Error(`${cmd} exited with ${code}`))))
    child.on('error', fail)
  })
}

async function buildJs(dir) {
  const { componentize } = await import('componentize-qjs')
  const deps = join(dir, 'wit', 'deps', 'tauri-backend')
  mkdirSync(deps, { recursive: true })
  copyFileSync(backendWit(), join(deps, 'backend.wit'))
  const entry = join(dir, 'index.js')
  const { component } = await componentize({
    witPath: join(dir, 'wit'),
    jsSource: readFileSync(entry, 'utf8'),
    jsPath: entry,
    // Plain (non-async) component ABI.
    sync: true
  })
  return component
}

async function buildRust(dir) {
  const manifest = readFileSync(join(dir, 'Cargo.toml'), 'utf8')
  const target = manifest.includes('extism-pdk') ? 'wasm32-unknown-unknown' : 'wasm32-wasip2'
  await run('cargo', ['build', '--release', '--target', target], dir)
  const crate = /^name\s*=\s*"([^"]+)"/m.exec(manifest)[1].replaceAll('-', '_')
  return readFileSync(join(dir, 'target', target, 'release', `${crate}.wasm`))
}

export async function build(dir) {
  const name = basename(dir)
  const started = Date.now()
  try {
    const bytes = existsSync(join(dir, 'Cargo.toml')) ? await buildRust(dir) : await buildJs(dir)
    const out = writeOutput(name, bytes)
    console.log(`[wasm] built ${relative(root, out)} (${(bytes.length / 1024).toFixed(0)} KiB) in ${Date.now() - started} ms`)
    return true
  } catch (e) {
    // stdout: `tauri dev` does not show the stderr of `beforeDevCommand`.
    console.log(`[wasm] failed to build ${name}: ${e?.message ?? e}`)
    return false
  }
}

export async function buildAll() {
  let ok = true
  for (const dir of modules()) ok = (await build(dir)) && ok
  return ok
}

/** Rebuild a module when one of its sources changes. */
export function watchAll() {
  for (const dir of modules()) {
    let timer
    let building = Promise.resolve()
    watch(dir, { recursive: true }, (_event, file) => {
      if (!file) return
      const f = file.replaceAll('\\', '/')
      if (f.startsWith('target/') || f.startsWith('wit/deps/') || f.endsWith('Cargo.lock')) return
      clearTimeout(timer)
      timer = setTimeout(() => {
        building = building.then(() => build(dir))
      }, 100)
    })
  }
  console.log('[wasm] watching wasm/ for changes')
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const ok = await buildAll()
  if (process.argv.includes('--watch')) watchAll()
  else if (!ok) process.exit(1)
}

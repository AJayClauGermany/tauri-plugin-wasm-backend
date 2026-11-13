# Tauri Plugin wasm-backend

Write parts of your Tauri backend as **WebAssembly modules** that are
**hot reloaded** while you develop, **sandboxed by capabilities**, and called
from the frontend **the same way as Rust commands**.

Rust compile times grow with your app, and even a one-line backend change means
recompiling and restarting it. Backend modules run inside the app in a WASM
runtime instead: rebuild a module (in under a second for JavaScript) and the
running app swaps it in, without recompiling Rust or losing frontend state.

- **Two engines**
  - **WIT components** ([component model](https://component-model.bytecodealliance.org/), run by
    [wasmtime](https://wasmtime.dev)): the future-facing option. Exported
    functions are commands with typed parameters.
  - **[Extism](https://extism.org) plugins**: for languages whose toolchains
    don't target the component model yet (Extism has
    [PDKs](https://extism.org/docs/concepts/pdk) for JS, Go, C#, Python, Zig, …).
- **Tauri conventions**: commands, events and `Result` semantics work like
  they do for Rust commands, so there's nothing to remap.
- **Capabilities**: modules can only emit/listen to the events, call the host
  commands and reach the files/network you grant them. The frontend's access
  to modules is controlled by regular Tauri permissions and scopes.
- **Two use cases**: fast iteration on your own backend logic during
  development, and **third-party plugins** loaded from a directory at runtime.
- **Generated TypeScript types** for WIT modules, rewritten on every reload.

## Install

```toml
# src-tauri/Cargo.toml
[dependencies]
tauri-plugin-wasm-backend = { git = "https://github.com/AJayClauGermany/tauri-plugin-wasm-backend" }
```

```sh
pnpm add tauri-plugin-wasm-backend-api
```

Cargo features (all but `specta` are on by default): `component` (WIT engine),
`extism` (Extism engine), `hot-reload` (file watching), `specta` (derive
`specta::Type` on the public models for
[tauri-specta](https://github.com/specta-rs/tauri-specta)).

Wasmtime compiles WASM to native code when a module is loaded, and an
unoptimized compiler makes that slow: a JavaScript module takes ~7 s to load
in a default debug build. Optimizing just the compiler crates brings this to
~1 s (and ~0.5 s for a reload) at the cost of a few minutes on the first
build. Copy the `[profile.dev.package.*]` sections from
[`examples/tauri-app/src-tauri/Cargo.toml`](examples/tauri-app/src-tauri/Cargo.toml):

```toml
[profile.dev.package.cranelift-codegen]
opt-level = 3
[profile.dev.package.wasmtime-internal-cranelift]
opt-level = 3
# … (18 crates in total)
```

## Quick start

### 1. Register the plugin

```rust
// src-tauri/src/lib.rs
tauri::Builder::default()
    .plugin(
        tauri_plugin_wasm_backend::Builder::new()
            // Functions modules may call back into (if their capabilities allow it).
            .host_command("app_version", |ctx: tauri_plugin_wasm_backend::HostCallContext<_>, _: ()| {
                Ok::<_, String>(ctx.app.package_info().version.to_string())
            })
            .build(),
    )
```

### 2. Configure modules

```jsonc
// src-tauri/tauri.conf.json
{
  "plugins": {
    "wasm-backend": {
      "modules": {
        "greeter": {
          "path": "wasm/greeter.wasm",              // release: relative to the resource dir
          "devPath": "../wasm/dist/greeter.wasm",   // debug: relative to src-tauri
          "capabilities": {
            "hostCommands": ["app_version"],
            "events": { "emit": ["greeter://*"], "listen": ["app://ping"] },
            "timeoutMs": 1000
          }
        }
      },
      // Optional: write typed bindings on every (re)load in debug builds.
      "typescriptOutDir": "../src/lib/wasm"
    }
  },
  "bundle": { "resources": { "../wasm/dist/": "wasm/" } }
}
```

Modules can also be registered in Rust (`Builder::module`), embedded in the
binary (`ModuleConfig::from_bytes(include_bytes!(…))`) or loaded at runtime
(`app.wasm_backend().load(…)`).

### 3. Write a module

Describe its commands in WIT. Include the plugin's `tauri:backend` world to get
the host API (`wit/backend.wit` in this repository, also shipped in the npm
package as `tauri-plugin-wasm-backend-api/wit/backend.wit`; put it in
`wit/deps/tauri-backend/`):

```wit
package example:greeter;

world greeter {
  include tauri:backend/module-with-events@0.1.0;

  record person { name: string, age-years: u32 }

  export greet: func(name: string) -> string;
  export describe: func(person: person) -> result<string, string>;
}
```

Implement it in JavaScript:

```js
import { log, emit, invoke } from 'tauri:backend/host@0.1.0'

export function greet(name) {
  emit('greeter://greeted', JSON.stringify({ name }))
  return `Hello, ${name}!`
}

export function describe(person) {
  if (!person.name) throw 'a person needs a name' // rejects the frontend's promise
  const version = JSON.parse(invoke('app_version', 'null'))
  return `${person.name} is ${person.ageYears} (app v${version})`
}

export const events = {
  onEvent(event, payload) { log('info', `got ${event}: ${payload}`) },
}
```

and compile it with [componentize-qjs](https://github.com/andreiltd/componentize-qjs):

```js
import { componentize } from 'componentize-qjs'
const { component } = await componentize({
  witPath: 'wasm/greeter/wit',
  jsSource: await readFile('wasm/greeter/index.js', 'utf8'),
  sync: true,
})
```

or [ComponentizeJS](https://github.com/bytecodealliance/ComponentizeJS)
(SpiderMonkey: more complete JS support, but ~10× larger and slower to load,
so reloads take seconds instead of a fraction of one). The example's
[`scripts/build-wasm.mjs`](examples/tauri-app/scripts/build-wasm.mjs) does
this and rebuilds on change; run it alongside Vite from `beforeDevCommand`.

Any language that targets WIT components works the same way, e.g. Rust with
[`wit-bindgen`](https://github.com/bytecodealliance/wit-bindgen) and
`cargo build --target wasm32-wasip2`.

### 4. Call it from the frontend

```ts
import { invokeModule } from 'tauri-plugin-wasm-backend-api'

// Like invoke('greet', { name }) for a Rust command:
const greeting = await invokeModule<string>('greeter', 'greet', { name: 'Ada' })

// Or with the generated, typed bindings:
import { greeter } from './lib/wasm/greeter'
await greeter.describe({ name: 'Ada', ageYears: 36 })
```

…or from Rust:

```rust
use tauri_plugin_wasm_backend::WasmBackendExt;
let greeting: String = app.wasm_backend().call("greeter", "greet", json!({ "name": "Ada" }))?;
```

Edit `index.js` while `tauri dev` runs: the module is rebuilt and swapped in,
and the frontend receives `wasm-backend://module-reloaded`
(`onModuleReloaded`). Module state is reset on reload.

## Commands, arguments and results

| Tauri (Rust command)                     | WIT module                                         |
|------------------------------------------|----------------------------------------------------|
| `#[tauri::command] fn get_user(user_id)` | `export get-user: func(user-id: u32) -> …`         |
| `invoke('get_user', { userId })`         | `invokeModule('m', 'get_user', { userId })`        |
| `Result<T, E>` → resolve / reject        | `result<T, E>` → resolve / reject                  |

- Command names match across `kebab-case`, `snake_case` and `camelCase`.
  Functions in an exported interface are invoked as `interface.function`.
- Arguments are an object keyed by parameter name (any casing).
- Values follow [jco](https://github.com/bytecodealliance/jco)'s JS
  conventions: records are objects with camelCase keys, `option<T>` is
  `T | null`, enums are strings, variants are `{ tag, val }`, flags are arrays
  of names, `list<u8>` is an array of numbers.
- A trap (panic, uncaught JS exception, timeout, out of memory) rejects the
  call; the module is re-instantiated (state reset) for the next call.

## Host API

Modules call into the app through the `tauri:backend/host` interface
([`wit/backend.wit`](wit/backend.wit)). Payloads are JSON strings:

| function                       | capability needed                 |
|--------------------------------|-----------------------------------|
| `log(level, message)`          | none (goes to the `log` crate)    |
| `emit(event, payload)`         | `events.emit` matches the event   |
| `invoke(command, args)`        | `hostCommands` matches the command|

Events named in `events.listen` (e.g. emitted by the frontend with
`emit('app://ping', …)`) are delivered to the module's `on-event` export.

Host commands are registered with `Builder::host_command`. They take any
`Deserialize` arguments and return `Result<impl Serialize, impl Display>`, and
receive a `HostCallContext` with the `AppHandle` and the calling module's name.

## Capabilities

Modules are deny-by-default:

```jsonc
"capabilities": {
  "hostCommands": ["app_version", "db_*"],   // `*` wildcards
  "events": {
    "emit": ["greeter://*"],
    "listen": ["app://ping"]                // exact names
  },
  "wasi": {
    "stdio": true,                          // inherit stdout/stderr
    "env": { "MODE": "dev" },               // Extism: exposed as config values
    "preopens": [{ "host": "$APPDATA/greeter", "guest": "/data", "readOnly": false }],
    "network": false                        // WIT only: TCP/UDP/DNS
  },
  "allowedHosts": ["api.example.com"],      // Extism HTTP only
  "memoryLimitMb": 64,
  "timeoutMs": 1000
}
```

### Frontend access

The plugin's commands are regular Tauri commands, controlled by
[permissions](permissions/autogenerated/reference.md):

- `wasm-backend:default`: `allow-invoke` and `allow-list-modules`.
- `allow-reload-module`, `allow-typescript`: development helpers.

`invoke` can call every module that is *exposed* (app modules unless
`"expose": false`; external plugins never), plus whatever `allow` scope entries
match. Like Tauri capabilities, allow entries only add access; use `deny`
entries or `"expose": false` to restrict it:

```json
{
  "identifier": "wasm-backend:allow-invoke",
  "allow": [{ "module": "greeter" }, { "module": "stats", "command": "summarize" }],
  "deny": [{ "module": "*", "command": "admin*" }]
}
```

## Third-party plugins

Let users extend your app with WASM plugins dropped into a directory:

```jsonc
"external": {
  "dir": "$APPDATA/plugins",           // every *.wasm file is a module named after the file
  "capabilities": { "events": { "emit": ["plugin://*"] } },  // granted to every plugin
  "expose": false                      // frontend needs explicit invoke scopes
}
```

New files are loaded and deleted ones unloaded while the app runs (when hot
reload is on). For finer control, e.g. per-plugin consent, use
`app.wasm_backend().load(name, ModuleConfig::from_path(…).capabilities(…))`.

## Extism modules

Core WASM modules use the Extism engine (or set `"engine": "extism"`). Each
exported function is a command: it receives the invoke `args` as JSON input,
and its output is parsed as JSON (or returned as a string). Errors reject the
call. Host functions take and return JSON strings:

| function       | input                    | output                                    |
|----------------|--------------------------|-------------------------------------------|
| `tauri_log`    | `{ level, message }`     | -                                         |
| `tauri_emit`   | `{ event, payload }`     | `{ "ok": null }` or `{ "err": "…" }`      |
| `tauri_invoke` | `{ command, args }`      | `{ "ok": result }` or `{ "err": "…" }`    |

An exported `on_event` (or `onEvent`) receives `{ event, payload }`.

In Rust ([`extism-pdk`](https://github.com/extism/rust-pdk)), see
[`examples/tauri-app/wasm/stats`](examples/tauri-app/wasm/stats). With the
[JS PDK](https://github.com/extism/js-pdk) (`extism-js`), declare the host
functions in your `.d.ts` and call them through `Host.getFunctions()`:

```ts
declare module 'main' {
  export function greet(): I32
}
declare module 'extism:host' {
  interface user {
    tauri_emit(ptr: I64): I64
  }
}
```

```js
const { tauri_emit } = Host.getFunctions()
export function greet() {
  const { name } = JSON.parse(Host.inputString())
  const reply = tauri_emit(Memory.fromString(JSON.stringify({ event: 'greeter://greeted', payload: { name } })).offset)
  Host.outputString(`Hello, ${name}!`)
}
```

## TypeScript types

For WIT modules the plugin generates a typed client from the component's
actual exports:

```ts
// src/lib/wasm/greeter.ts (generated)
export const greeter = {
  greet: (name: string): Promise<string> => invokeModule<string>("greeter", "greet", { name }),
  describe: (person: { name: string; ageYears: number }): Promise<string> => …,
}
```

Set `typescriptOutDir` (or `Builder::typescript_out_dir`) to write these on
every load/reload in debug builds, or fetch them with `moduleTypescript(name)`.
With the `specta` feature, the plugin's own models (`ModuleInfo`,
`Capabilities`, …) implement `specta::Type` for use with tauri-specta.

## Example

[`examples/tauri-app`](examples/tauri-app) has a JavaScript WIT module
(`wasm/greeter`) and a Rust Extism module (`wasm/stats`):

```sh
cd examples/tauri-app
pnpm install
pnpm tauri dev   # then edit wasm/greeter/index.js
```

## Platform notes

Wasmtime and Extism compile WASM to native code at runtime, which works on
desktop platforms. iOS forbids runtime code generation; mobile support is not
part of this initial version.

## License

Licensed under the [Apache License, Version 2.0 with LLVM Exceptions](LICENSE) (`Apache-2.0 WITH LLVM-exception`).

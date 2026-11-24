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
            .host_command("app_version", |ctx, _: ()| {
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

// Host commands the module calls (see "Typed host commands").
interface app {
  app-version: func() -> string;
}

world greeter {
  include tauri:backend/module-with-events@0.1.0;
  import app;

  record person { name: string, age-years: u32 }

  export greet: func(name: string) -> string;
  export describe: func(person: person) -> result<string, string>;
}
```

Implement it in JavaScript:

```js
import { log, emit } from 'tauri:backend/host@0.1.0'
import { appVersion } from 'example:greeter/app'

export function greet(name) {
  emit('greeter://greeted', JSON.stringify({ name }))
  return `Hello, ${name}!`
}

export function describe(person) {
  if (!person.name) throw 'a person needs a name' // rejects the frontend's promise
  return `${person.name} is ${person.ageYears} (app v${appVersion()})`
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
  call; the module is re-instantiated (state reset) for the next call, and
  `wasm-backend://module-trapped` is emitted (`onModuleTrapped`). With the
  `maxRestarts` capability, a module that keeps trapping is stopped until it
  is reloaded instead of being restarted forever.
  To return an error the frontend can handle, declare the function as
  `-> result<T, E>`: a JS `throw` then becomes `err` and the promise rejects
  with the thrown value, keeping module state. In a function without a
  `result` return type (e.g. `-> string`), a `throw` has nowhere to go and
  traps, so the frontend gets only a generic
  ``"WASM runtime error: … wasm `unreachable` instruction executed"`` string
  and module state is reset.
- A module that fails to *build* (e.g. a JS syntax error) never reaches the
  app: the build tool reports it, and the previously loaded version keeps
  running.

## Host API

Modules call into the app through the `tauri:backend/host` interface
([`wit/backend.wit`](wit/backend.wit)). Payloads are JSON strings:

| function                       | capability needed                 |
|--------------------------------|-----------------------------------|
| `log(level, message)`          | none (goes to the `log` crate)    |
| `emit(event, payload)`         | `events.emit` matches the event   |
| `invoke(command, args)`        | `hostCommands` matches the command|

A module's memory is reset whenever it is reloaded (every hot reload) or
traps. For what should outlive that, import the `tauri:backend/state`
interface next to the base world: a key-value store of the module's own,
kept by the app.

```wit
world counter {
  include tauri:backend/module@0.1.0;
  import tauri:backend/state@0.1.0;
  export next: func() -> u32;
}
```

| function             | capability needed          |
|----------------------|----------------------------|
| `get(key)`           | `state` matches the key    |
| `set(key, value)`    | `state` matches the key    |
| `remove(key)`        | `state` matches the key    |
| `keys()`             | lists the keys `state` matches |

Values are strings (at most 64 KiB; keys at most 256 bytes). Writes made
during a call are kept when it returns and dropped if it traps, so a crash
never leaves half an update behind. State is stored in
`$APPDATA/wasm-backend/state`, one JSON file per module, so it survives
restarting the app; change the directory with `"stateDir"` (or
`Builder::state_dir`), or keep it elsewhere with `Builder::state_store`.
From Rust, `app.wasm_backend().state(module)` reads a module's store and
`clear_state(module)` wipes it (unloading a module keeps its state).

Events named in `events.listen` (e.g. emitted by the frontend with
`emit('app://ping', …)`) are delivered to the module's `on-event` export.

Host commands are registered with `Builder::host_command`. They take any
`Deserialize` arguments and return `Result<impl Serialize, impl Display>`, and
receive a `HostCallContext` with the `AppHandle` and the calling module's name.
Like module commands, their names match across `snake_case`, `kebab-case` and
`camelCase`, and "no arguments" may be sent as `null` or `{}`.

### Typed host commands

Instead of passing JSON strings to `invoke`, a WIT module can import host
commands as ordinary typed functions. Every function imported from an
interface outside `wasi:*` and `tauri:backend/*` is backed by the host command
with the same name (`interface.function` is tried first, then `function`):

```wit
interface app {
  record version { major: u32, minor: u32 }
  app-version: func() -> version;                                 // host command `app_version`
  add-numbers: func(left-side: s32, right-side: s32) -> result<s32, string>;
}

world greeter {
  include tauri:backend/module@0.1.0;
  import app;
}
```

```js
import { appVersion, addNumbers } from 'example:greeter/app'
const { major } = appVersion()
```

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddArgs { left_side: i32, right_side: i32 }

Builder::new().host_command("add_numbers", |_ctx, a: AddArgs| {
    a.left_side.checked_add(a.right_side).ok_or("overflow")
})
```

- Arguments reach the host command as an object keyed by camelCase
  parameter name, the way `invoke` arguments reach a Tauri command, and its
  JSON result is converted to the declared WIT type.
- A host `Err` becomes the `err` case of a `result` return type. Without one
  it traps the module, like a `throw` in an export without a `result`.
- Imports are checked **when the module loads**: each one needs a registered
  host command that `capabilities.hostCommands` allows. A missing, renamed or
  disallowed command fails the load with every problem listed (a hot reload
  keeps the previous version running), instead of failing at the first call.
  `ModuleInfo::imports` lists them, e.g. to show what a plugin will use.

Publishing the interface as a `.wit` file gives plugin authors the app's
contract to build against; wit-bindgen turns it into typed Rust functions,
so a change there breaks their build instead of their plugin.

## What catches a change

Each boundary of a module is checked as early as the information exists:

| You change…                                  | Caught by                                                                                         | When            |
|----------------------------------------------|---------------------------------------------------------------------------------------------------|-----------------|
| A WIT export (name, parameters, types)       | The regenerated [TypeScript bindings](#typescript-types): type errors at frontend call sites      | on reload       |
| …called through untyped `invokeModule`       | `invalid arguments for …: missing required key …` / `expected …, got …`                           | on call         |
| A host command's name                        | Modules importing it fail to load; stale `hostCommands` entries become `ModuleInfo::warnings`     | on load         |
| A host command's result type                 | Typed imports: `host command … returned a value that doesn't match its import …: .field: …`      | on call         |
| A host command's argument type               | `invalid arguments for host command …` (serde's message)                                          | on call         |
| A module's imports or capabilities           | Load error listing every import that is unregistered or not allowed                              | on load         |
| `events.listen` for a module without handler | `ModuleInfo::warnings`                                                                            | on load         |
| Module calls through string `invoke`         | Only the call itself: arguments and result are untyped JSON                                       | on call         |
| Event payloads, Extism exports               | Not checked (untyped, like Tauri events); bindings use `unknown`                                  | -               |

Warnings are logged (`log` crate, target `wasm_backend`) and included in the
`ModuleInfo` sent with `wasm-backend://module-loaded`/`-reloaded`, so a dev UI
can show them. Frontend type errors only appear if something type-checks the
frontend: the example runs `svelte-check --watch` from its `pnpm dev`, so a
changed WIT signature shows up in the `tauri dev` terminal.

## Capabilities

Modules are deny-by-default:

```jsonc
"capabilities": {
  "hostCommands": ["app_version", "db_*"],   // `*` wildcards
  "state": ["*"],                           // keys of its state store, `*` wildcards
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
  "timeoutMs": 1000,
  "maxRestarts": 3                          // stop after the 4th trap, until reloaded
}
```

`listModules()` shows what each module holds and uses: its `capabilities`,
the interfaces it imports (`requires`, WIT modules only), the SHA-256 of its
bytes, how often it trapped (`traps`) and whether it was `stopped`.

A WIT module doesn't need WASI. A Rust crate using `wit-bindgen` built with
`cargo build --target wasm32-unknown-unknown` is loaded as a component too,
and imports no `wasi:*` interface at all: the host API and the host commands
it imports are everything it can reach.

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

To decide which plugins run at all, set a load gate. It is asked before any
module is loaded or reloaded, and before a byte of it is compiled:

```rust
tauri_plugin_wasm_backend::Builder::new()
    .load_gate(|app, request| {
        if !request.external || approved(app).contains(request.sha256) {
            Ok(())
        } else {
            Err("this plugin has not been approved".into())
        }
    })
```

A refused load is reported like any other load failure
(`wasm-backend://module-error`); a refused reload keeps the previous version
running. Give third-party plugins a `maxRestarts` so one that keeps crashing
is stopped.

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
| `tauri_state_get`    | `{ key }`          | `{ "ok": value or null }` or `{ "err": "…" }` |
| `tauri_state_set`    | `{ key, value }`   | `{ "ok": null }` or `{ "err": "…" }`      |
| `tauri_state_remove` | `{ key }`          | `{ "ok": null }` or `{ "err": "…" }`      |
| `tauri_state_keys`   | `{}`               | `{ "ok": [key, …] }` or `{ "err": "…" }`  |

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
Record and variant types are inlined, as components don't keep the names of
world-level types; name one with
`type Person = Parameters<typeof greeter.describe>[0]` instead of copying it,
so it follows the module.
With the `specta` feature, the plugin's own models (`ModuleInfo`,
`Capabilities`, …) implement `specta::Type` for use with tauri-specta.

## Example

[`examples/tauri-app`](examples/tauri-app) has a JavaScript WIT module
(`wasm/greeter`) and a Rust Extism module (`wasm/stats`):

The example uses the frontend API from this repository (`file:../../`), so
build it first. Its `dist-js/` output isn't committed, and without it Vite
fails with `Failed to resolve import "tauri-plugin-wasm-backend-api"`.

```sh
# 1. Build the frontend API at the repository root
pnpm install
pnpm build

# 2. Install and run the example
cd examples/tauri-app
pnpm install
pnpm tauri dev   # then edit wasm/greeter/index.js
```

pnpm copies `file:` dependencies when it installs them, so run
`pnpm install` in `examples/tauri-app` again whenever you rebuild the API
(after the first build, or after changing `guest-js/`). If the import still
fails, delete `examples/tauri-app/node_modules` and reinstall.

## Using the core outside Tauri

The engines, capabilities, module registry and hot reload live in the
framework-agnostic `wasm-backend-core` crate, developed in
[rust-wasm-core-interchange](https://github.com/AJayClauGermany/rust-wasm-core-interchange)
together with guest modules in several languages that test it. The
Tauri plugin is a thin layer on top of it (the plugin, IPC commands,
permissions and `TauriPlatform`) and re-exports its types, so Tauri apps
don't need to depend on it directly.

Other Rust apps (GPUI and its derivatives, CLIs, servers, tests) can host
the same modules by implementing the `Platform` trait: how events are
delivered, where the standard directories (`$APPDATA`, `$RESOURCE`, …) are,
and where blocking work runs. Apps without an event system can delegate to
the bundled `EventBus`.

```rust
use wasm_backend_core::{Backend, ModuleConfig};

let backend = Backend::builder(MyPlatform::new())
    .host_command("now", |_call, _args: ()| Ok::<_, String>(42))
    .build()?;
backend.load_all([("greeter".into(), ModuleConfig::from_path("greeter.wasm"))]);
let out = backend.invoke("greeter", "greet", serde_json::json!({ "name": "GPUI" }))?;
```

`Backend::invoke_async` runs the call through `Platform::spawn_blocking` and
returns a plain future, so it works with any async runtime (tokio, smol,
GPUI's executors). The core itself is synchronous and doesn't depend on one.

See the crate docs for a complete `Platform` implementation. The guest API
(the `tauri:backend` WIT package and the `tauri_*` Extism host functions) is
identical on every platform, so a module built for a Tauri app runs
unchanged elsewhere. Frontend access control (the `invoke` scopes) is a Tauri
concept and stays in the plugin; other integrations decide themselves who
may call `Backend::invoke` (`ModuleInfo::exposed` carries the module's
setting).

## Platform notes

Wasmtime and Extism compile WASM to native code at runtime, which works on
desktop platforms. iOS forbids runtime code generation; mobile support is not
part of this initial version.

## License

Licensed under the [Apache License, Version 2.0 with LLVM Exceptions](LICENSE) (`Apache-2.0 WITH LLVM-exception`).

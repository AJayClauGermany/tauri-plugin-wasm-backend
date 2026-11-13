# tauri-plugin-wasm-backend example

A Tauri + Svelte app with a JavaScript WIT module (`wasm/greeter`) and a Rust
Extism module (`wasm/stats`).

## Running

Build the frontend API at the repository root first. The example depends on
it via `file:../../`, and its `dist-js/` output isn't committed:

```sh
# from the repository root
pnpm install
pnpm build

# then, from examples/tauri-app
pnpm install
pnpm tauri dev   # then edit wasm/greeter/index.js
```

pnpm copies `file:` dependencies at install time, so rerun `pnpm install` here
after rebuilding the API. If Vite reports `Failed to resolve import
"tauri-plugin-wasm-backend-api"`, the copy is stale: delete `node_modules` and
reinstall.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).


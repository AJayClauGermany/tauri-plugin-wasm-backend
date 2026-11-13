use tauri::Emitter;
use tauri_plugin_wasm_backend::{Builder, HostCallContext, WasmBackendExt};

// Learn more about Tauri commands at https://v2.tauri.app/develop/calling-rust/#commands
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![greet])
        .plugin(
            // Modules and their capabilities are configured in tauri.conf.json
            // (`plugins.wasm-backend`); host commands are registered here.
            Builder::new()
                .host_command("app_version", |ctx: HostCallContext<_>, _: ()| {
                    Ok::<_, String>(ctx.app.package_info().version.to_string())
                })
                .build(),
        )
        .setup(|app| {
            // Rust can call WASM modules too.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                if let Ok(greeting) = handle.wasm_backend().call::<_, String>(
                    "greeter",
                    "greet",
                    serde_json::json!({ "name": "Rust" }),
                ) {
                    println!("{greeting}");
                }
                let _ = handle.emit("app://ready", ());
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

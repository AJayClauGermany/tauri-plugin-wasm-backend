//! End-to-end tests running real WASM modules inside a mock Tauri app.
//!
//! The guest fixtures in `tests/fixtures` are built on demand, which needs
//! the `wasm32-wasip2` and `wasm32-unknown-unknown` Rust targets.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock, mpsc},
    time::Duration,
};

use serde_json::{Value, json};
use tauri::{Listener, Manager, test::MockRuntime};
use tauri_plugin_wasm_backend::{
    Builder, Capabilities, EngineKind, Error, ExternalConfig, ModuleConfig, WasmBackend,
    WasmBackendExt,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Build a fixture crate (once per test run) and return the `.wasm` path.
fn fixture(name: &str, target: &str) -> PathBuf {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = root().join("tests/fixtures").join(name);
    let target_dir = root().join("target/fixtures");
    let out = target_dir
        .join(target)
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    static BUILT: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut built = BUILT.lock().unwrap();
    if !built.contains(&name.to_string()) {
        // Component guests import the plugin's WIT package.
        let deps = dir.join("wit/deps/tauri-backend");
        if dir.join("wit").exists() {
            std::fs::create_dir_all(&deps).unwrap();
            std::fs::copy(root().join("wit/backend.wit"), deps.join("backend.wit")).unwrap();
        }
        let status = Command::new(env!("CARGO"))
            .args(["build", "--release", "--target", target, "--manifest-path"])
            .arg(dir.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", &target_dir)
            .status()
            .expect("failed to run cargo");
        assert!(status.success(), "failed to build fixture {name}");
        built.push(name.to_string());
    }
    out
}

fn component() -> PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| fixture("component-guest", "wasm32-wasip2"))
        .clone()
}

fn extism() -> PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| fixture("extism-guest", "wasm32-unknown-unknown"))
        .clone()
}

fn caps() -> Capabilities {
    Capabilities::default()
        .allow_emit("test://*")
        .allow_host_command("echo")
        .allow_host_command("reenter")
        .allow_host_command("list")
        .listen("app://ping")
}

fn app(builder: Builder<MockRuntime>) -> tauri::App<MockRuntime> {
    tauri::test::mock_builder()
        .plugin(
            builder
                .host_command("echo", |ctx, args: Value| {
                    Ok::<_, String>(json!({ "module": ctx.module, "args": args }))
                })
                .host_command("reenter", |ctx, _: Value| {
                    ctx.app
                        .wasm_backend()
                        .invoke(&ctx.module, "count", Value::Null)
                        .map_err(|e| e.to_string())
                })
                // Reads the registry while the calling module is running.
                .host_command("list", |ctx, _: Value| {
                    let b = ctx.app.wasm_backend();
                    let names: Vec<String> = b.modules().into_iter().map(|m| m.name).collect();
                    let info = b.module(&ctx.module).map_err(|e| e.to_string())?;
                    let ts = b.typescript(&ctx.module).map_err(|e| e.to_string())?;
                    Ok::<_, String>(
                        json!({ "names": names, "self": info.name, "ts": !ts.is_empty() }),
                    )
                })
                .host_command("secret", |_, _: ()| Ok::<_, String>("nope"))
                .build(),
        )
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("failed to build app")
}

fn backend(app: &tauri::App<MockRuntime>) -> WasmBackend<MockRuntime> {
    app.wasm_backend().clone()
}

fn wait_for<T>(rx: &mpsc::Receiver<T>) -> T {
    rx.recv_timeout(Duration::from_secs(30))
        .expect("timed out waiting for event")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = root().join("target/test-tmp").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn component_commands_and_types() {
    let app = app(Builder::new().module(
        "fixture",
        ModuleConfig::from_path(component()).capabilities(caps()),
    ));
    let b = backend(&app);

    let info = b.module("fixture").unwrap();
    assert_eq!(info.engine, EngineKind::Component);
    let names: Vec<_> = info.commands.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"greet"), "{names:?}");
    assert!(names.contains(&"math.double"), "{names:?}");

    assert_eq!(
        b.invoke("fixture", "greet", json!({ "name": "Tauri" }))
            .unwrap(),
        json!("Hello, Tauri!")
    );
    assert_eq!(
        b.invoke("fixture", "add", json!({ "a": 2, "b": 40 }))
            .unwrap(),
        json!(42)
    );
    // Interface exports, and any naming convention for commands.
    assert_eq!(
        b.invoke("fixture", "math.double", json!({ "x": 21 }))
            .unwrap(),
        json!(42)
    );
    assert_eq!(
        b.invoke("fixture", "perms_of", json!({ "p": ["write", "read"] }))
            .unwrap(),
        json!(["read", "write"])
    );
    assert_eq!(
        b.invoke("fixture", "permsOf", json!({ "p": { "read": true } }))
            .unwrap(),
        json!(["read"])
    );

    // Records use camelCase keys; options are null; result<T, E> resolves or rejects.
    assert_eq!(
        b.invoke(
            "fixture",
            "describe",
            json!({ "person": { "name": "Ada", "ageYears": 36 } })
        )
        .unwrap(),
        json!({ "name": "Ada", "ageYears": 36, "nickname": "ada" })
    );
    match b.invoke(
        "fixture",
        "describe",
        json!({ "person": { "name": "Old", "age_years": 200, "nickname": null } }),
    ) {
        Err(Error::Command(v)) => assert_eq!(v, json!("Old is too old")),
        other => panic!("expected command error, got {other:?}"),
    }

    // Variants, enums and tuples.
    assert_eq!(
        b.invoke(
            "fixture",
            "area",
            json!({ "shape": { "tag": "square", "val": 3.0 } })
        )
        .unwrap(),
        json!(9.0)
    );
    assert_eq!(
        b.invoke("fixture", "area", json!({ "shape": { "square": 2.0 } }))
            .unwrap(),
        json!(4.0)
    );
    assert_eq!(
        b.invoke("fixture", "area", json!({ "shape": "point" }))
            .unwrap(),
        json!(0.0)
    );
    assert_eq!(
        b.invoke("fixture", "flip", json!({ "mood": "happy" }))
            .unwrap(),
        json!("sad")
    );
    assert_eq!(
        b.invoke("fixture", "swap", json!({ "t": [7, "x"] }))
            .unwrap(),
        json!(["x", 7])
    );

    // Typed helper.
    let sum: i32 = b.call("fixture", "add", json!({ "a": 1, "b": 2 })).unwrap();
    assert_eq!(sum, 3);

    // Errors.
    assert!(matches!(
        b.invoke("fixture", "nope", Value::Null),
        Err(Error::CommandNotFound { .. })
    ));
    assert!(matches!(
        b.invoke("fixture", "add", json!({ "a": 1 })),
        Err(Error::InvalidArgs { .. })
    ));
    assert!(matches!(
        b.invoke("fixture", "add", json!({ "a": "1", "b": 2 })),
        Err(Error::InvalidArgs { .. })
    ));
    assert!(matches!(
        b.invoke("missing", "add", Value::Null),
        Err(Error::ModuleNotFound(_))
    ));

    // TypeScript bindings.
    let ts = b.typescript("fixture").unwrap();
    assert!(
        ts.contains("greet: (name: string): Promise<string>"),
        "{ts}"
    );
    assert!(
        ts.contains("describe: (person: { name: string; ageYears: number; nickname?: string | null }): Promise<{ name: string; ageYears: number; nickname?: string | null }>"),
        "{ts}"
    );
    assert!(
        ts.contains("mathDouble: (x: number): Promise<number>"),
        "{ts}"
    );
}

#[test]
fn component_state_traps_and_timeouts() {
    let app = app(Builder::new().module(
        "fixture",
        ModuleConfig::from_path(component()).capabilities(caps().timeout_ms(200)),
    ));
    let b = backend(&app);

    // State persists between calls.
    assert_eq!(b.invoke("fixture", "count", Value::Null).unwrap(), json!(1));
    assert_eq!(b.invoke("fixture", "count", Value::Null).unwrap(), json!(2));

    // A trap is reported and the instance is recreated (state reset).
    assert!(matches!(
        b.invoke("fixture", "crash", Value::Null),
        Err(Error::Runtime(_))
    ));
    assert_eq!(b.invoke("fixture", "count", Value::Null).unwrap(), json!(1));

    // Infinite loops are interrupted.
    let started = std::time::Instant::now();
    match b.invoke("fixture", "spin", Value::Null) {
        Err(Error::Runtime(msg)) => assert!(msg.contains("timed out"), "{msg}"),
        other => panic!("expected timeout, got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        b.invoke("fixture", "add", json!({ "a": 1, "b": 1 }))
            .unwrap(),
        json!(2)
    );
}

/// Shared checks for the host API (events, host commands, capabilities).
fn check_host_api(
    b: &WasmBackend<MockRuntime>,
    app: &tauri::App<MockRuntime>,
    module: &str,
    extism: bool,
) {
    let emit = |event: &str, payload: Value| {
        if extism {
            b.invoke(
                module,
                "emit_event",
                json!({ "event": event, "payload": payload }),
            )
        } else {
            b.invoke(
                module,
                "emit-event",
                json!({ "name": event, "payload": payload.to_string() }),
            )
        }
    };
    let call_host = |command: &str, args: Value| {
        if extism {
            b.invoke(
                module,
                "call_host",
                json!({ "command": command, "args": args }),
            )
        } else {
            b.invoke(
                module,
                "call-host",
                json!({ "command": command, "args": args.to_string() }),
            )
            .map(|v| serde_json::from_str(v.as_str().unwrap()).unwrap())
        }
    };

    // Emitting an allowed event reaches app listeners.
    let (tx, rx) = mpsc::channel();
    app.listen_any("test://hello", move |e| {
        tx.send(e.payload().to_string()).unwrap();
    });
    emit("test://hello", json!({ "n": 1 })).unwrap();
    assert_eq!(wait_for(&rx), r#"{"n":1}"#);

    // Emitting a disallowed event is rejected.
    let err = emit("other://hello", json!(null)).unwrap_err().to_string();
    assert!(err.contains("not allowed to emit"), "{err}");

    // Host commands receive the calling module's name.
    assert_eq!(
        call_host("echo", json!({ "x": 1 })).unwrap(),
        json!({ "module": module, "args": { "x": 1 } })
    );
    let err = call_host("secret", json!(null)).unwrap_err().to_string();
    assert!(err.contains("not allowed to invoke"), "{err}");

    // Host commands can inspect the registry while the module is running.
    assert_eq!(
        call_host("list", json!(null)).unwrap(),
        json!({ "names": [module], "self": module, "ts": true })
    );

    // Calling back into the same module is detected instead of deadlocking.
    let err = call_host("reenter", json!(null)).unwrap_err().to_string();
    assert!(err.contains("reentrant"), "{err}");

    // Subscribed app events are delivered to the module.
    use tauri::Emitter;
    app.emit("app://ping", json!({ "hello": "module" }))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let last = b.invoke(module, "last_event", Value::Null).unwrap();
        if !last.is_null() {
            if extism {
                assert_eq!(
                    last,
                    json!({ "event": "app://ping", "payload": { "hello": "module" } })
                );
            } else {
                assert_eq!(last, json!(r#"app://ping:{"hello":"module"}"#));
            }
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event was not delivered"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn component_host_api() {
    let app = app(Builder::new().module(
        "fixture",
        ModuleConfig::from_path(component()).capabilities(caps()),
    ));
    check_host_api(&backend(&app), &app, "fixture", false);
}

#[test]
fn extism_host_api() {
    let app = app(Builder::new().module(
        "ext",
        ModuleConfig::from_path(extism()).capabilities(caps()),
    ));
    check_host_api(&backend(&app), &app, "ext", true);
}

#[test]
fn extism_commands() {
    let app = app(Builder::new().module("ext", ModuleConfig::from_path(extism())));
    let b = backend(&app);
    let info = b.module("ext").unwrap();
    assert_eq!(info.engine, EngineKind::Extism);
    let names: Vec<_> = info.commands.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"greet") && names.contains(&"count"),
        "{names:?}"
    );
    assert!(!names.contains(&"on_event"), "{names:?}");

    assert_eq!(
        b.invoke("ext", "greet", json!({ "name": "Extism" }))
            .unwrap(),
        json!("Hello, Extism!")
    );
    assert_eq!(b.invoke("ext", "count", Value::Null).unwrap(), json!(1));
    assert_eq!(b.invoke("ext", "count", Value::Null).unwrap(), json!(2));
    // camelCase names resolve to snake_case exports.
    assert_eq!(
        b.invoke("ext", "lastEvent", Value::Null).unwrap(),
        Value::Null
    );
    match b.invoke("ext", "fail", Value::Null) {
        Err(Error::Command(v)) => assert!(v.as_str().unwrap().contains("always fails"), "{v}"),
        other => panic!("expected command error, got {other:?}"),
    }
    assert!(matches!(
        b.invoke("ext", "nope", Value::Null),
        Err(Error::CommandNotFound { .. })
    ));
    // Traps are runtime errors, not command errors.
    assert!(matches!(
        b.invoke("ext", "crash", Value::Null),
        Err(Error::Runtime(_))
    ));
    assert_eq!(
        b.invoke("ext", "greet", json!({ "name": "again" }))
            .unwrap(),
        json!("Hello, again!")
    );

    // Event handlers aren't commands.
    assert!(matches!(
        b.invoke("ext", "on_event", Value::Null),
        Err(Error::CommandNotFound { .. })
    ));
}

#[test]
fn embedded_bytes_and_unload() {
    let bytes = std::fs::read(component()).unwrap();
    let app = app(Builder::new().module("embedded", ModuleConfig::from_bytes(bytes)));
    let b = backend(&app);
    assert_eq!(
        b.invoke("embedded", "add", json!({ "a": 1, "b": 2 }))
            .unwrap(),
        json!(3)
    );
    assert!(b.reload("embedded").is_err());
    b.unload("embedded").unwrap();
    assert!(matches!(
        b.invoke("embedded", "add", json!({ "a": 1, "b": 2 })),
        Err(Error::ModuleNotFound(_))
    ));
}

fn copy(from: &Path, to: &Path) {
    // Write to a temp file and rename, like build tools do.
    let tmp = to.with_extension("tmp");
    std::fs::copy(from, &tmp).unwrap();
    std::fs::rename(&tmp, to).unwrap();
}

#[test]
fn hot_reload() {
    let dir = temp_dir("hot-reload");
    let path = dir.join("hot.wasm");
    copy(&component(), &path);

    let app = app(Builder::new().module("hot", ModuleConfig::from_path(&path).watch(true)));
    let b = backend(&app);
    let (tx, rx) = mpsc::channel();
    app.listen_any(tauri_plugin_wasm_backend::EVENT_MODULE_RELOADED, move |e| {
        tx.send(e.payload().to_string()).unwrap();
    });
    assert_eq!(b.module("hot").unwrap().engine, EngineKind::Component);

    // Replacing the file swaps the module, even across engines.
    copy(&extism(), &path);
    let payload: Value = serde_json::from_str(&wait_for(&rx)).unwrap();
    assert_eq!(payload["generation"], json!(1));
    assert_eq!(payload["engine"], json!("extism"));
    assert_eq!(
        b.invoke("hot", "greet", json!({ "name": "reloaded" }))
            .unwrap(),
        json!("Hello, reloaded!")
    );
}

#[test]
fn modules_missing_at_startup_load_when_built() {
    let dir = temp_dir("pending");
    let path = dir.join("late.wasm");

    let app = app(Builder::new().module("late", ModuleConfig::from_path(&path).watch(true)));
    let b = backend(&app);
    let (tx, rx) = mpsc::channel();
    app.listen_any(tauri_plugin_wasm_backend::EVENT_MODULE_LOADED, move |e| {
        tx.send(e.payload().to_string()).unwrap();
    });
    assert!(matches!(b.module("late"), Err(Error::ModuleNotFound(_))));

    copy(&component(), &path);
    let payload: Value = serde_json::from_str(&wait_for(&rx)).unwrap();
    assert_eq!(payload["name"], json!("late"));
    assert_eq!(
        b.invoke("late", "add", json!({ "a": 2, "b": 2 })).unwrap(),
        json!(4)
    );
}

#[test]
fn external_plugins() {
    let dir = temp_dir("external");
    copy(&extism(), &dir.join("third-party.wasm"));

    let app = app(Builder::new().hot_reload(true).external(ExternalConfig {
        dir: dir.clone(),
        capabilities: Capabilities::default().allow_host_command("echo"),
        expose: false,
        engine: EngineKind::Auto,
    }));
    let b = backend(&app);
    let info = b.module("third-party").unwrap();
    assert!(info.external);
    assert!(!info.exposed);
    assert_eq!(
        b.invoke(
            "third-party",
            "call_host",
            json!({ "command": "echo", "args": 1 })
        )
        .unwrap(),
        json!({ "module": "third-party", "args": 1 })
    );
    // The external capability set is enforced.
    let err = b
        .invoke(
            "third-party",
            "emit_event",
            json!({ "event": "test://x", "payload": 1 }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("not allowed"), "{err}");

    // New plugins dropped into the directory are picked up, removed ones unloaded.
    let (tx, rx) = mpsc::channel();
    app.listen_any(tauri_plugin_wasm_backend::EVENT_MODULE_LOADED, move |e| {
        tx.send(e.payload().to_string()).unwrap();
    });
    copy(&component(), &dir.join("another.wasm"));
    let payload: Value = serde_json::from_str(&wait_for(&rx)).unwrap();
    assert_eq!(payload["name"], json!("another"));

    let (tx, rx) = mpsc::channel();
    app.listen_any(tauri_plugin_wasm_backend::EVENT_MODULE_UNLOADED, move |e| {
        tx.send(e.payload().to_string()).unwrap();
    });
    std::fs::remove_file(dir.join("third-party.wasm")).unwrap();
    assert_eq!(wait_for(&rx), r#""third-party""#);
    assert!(b.module("third-party").is_err());
}

#[test]
fn external_plugins_dir_created_at_startup_is_watched() {
    let dir = temp_dir("external-missing");
    std::fs::remove_dir_all(&dir).unwrap();

    let app = app(Builder::new().hot_reload(true).external(ExternalConfig {
        dir: dir.clone(),
        capabilities: Capabilities::default(),
        expose: false,
        engine: EngineKind::Auto,
    }));
    assert!(dir.is_dir());
    let (tx, rx) = mpsc::channel();
    app.listen_any(tauri_plugin_wasm_backend::EVENT_MODULE_LOADED, move |e| {
        tx.send(e.payload().to_string()).unwrap();
    });
    copy(&extism(), &dir.join("late-plugin.wasm"));
    let payload: Value = serde_json::from_str(&wait_for(&rx)).unwrap();
    assert_eq!(payload["name"], json!("late-plugin"));
}

#[test]
fn typescript_out_dir() {
    let dir = temp_dir("typescript");
    let app = app(Builder::new()
        .module("fixture", ModuleConfig::from_path(component()))
        .typescript_out_dir(&dir));
    let _ = app.state::<WasmBackend<MockRuntime>>();
    let ts = std::fs::read_to_string(dir.join("fixture.ts")).unwrap();
    assert!(ts.contains("export const fixture = {"), "{ts}");
}

/// Runs a JavaScript component built with componentize-js when
/// `WASM_BACKEND_JS_FIXTURE` points to one (see `examples/tauri-app`).
#[test]
fn javascript_component() {
    let Ok(path) = std::env::var("WASM_BACKEND_JS_FIXTURE") else {
        eprintln!("WASM_BACKEND_JS_FIXTURE not set, skipping");
        return;
    };
    let app = app(Builder::new().module(
        "greeter",
        ModuleConfig::from_path(path).capabilities(
            Capabilities::default()
                .allow_emit("greeter://*")
                .allow_host_command("echo"),
        ),
    ));
    let b = backend(&app);
    let out = b
        .invoke("greeter", "greet", json!({ "name": "JS" }))
        .unwrap();
    assert!(out.as_str().unwrap().contains("JS"), "{out}");
}

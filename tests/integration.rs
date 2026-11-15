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

fn typed() -> PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| fixture("typed-guest", "wasm32-wasip2"))
        .clone()
}

fn caps() -> Capabilities {
    Capabilities::default()
        .allow_emit("test://*")
        .allow_host_command("echo")
        .allow_host_command("app_version")
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
                .host_command("app_version", |_, _: ()| Ok::<_, String>("1.0.0"))
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

    // Host command names match across naming conventions, like module commands.
    assert_eq!(
        call_host("appVersion", json!(null)).unwrap(),
        json!("1.0.0")
    );
    assert_eq!(
        call_host("app-version", json!(null)).unwrap(),
        json!("1.0.0")
    );

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

/// Runs the example's JavaScript component (built with componentize-qjs by
/// `examples/tauri-app/scripts/build-wasm.mjs`) when
/// `WASM_BACKEND_JS_FIXTURE` points to it.
#[test]
fn javascript_component() {
    let Ok(path) = std::env::var("WASM_BACKEND_JS_FIXTURE") else {
        eprintln!("WASM_BACKEND_JS_FIXTURE not set, skipping");
        return;
    };
    let app = app(Builder::new());
    let b = backend(&app);
    let caps = Capabilities::default()
        .allow_emit("greeter://*")
        .listen("app://ping");
    // It imports the `app_version` host command, so it needs the capability.
    let err = b
        .load(
            "greeter",
            ModuleConfig::from_path(&path).capabilities(caps.clone()),
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not allowed to invoke host command `app_version`"),
        "{err}"
    );

    let info = b
        .load(
            "greeter",
            ModuleConfig::from_path(&path).capabilities(caps.allow_host_command("app_version")),
        )
        .unwrap();
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);
    let out = b
        .invoke("greeter", "greet", json!({ "name": "JS" }))
        .unwrap();
    assert!(out.as_str().unwrap().contains("JS"), "{out}");
    let out = b
        .invoke(
            "greeter",
            "describe",
            json!({ "person": { "name": "Ada", "ageYears": 36 } }),
        )
        .unwrap();
    assert_eq!(out, json!("Ada is 36 years old (app v1.0.0)"));
    match b.invoke(
        "greeter",
        "describe",
        json!({ "person": { "name": "", "ageYears": 1 } }),
    ) {
        Err(Error::Command(v)) => assert_eq!(v, json!("a person needs a name")),
        other => panic!("expected command error, got {other:?}"),
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Version {
    major: u32,
    minor: u32,
    pre_release: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddArgs {
    left_side: i32,
    right_side: i32,
}

/// Host commands backing every import of the typed fixture.
fn typed_host_commands(builder: Builder<MockRuntime>) -> Builder<MockRuntime> {
    builder
        .host_command("add_numbers", |_, args: AddArgs| {
            args.left_side
                .checked_add(args.right_side)
                .ok_or_else(|| "overflow".to_string())
        })
        .host_command("fail_hard", |_, args: Value| {
            Err::<(), _>(args["message"].as_str().unwrap_or_default().to_string())
        })
        .host_command("storage.get", |_, args: Value| {
            Ok::<_, String>((args["key"] == "known").then_some("value"))
        })
        .host_command("now_ms", |_, _: ()| Ok::<_, String>(1234u64))
}

fn typed_caps() -> Capabilities {
    Capabilities::default()
        .allow_host_command("app_version")
        .allow_host_command("add_numbers")
        .allow_host_command("fail_hard")
        .allow_host_command("storage.*")
        .allow_host_command("now_ms")
}

#[test]
fn typed_host_imports() {
    let app = tauri::test::mock_builder()
        .plugin(
            typed_host_commands(Builder::new())
                .host_command("app_version", |_, _: ()| {
                    Ok::<_, String>(Version {
                        major: 1,
                        minor: 2,
                        pre_release: Some("beta".into()),
                    })
                })
                .module(
                    "typed",
                    ModuleConfig::from_path(typed()).capabilities(typed_caps()),
                )
                .build(),
        )
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let b = backend(&app);

    // Records come back typed; no JSON strings on either side.
    assert_eq!(
        b.invoke("typed", "version", Value::Null).unwrap(),
        json!("1.2-beta")
    );
    // Arguments arrive keyed by camelCase parameter name; a host `Err`
    // becomes the `err` case of a `result` import.
    assert_eq!(
        b.invoke("typed", "add", json!({ "a": 2, "b": 40 }))
            .unwrap(),
        json!(42)
    );
    match b.invoke("typed", "add", json!({ "a": i32::MAX, "b": 1 })) {
        Err(Error::Command(v)) => assert_eq!(v, json!("overflow")),
        other => panic!("expected command error, got {other:?}"),
    }
    // `interface.function` host commands, and option results.
    assert_eq!(
        b.invoke("typed", "lookup", json!({ "key": "known" }))
            .unwrap(),
        json!("value")
    );
    assert_eq!(
        b.invoke("typed", "lookup", json!({ "key": "other" }))
            .unwrap(),
        Value::Null
    );
    // Without a `result` to put it in, a host error traps with its message.
    match b.invoke("typed", "fail-hard", json!({ "message": "boom" })) {
        Err(Error::Runtime(msg)) => {
            assert!(msg.starts_with("host command `fail_hard`"), "{msg}");
            assert!(msg.contains("failed: boom"), "{msg}");
        }
        other => panic!("expected runtime error, got {other:?}"),
    }

    // The imports are listed with their registered names and WIT types.
    let info = b.module("typed").unwrap();
    let mut imports: Vec<_> = info.imports.iter().map(|c| c.name.as_str()).collect();
    imports.sort();
    assert_eq!(
        imports,
        [
            "add_numbers",
            "app_version",
            "fail_hard",
            "now_ms",
            "storage.get"
        ],
        "{imports:?}"
    );
    let add = info
        .imports
        .iter()
        .find(|c| c.name == "add_numbers")
        .unwrap();
    assert_eq!(add.result.as_deref(), Some("result<s32, string>"));
    assert_eq!(add.params[0].name, "leftSide");
    // World-level function imports.
    assert_eq!(b.invoke("typed", "now", Value::Null).unwrap(), json!(1234));
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);
}

#[test]
fn typed_host_imports_are_checked_at_load() {
    let app = tauri::test::mock_builder()
        .plugin(
            Builder::new()
                .host_command("app_version", |_, _: ()| Ok::<_, String>("not a record"))
                .host_command("add_numbers", |_, _: Value| Ok::<_, String>(0))
                .build(),
        )
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let b = backend(&app);

    // Missing host commands and missing capabilities are all reported at
    // once, before anything runs.
    let err = b
        .load(
            "typed",
            ModuleConfig::from_path(typed())
                .capabilities(Capabilities::default().allow_host_command("app_version")),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("failed to load"), "{err}");
    assert!(
        err.contains("`test:typed/storage#get` needs a host command named `storage.get` or `get`"),
        "{err}"
    );
    assert!(
        err.contains("registered: add_numbers, app_version"),
        "{err}"
    );
    assert!(
        err.contains("not allowed to invoke host command `add_numbers` (add \"add_numbers\" to `capabilities.hostCommands`)"),
        "{err}"
    );
    assert!(err.contains("`test:typed/app#fail-hard`"), "{err}");
    assert!(matches!(b.module("typed"), Err(Error::ModuleNotFound(_))));
}

#[test]
fn typed_host_import_type_mismatch() {
    // `app_version` returns a string where the module imports a record.
    let app = tauri::test::mock_builder()
        .plugin(
            typed_host_commands(Builder::new())
                .host_command("app_version", |_, _: ()| Ok::<_, String>("1.2"))
                .module(
                    "typed",
                    ModuleConfig::from_path(typed()).capabilities(typed_caps()),
                )
                .build(),
        )
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let b = backend(&app);
    match b.invoke("typed", "version", Value::Null) {
        Err(Error::Runtime(msg)) => {
            assert!(
                msg.contains("host command `app_version` returned a value that doesn't match its import `test:typed/app#app-version`"),
                "{msg}"
            );
            assert!(msg.contains("expected an object"), "{msg}");
        }
        other => panic!("expected runtime error, got {other:?}"),
    }
    // Arguments the host command can't deserialize fail with serde's message.
    let app = tauri::test::mock_builder()
        .plugin(
            typed_host_commands(Builder::new())
                .host_command("app_version", |_, _: ()| Ok::<_, String>("1.2"))
                .host_command("add_numbers", |_, args: AddArgs| {
                    Ok::<_, String>(args.left_side)
                })
                .build(),
        )
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let b = backend(&app);
    b.load(
        "typed",
        ModuleConfig::from_path(typed()).capabilities(typed_caps()),
    )
    .unwrap();
    assert_eq!(
        b.invoke("typed", "add", json!({ "a": 5, "b": 1 })).unwrap(),
        json!(5)
    );
}

#[test]
fn configuration_warnings() {
    let app = app(typed_host_commands(Builder::new()));
    let b = backend(&app);

    // A capability naming a host command that doesn't exist (a typo, or a
    // command renamed in Rust) is flagged when the module loads.
    let info = b
        .load(
            "fixture",
            ModuleConfig::from_path(component()).capabilities(
                Capabilities::default()
                    .allow_host_command("echo")
                    .allow_host_command("app_verison")
                    .listen("app://ping"),
            ),
        )
        .unwrap();
    assert_eq!(info.warnings.len(), 1, "{:?}", info.warnings);
    assert!(
        info.warnings[0].contains("entry `app_verison` matches no registered host command (registered: `add_numbers`, `app_version`"),
        "{:?}",
        info.warnings
    );

    // So are subscriptions to events the module can't receive.
    let info = b
        .load(
            "typed",
            ModuleConfig::from_path(typed()).capabilities(typed_caps().listen("app://ping")),
        )
        .unwrap();
    assert_eq!(info.warnings.len(), 1, "{:?}", info.warnings);
    assert!(
        info.warnings[0].contains("subscribes to `app://ping` but the module has no event handler"),
        "{:?}",
        info.warnings
    );
    // Extism modules with an `on_event` export are fine.
    let info = b
        .load(
            "ext",
            ModuleConfig::from_path(extism()).capabilities(caps()),
        )
        .unwrap();
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);
}

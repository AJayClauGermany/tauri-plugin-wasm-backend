//! Runs real WASM modules on a minimal, non-Tauri platform, to make sure the
//! core works without any app framework.
//!
//! Uses the guest fixtures of the Tauri plugin (`../../tests/fixtures`),
//! which needs the `wasm32-wasip2` and `wasm32-unknown-unknown` targets.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use serde_json::{Value, json};
use wasm_backend_core::{
    Backend, BaseDir, Capabilities, EVENT_MODULE_LOADED, EVENT_MODULE_UNLOADED, EngineKind,
    EventBus, EventHandler, ModuleConfig, Platform, Result, TypeScriptOptions,
};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str, target: &str) -> PathBuf {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = repo().join("tests/fixtures").join(name);
    let target_dir = repo().join("target/fixtures");
    if dir.join("wit").exists() {
        let deps = dir.join("wit/deps/tauri-backend");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::write(deps.join("backend.wit"), wasm_backend_core::WIT).unwrap();
    }
    let status = Command::new(env!("CARGO"))
        .args(["build", "--release", "--target", target, "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .expect("failed to run cargo");
    assert!(status.success(), "failed to build fixture {name}");
    target_dir
        .join(target)
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")))
}

/// Records every emitted event and dispatches it on an in-process bus.
#[derive(Clone, Default)]
struct TestPlatform(Arc<State>);

#[derive(Default)]
struct State {
    bus: EventBus,
    emitted: Mutex<Vec<(String, Value)>>,
}

impl std::ops::Deref for TestPlatform {
    type Target = State;
    fn deref(&self) -> &State {
        &self.0
    }
}

impl Platform for TestPlatform {
    type ListenerId = u64;

    fn emit(&self, event: &str, payload: Value) -> std::result::Result<(), String> {
        self.emitted
            .lock()
            .unwrap()
            .push((event.to_string(), payload.clone()));
        self.bus.emit(event, &payload);
        Ok(())
    }

    fn listen(&self, event: &str, handler: EventHandler) -> u64 {
        self.bus.listen(event, handler)
    }

    fn unlisten(&self, id: u64) {
        self.bus.unlisten(id)
    }

    fn base_dir(&self, _: BaseDir) -> Result<PathBuf> {
        Ok(std::env::temp_dir())
    }
}

/// Minimal executor: the core's futures don't need a specific runtime.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Wake, Waker};
    struct ThreadWaker(std::thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

fn backend() -> (TestPlatform, Backend<TestPlatform>) {
    let platform = TestPlatform::default();
    let backend = Backend::builder(platform.clone())
        .hot_reload(false)
        .typescript_options(TypeScriptOptions {
            generator: "test".into(),
            api_module: "my-api".into(),
        })
        .host_command("echo", |call, args: Value| {
            Ok::<_, String>(json!({ "module": call.module, "args": args }))
        })
        .build()
        .unwrap();
    (platform, backend)
}

fn caps() -> Capabilities {
    Capabilities::default()
        .allow_emit("test://*")
        .allow_host_command("echo")
        .listen("app://ping")
}

fn wait_for_event(backend: &Backend<TestPlatform>, module: &str, command: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let last = backend.invoke(module, command, Value::Null).unwrap();
        if !last.is_null() {
            return last;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event was not delivered"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn extism_without_tauri() {
    let (platform, b) = backend();
    let path = fixture("extism-guest", "wasm32-unknown-unknown");
    b.load_all([(
        "ext".to_string(),
        ModuleConfig::from_path(path).capabilities(caps()),
    )]);
    let info = b.module("ext").unwrap();
    assert_eq!(info.engine, EngineKind::Extism);
    assert!(
        platform
            .emitted
            .lock()
            .unwrap()
            .iter()
            .any(|(e, p)| e == EVENT_MODULE_LOADED && p["name"] == "ext")
    );

    assert_eq!(
        b.invoke("ext", "greet", json!({ "name": "GPUI" })).unwrap(),
        json!("Hello, GPUI!")
    );

    // Async invokes run on the platform's blocking threads.
    assert_eq!(
        block_on(b.invoke_async("ext", "greet", json!({ "name": "async" }))).unwrap(),
        json!("Hello, async!")
    );
    assert!(matches!(
        block_on(b.invoke_async("nope", "greet", Value::Null)),
        Err(wasm_backend_core::Error::ModuleNotFound(_))
    ));

    // Events go through the platform.
    let (tx, rx) = mpsc::channel();
    platform
        .bus
        .listen("test://hello", Box::new(move |v| tx.send(v).unwrap()));
    b.invoke(
        "ext",
        "emit_event",
        json!({ "event": "test://hello", "payload": { "n": 1 } }),
    )
    .unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        json!({ "n": 1 })
    );

    // Host commands get the platform-independent context.
    assert_eq!(
        b.invoke(
            "ext",
            "call_host",
            json!({ "command": "echo", "args": [1] })
        )
        .unwrap(),
        json!({ "module": "ext", "args": [1] })
    );

    // Subscribed events are delivered off-thread.
    platform.emit("app://ping", json!("hi")).unwrap();
    assert_eq!(
        wait_for_event(&b, "ext", "last_event"),
        json!({ "event": "app://ping", "payload": "hi" })
    );

    let ts = b.typescript("ext").unwrap();
    assert!(
        ts.starts_with(
            "// Generated by test from module `ext`. Do not edit.\nimport { invokeModule } from 'my-api'\n"
        ),
        "{ts}"
    );

    b.unload("ext").unwrap();
    assert!(
        platform
            .emitted
            .lock()
            .unwrap()
            .iter()
            .any(|(e, p)| e == EVENT_MODULE_UNLOADED && p == "ext")
    );
}

#[test]
fn component_without_tauri() {
    let (platform, b) = backend();
    let path = fixture("component-guest", "wasm32-wasip2");
    b.load(
        "fixture",
        ModuleConfig::from_path(path).capabilities(caps()),
    )
    .unwrap();
    assert_eq!(
        b.invoke("fixture", "add", json!({ "a": 2, "b": 40 }))
            .unwrap(),
        json!(42)
    );
    platform.emit("app://ping", json!({ "x": 1 })).unwrap();
    assert_eq!(
        wait_for_event(&b, "fixture", "last-event"),
        json!(r#"app://ping:{"x":1}"#)
    );
}

//! Test fixture: a WIT component importing host commands as typed functions.

wit_bindgen::generate!({ world: "typed", generate_all });

use test::typed::{app, storage};

struct Typed;

impl Guest for Typed {
    fn version() -> String {
        let v = app::app_version();
        let pre = v.pre_release.map(|p| format!("-{p}")).unwrap_or_default();
        format!("{}.{}{pre}", v.major, v.minor)
    }

    fn add(a: i32, b: i32) -> Result<i32, String> {
        app::add_numbers(a, b)
    }

    fn lookup(key: String) -> Option<String> {
        storage::get(&key)
    }

    fn fail_hard(message: String) {
        app::fail_hard(&message)
    }

    fn now() -> u64 {
        now_ms()
    }
}

export!(Typed);

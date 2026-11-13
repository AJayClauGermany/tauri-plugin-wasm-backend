//! Test fixture: a WIT component exercising every part of the host API.

use std::cell::RefCell;

wit_bindgen::generate!({ world: "fixture", generate_all });

use exports::test::fixture::math;
use exports::tauri::backend::events;
use tauri::backend::host::{self, LogLevel};

struct Fixture;

thread_local! {
    static COUNT: RefCell<u32> = const { RefCell::new(0) };
    static LAST_EVENT: RefCell<Option<String>> = const { RefCell::new(None) };
}

impl Guest for Fixture {
    fn greet(name: String) -> String {
        host::log(LogLevel::Info, &format!("greeting {name}"));
        format!("Hello, {name}!")
    }

    fn add(a: i32, b: i32) -> i32 {
        a + b
    }

    fn describe(person: Person) -> Result<Person, String> {
        if person.age_years > 150 {
            return Err(format!("{} is too old", person.name));
        }
        Ok(Person {
            nickname: Some(person.nickname.unwrap_or_else(|| person.name.to_lowercase())),
            ..person
        })
    }

    fn area(shape: Shape) -> f64 {
        match shape {
            Shape::Circle(r) => std::f64::consts::PI * r * r,
            Shape::Square(s) => s * s,
            Shape::Point => 0.0,
        }
    }

    fn flip(mood: Mood) -> Mood {
        match mood {
            Mood::Happy => Mood::Sad,
            Mood::Sad => Mood::Happy,
        }
    }

    fn perms_of(p: Perms) -> Vec<String> {
        let mut out = Vec::new();
        if p.contains(Perms::READ) {
            out.push("read".to_string());
        }
        if p.contains(Perms::WRITE) {
            out.push("write".to_string());
        }
        out
    }

    fn swap(t: (u8, String)) -> (String, u8) {
        (t.1, t.0)
    }

    fn count() -> u32 {
        COUNT.with(|c| {
            *c.borrow_mut() += 1;
            *c.borrow()
        })
    }

    fn emit_event(name: String, payload: String) -> Result<(), String> {
        host::emit(&name, &payload)
    }

    fn call_host(command: String, args: String) -> Result<String, String> {
        host::invoke(&command, &args)
    }

    fn last_event() -> Option<String> {
        LAST_EVENT.with(|e| e.borrow().clone())
    }

    #[allow(clippy::empty_loop)]
    fn spin() {
        loop {}
    }

    fn crash() {
        panic!("crash requested");
    }
}

impl math::Guest for Fixture {
    fn double(x: i32) -> i32 {
        x * 2
    }
}

impl events::Guest for Fixture {
    fn on_event(event: String, payload: String) {
        LAST_EVENT.with(|e| *e.borrow_mut() = Some(format!("{event}:{payload}")));
    }
}

export!(Fixture);

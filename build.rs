const COMMANDS: &[&str] = &[
    "invoke",
    "list_modules",
    "list_inactive_modules",
    "reload_module",
    "enable_module",
    "disable_module",
    "typescript",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}

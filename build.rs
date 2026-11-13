const COMMANDS: &[&str] = &["invoke", "list_modules", "reload_module", "typescript"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}

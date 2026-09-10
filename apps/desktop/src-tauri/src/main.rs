// Prevents an extra console window on Windows in release builds. Linux is the
// only supported target today, but the attribute is harmless and keeps the
// upstream template shape.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    princesside_desktop_lib::run()
}

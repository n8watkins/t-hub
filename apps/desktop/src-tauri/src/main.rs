// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if let Some(code) = t_hub_lib::run_preview_static_helper(&args[1..]) {
        std::process::exit(code);
    }
    t_hub_lib::run();
}

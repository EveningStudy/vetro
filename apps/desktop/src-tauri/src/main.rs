#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    if let Err(err) = vetro_desktop_lib::run() {
        eprintln!("vetro-desktop: {err:#}");
        std::process::exit(1);
    }
}

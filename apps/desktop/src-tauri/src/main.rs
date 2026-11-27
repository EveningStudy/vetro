#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    if let Err(err) = verre_desktop_lib::run() {
        eprintln!("verre-desktop: {err:#}");
        std::process::exit(1);
    }
}

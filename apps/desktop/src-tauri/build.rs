fn main() {
    for path in [
        "tauri.conf.json",
        "icons",
        "icons/icon.ico",
        "icons/icon.png",
        "icons/32x32.png",
        "icons/64x64.png",
        "icons/128x128.png",
        "icons/128x128@2x.png",
        "icons/icon.icns",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    tauri_build::build()
}

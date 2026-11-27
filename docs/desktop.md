# Desktop shell (`apps/desktop`)

A Tauri 2 application named Verre (`io.github.eveningstudy.verre`). The shell owns the
window, the tray icon and single-instance behaviour; the Verre core serves the
UI over `http://127.0.0.1:<port>` and the window loads that URL. There is no
JavaScript frontend of its own — `frontend/` is only a placeholder page shown
before the core URL is known or when the core fails to start.

## Run in development

Run `cargo run -p verre-desktop` from the workspace root to start the
embedded core and the desktop window. To point the shell at an existing
development server, set `VERRE_DESKTOP_URL`:

```sh
VERRE_DESKTOP_URL=http://127.0.0.1:8080 cargo run -p verre-desktop
```

With the embedded core, turn on **Keep running when the window closes** in
**Settings → Remote access**. The setting takes effect immediately and is saved
as `closeToTray` in the Verre data directory's `config.json`.
The tray's **Open Verre** action restores the window, and **Quit** stops the core
and exits the application. On macOS, opening Verre again from the Dock also
restores its window while the core continues running. The window closes
normally in development URL mode.

Useful knobs while developing:

- `CARGO_TARGET_DIR=target/desktop` keeps this heavy dependency tree out of
  the default `target/` dir.
- Second launch of the binary focuses the existing window (single instance).
- `--new-instance`, `VERRE_MULTI_INSTANCE=1`, or a different executable name
  allows a separate window. Use a different `VERRE_HOME` for each embedded core.
- Links outside the core's origin open in the system browser.

## Build

From `apps/desktop` (the Tauri CLI finds `src-tauri/` automatically):

```sh
pnpm dlx @tauri-apps/cli build
```

or `cargo build --release -p verre-desktop` for just the binary without
bundling. Bundle targets come from the per-platform config files
(`tauri.windows.conf.json`, `tauri.macos.conf.json`, `tauri.linux.conf.json`):
NSIS on Windows, `.app` + `.dmg` on macOS, AppImage + deb on Linux.

### Windows

- MSVC toolchain (`rustup target add x86_64-pc-windows-msvc`) and the WebView2
  runtime (present on Windows 11; the NSIS installer bootstraps it).
- Output: `src-tauri/target/desktop/release/bundle/nsis/Verre_*.exe`.

### macOS

- `xcode-select --install` for the toolchain; nothing else is needed.
- Output: `bundle/macos/Verre.app` and `bundle/dmg/Verre_*.dmg`.

### Linux

Debian/Ubuntu packages needed before building:

```sh
sudo apt update
sudo apt install -y \
  build-essential curl wget file libssl-dev \
  libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
  libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev \
  patchelf
```

`patchelf` is used by the AppImage bundler; `fuse`/`libfuse2` is needed to run
the resulting AppImage on distros without FUSE 3.

The Linux release workflow builds on Ubuntu 24.04 and launches the resulting AppImage before uploading it. To check the native window, Python ACP handshake, default file opener, and listener shutdown, run:

```sh
APPIMAGE_EXTRACT_AND_RUN=1 xvfb-run -a node .github/scripts/smoke-appimage.mjs path/to/Verre.AppImage
```

## Icons

`apps/desktop/icon.svg` is the source. Regenerate the set after changing it:

```sh
pnpm dlx @resvg/resvg-js-cli --fit-width 1024 icon.svg icon.png
pnpm dlx @tauri-apps/cli icon icon.png -o src-tauri/icons
```

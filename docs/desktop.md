# Desktop shell (`apps/desktop`)

A Tauri 2 application named Vetro (`dev.vetro.desktop`). The shell owns the
window, the tray icon and single-instance behaviour; the Vetro core serves the
UI over `http://127.0.0.1:<port>` and the window loads that URL. There is no
JavaScript frontend of its own — `frontend/` is only a placeholder page shown
before the core URL is known or when the core fails to start.

## Run in development

Start the core (or any static HTTP server) first, then point the shell at it:

```sh
VETRO_DESKTOP_URL=http://127.0.0.1:8080 cargo run -p vetro-desktop
```

`core::start()` currently reads that URL from `VETRO_DESKTOP_URL` and fails
with a visible error page when it is unset; the orchestrator replaces the body
with the real core startup. `VETRO_DESKTOP_CLOSE_TO_TRAY=1` additionally
enables close-to-tray.

Useful knobs while developing:

- `CARGO_TARGET_DIR=target/desktop` keeps this heavy dependency tree out of
  the default `target/` dir.
- Second launch of the binary focuses the existing window (single instance).
- Links outside the core's origin open in the system browser.

## Build

From `apps/desktop` (the Tauri CLI finds `src-tauri/` automatically):

```sh
pnpm dlx @tauri-apps/cli build
```

or `cargo build --release -p vetro-desktop` for just the binary without
bundling. Bundle targets come from the per-platform config files
(`tauri.windows.conf.json`, `tauri.macos.conf.json`, `tauri.linux.conf.json`):
NSIS on Windows, `.app` + `.dmg` on macOS, AppImage + deb on Linux.

### Windows

- MSVC toolchain (`rustup target add x86_64-pc-windows-msvc`) and the WebView2
  runtime (present on Windows 11; the NSIS installer bootstraps it).
- Output: `src-tauri/target/desktop/release/bundle/nsis/Vetro_*.exe`.

### macOS

- `xcode-select --install` for the toolchain; nothing else is needed.
- Output: `bundle/macos/Vetro.app` and `bundle/dmg/Vetro_*.dmg`.

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

## Icons

`apps/desktop/icon.svg` is the source. Regenerate the set after changing it:

```sh
pnpm dlx @resvg/resvg-js-cli --fit-width 1024 icon.svg icon.png
pnpm dlx @tauri-apps/cli icon icon.png -o src-tauri/icons
```

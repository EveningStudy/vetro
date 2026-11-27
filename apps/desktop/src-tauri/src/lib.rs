mod core;
mod updater;

use std::error::Error;
use std::ffi::OsStr;
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

#[cfg(any(target_os = "windows", test))]
use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::NewWindowResponse;
use tauri::window::Color;
use tauri::{
    AppHandle, Manager, RunEvent, Theme, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent, Wry,
};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::broadcast::error::RecvError;
use verre_core::{Core, WindowTheme};

use crate::core::{CoreHandle, ShutdownHandle};

const MAIN_WINDOW: &str = "main";

pub(crate) struct CoreShutdown(pub(crate) Mutex<Option<ShutdownHandle>>);

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<()> {
    let tag = side_by_side_tag();
    let mut builder = tauri::Builder::default();
    if tag.is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }));
    }
    let app = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            let tag = tag.as_deref();
            let handle_app = app.handle().clone();
            let core = tauri::async_runtime::block_on(crate::core::start());
            let (language_rx, lang) = match core {
                Ok(mut handle) => {
                    if let Some(core_arc) = handle.embedded_core.upgrade() {
                        core_arc.set_updater(Arc::new(crate::updater::DesktopUpdater::new(
                            handle_app.clone(),
                            Arc::downgrade(&core_arc),
                        )));
                    }
                    app.manage(CoreShutdown(Mutex::new(handle.shutdown.take())));
                    let language_rx = handle.language_rx.clone();
                    let initial_lang = language_rx.as_ref().and_then(|rx| rx.borrow().clone());
                    let lang = resolve_desktop_lang(initial_lang.as_deref());
                    core_window(&handle_app, handle, tag)?;
                    (language_rx, lang)
                }
                Err(err) => {
                    let lang = resolve_desktop_lang(None);
                    error_window(&handle_app, &format!("{err:#}"), tag, lang)?;
                    (None, lang)
                }
            };
            let (open_item, quit_item) = build_tray(&handle_app, tag, lang)?;
            if let Some(mut language_rx) = language_rx {
                let tag = tag.map(str::to_owned);
                tauri::async_runtime::spawn(async move {
                    while language_rx.changed().await.is_ok() {
                        let pref = language_rx.borrow_and_update().clone();
                        let lang = resolve_desktop_lang(pref.as_deref());
                        let (open_text, quit_text) = tray_labels(lang, tag.as_deref());
                        let _ = open_item.set_text(open_text);
                        let _ = quit_item.set_text(quit_text);
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())?;
    app.run(|app, event| match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => show_main_window(app),
        RunEvent::ExitRequested { api, code, .. } => {
            if let Some(state) = app.try_state::<CoreShutdown>() {
                if let Some(shutdown) = state.0.lock().unwrap().take() {
                    api.prevent_exit();
                    let exit_app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_secs(3), shutdown.stop()).await;
                        exit_app.exit(code.unwrap_or(0));
                    });
                }
            }
        }
        _ => {}
    });
    Ok(())
}

fn side_by_side_tag() -> Option<String> {
    let requested = std::env::args_os()
        .skip(1)
        .any(|arg| arg == "--new-instance");
    let configured = std::env::var_os("VERRE_MULTI_INSTANCE");
    let executable = std::env::current_exe().ok();
    instance_tag(
        requested,
        configured.as_deref(),
        executable.as_deref().and_then(std::path::Path::file_stem),
    )
}

fn instance_tag(
    requested: bool,
    configured: Option<&OsStr>,
    stem: Option<&OsStr>,
) -> Option<String> {
    if requested || configured.is_some_and(|value| !value.is_empty() && value != "0") {
        return Some(format!("pid-{}", std::process::id()));
    }
    let stem = stem?.to_string_lossy();
    if stem.eq_ignore_ascii_case("verre-desktop") {
        return None;
    }
    Some(
        stem.chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                    ch
                } else {
                    '-'
                }
            })
            .collect(),
    )
}

fn instance_title(base: &str, tag: Option<&str>) -> String {
    let Some(tag) = tag else {
        return base.into();
    };
    let label = tag
        .strip_prefix("verre-desktop-")
        .or_else(|| tag.strip_prefix("verre-"))
        .filter(|label| !label.is_empty())
        .unwrap_or(tag);
    format!("{base} ({label})")
}

fn core_window(
    app: &AppHandle,
    mut handle: CoreHandle,
    tag: Option<&str>,
) -> Result<WebviewWindow> {
    let url = Url::parse(&handle.url)?;
    let origin = url.origin();
    #[cfg(target_os = "windows")]
    let allowed_mic_origin = origin.ascii_serialization();
    let initial_theme = handle
        .theme_rx
        .as_mut()
        .map(|theme| theme.borrow_and_update().clone())
        .unwrap_or_default();
    let builder = window_builder(app, WebviewUrl::External(url), tag)
        .theme(native_theme(initial_theme.theme.as_deref()));
    let builder = if let Some(background) = window_background(&initial_theme) {
        builder.background_color(background)
    } else {
        builder
    };
    #[cfg(target_os = "windows")]
    let (builder, platform, chrome) = (
        builder.decorations(false).shadow(true).transparent(false),
        "windows",
        "custom",
    );
    #[cfg(target_os = "macos")]
    let (builder, platform, chrome) = (
        builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true),
        "macos",
        "overlay",
    );
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let (platform, chrome) = (std::env::consts::OS, "native");

    let embedded_core = handle.embedded_core.strong_count() > 0;
    let window = builder
        .initialization_script(format!(
            r#"window.__VERRE_DESKTOP__ = {{ platform: "{platform}", chrome: "{chrome}", maximized: false, embeddedCore: {embedded_core} }};"#
        ))
        .on_navigation({
            let app = app.clone();
            let origin = origin.clone();
            move |target| {
                if target.origin() == origin || target.as_str() == "about:blank" {
                    return true;
                }
                open_external(&app, target);
                false
            }
        })
        .on_new_window({
            let app = app.clone();
            move |target, _features| {
                if target.origin() != origin {
                    open_external(&app, &target);
                }
                NewWindowResponse::Deny
            }
        })
        .build()?;
    watch_window_close(&window, handle.embedded_core.clone());
    #[cfg(target_os = "windows")]
    {
        watch_window_maximized(&window);
        allow_loopback_microphone(&window, allowed_mic_origin);
    }
    if let Some(mut theme) = handle.theme_rx {
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            while theme.changed().await.is_ok() {
                let (mapped, background) = {
                    let theme = theme.borrow_and_update();
                    (
                        native_theme(theme.theme.as_deref()),
                        window_background(&theme),
                    )
                };
                let _ = window.set_theme(mapped);
                let _ = window.set_background_color(background);
            }
        });
    }
    if let Some(mut badge) = handle.badge_rx {
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            while badge.changed().await.is_ok() {
                let count = *badge.borrow_and_update();
                update_window_badge(&window, count);
            }
        });
    }
    if let Some(mut notifications) = handle.notify_rx {
        let app_for_action = app.clone();
        let core_for_action = handle.embedded_core;
        let _ = app.notification().on_action(move |performed| {
            show_main_window(&app_for_action);
            if let Some(thread_id) = performed
                .notification()
                .and_then(|data| data.extra().get("threadId"))
                .and_then(|value| value.as_str())
            {
                if let Some(core) = core_for_action.upgrade() {
                    core.emit_navigate(thread_id.to_owned());
                }
            }
        });
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                match notifications.recv().await {
                    Ok(params) => {
                        let mut builder = app
                            .notification()
                            .builder()
                            .title(params.title)
                            .body(params.body);
                        if let Some(thread_id) = params.thread_id {
                            builder = builder.extra("threadId", thread_id);
                        }
                        if let Err(error) = builder.show() {
                            eprintln!("Could not show desktop notification: {error}");
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }
    Ok(window)
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let [_, red, green, blue] = u32::from_str_radix(hex, 16).ok()?.to_be_bytes();
    Some(Color(red, green, blue, 255))
}

fn window_background(theme: &WindowTheme) -> Option<Color> {
    theme
        .background
        .as_deref()
        .and_then(parse_hex_color)
        .or(match theme.theme.as_deref() {
            Some("dark") => Some(Color(10, 10, 10, 255)),
            Some("light") => Some(Color(252, 252, 252, 255)),
            _ => None,
        })
}

fn native_theme(pref: Option<&str>) -> Option<Theme> {
    match pref {
        Some("dark") => Some(Theme::Dark),
        Some("light") => Some(Theme::Light),
        _ => None,
    }
}

fn error_window(
    app: &AppHandle,
    message: &str,
    tag: Option<&str>,
    lang: &str,
) -> Result<WebviewWindow> {
    let script = format!(
        "window.__VERRE_ERROR__ = {}; window.__VERRE_LANG__ = {};",
        serde_json::to_string(message)?,
        serde_json::to_string(lang)?,
    );
    let window = window_builder(app, WebviewUrl::App("error.html".into()), tag)
        .initialization_script(script)
        .build()?;
    Ok(window)
}

fn window_builder<'app>(
    app: &'app AppHandle,
    url: WebviewUrl,
    tag: Option<&str>,
) -> WebviewWindowBuilder<'app, Wry, AppHandle> {
    let builder = WebviewWindowBuilder::new(app, MAIN_WINDOW, url)
        .title(instance_title("Verre", tag))
        .inner_size(1280.0, 820.0)
        .min_inner_size(900.0, 600.0)
        .center();
    #[cfg(target_os = "macos")]
    {
        builder.incognito(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(dir) = verre_home(app) {
            let data = match tag {
                Some(tag) => dir.join("webview-instances").join(tag),
                None => dir.join("webview"),
            };
            return builder.data_directory(data);
        }
        builder
    }
}

#[cfg(not(target_os = "macos"))]
fn verre_home(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("VERRE_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    app.path().home_dir().ok().map(|home| home.join(".verre"))
}

fn watch_window_close(window: &WebviewWindow, core: Weak<Core>) {
    let w = window.clone();
    window.clone().on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            if core.upgrade().is_some_and(|core| core.home.close_to_tray()) {
                api.prevent_close();
                let _ = w.hide();
            }
        }
    });
}

#[cfg(target_os = "windows")]
fn allow_loopback_microphone(window: &WebviewWindow, allowed_origin: String) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE, COREWEBVIEW2_PERMISSION_KIND_UNKNOWN_PERMISSION,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::{take_pwstr, PermissionRequestedEventHandler};
    use windows_core::PWSTR;

    let _ = window.with_webview(move |webview| unsafe {
        let Ok(core) = webview.controller().CoreWebView2() else {
            return;
        };
        let mut token = 0i64;
        let handler = PermissionRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else {
                return Ok(());
            };
            let mut kind = COREWEBVIEW2_PERMISSION_KIND_UNKNOWN_PERMISSION;
            if args.PermissionKind(&mut kind).is_ok()
                && kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
            {
                let mut raw_uri = PWSTR::null();
                if args.Uri(&mut raw_uri).is_ok() {
                    let uri = take_pwstr(raw_uri);
                    if Url::parse(&uri)
                        .is_ok_and(|parsed| parsed.origin().ascii_serialization() == allowed_origin)
                    {
                        let _ = args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW);
                    }
                }
            }
            Ok(())
        }));
        let _ = core.add_PermissionRequested(&handler, &mut token);
    });
}

#[cfg(target_os = "windows")]
fn watch_window_maximized(window: &WebviewWindow) {
    let maximized = AtomicBool::new(false);
    let target = window.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Resized(_) | WindowEvent::Moved(_)) {
            let current = target.is_maximized().unwrap_or(false);
            if maximized.swap(current, Ordering::Relaxed) != current {
                let _ = target.eval(format!(
                    r#"if (window.__VERRE_DESKTOP__) {{ window.__VERRE_DESKTOP__.maximized = {current}; }}
window.dispatchEvent(new CustomEvent("verre:window-state", {{ detail: {{ maximized: {current} }} }}));"#
                ));
            }
        }
    });
}

fn resolve_desktop_lang(pref: Option<&str>) -> &'static str {
    match pref {
        Some("zh") => "zh",
        Some("en") => "en",
        _ => system_lang(),
    }
}

fn system_lang() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        unsafe extern "system" {
            fn GetUserDefaultLocaleName(lp_locale_name: *mut u16, cch_locale_name: i32) -> i32;
        }
        let mut buf = [0u16; 85];
        let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if len > 1 {
            let locale = String::from_utf16_lossy(&buf[..(len as usize - 1)]);
            if locale.to_ascii_lowercase().starts_with("zh") {
                return "zh";
            }
        }
        "en"
    }
    #[cfg(not(target_os = "windows"))]
    {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(key) {
                if !value.is_empty() {
                    return if value.to_ascii_lowercase().starts_with("zh") {
                        "zh"
                    } else {
                        "en"
                    };
                }
            }
        }
        "en"
    }
}

fn tray_labels(lang: &str, tag: Option<&str>) -> (String, String) {
    if lang == "zh" {
        (instance_title("打开 Verre", tag), "退出".into())
    } else {
        (instance_title("Open Verre", tag), "Quit".into())
    }
}

fn update_window_badge(window: &WebviewWindow, count: u32) {
    #[cfg(target_os = "windows")]
    {
        let _ = window.set_overlay_icon(render_badge_icon(count));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window.set_badge_count((count > 0).then(|| i64::from(count)));
    }
}

#[cfg(any(target_os = "windows", test))]
fn render_badge_icon(count: u32) -> Option<Image<'static>> {
    if count == 0 {
        return None;
    }
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let center = (SIZE as f32 - 1.0) / 2.0;
    let radius = 14.5_f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius + 0.5 {
                let alpha = (radius + 0.5 - dist).clamp(0.0, 1.0);
                let border = dist > radius - 1.0;
                let (r, g, b) = if border { (164, 34, 40) } else { (229, 72, 77) };
                let idx = (y * SIZE + x) * 4;
                rgba[idx] = r;
                rgba[idx + 1] = g;
                rgba[idx + 2] = b;
                rgba[idx + 3] = (alpha * 255.0).round() as u8;
            }
        }
    }
    let paint = |rgba: &mut [u8], px: usize, py: usize| {
        if px < SIZE && py < SIZE {
            let idx = (py * SIZE + px) * 4;
            rgba[idx] = 255;
            rgba[idx + 1] = 255;
            rgba[idx + 2] = 255;
            rgba[idx + 3] = 255;
        }
    };
    let draw_glyph = |rgba: &mut [u8], ch: char, ox: usize, oy: usize| {
        let rows: [u8; 7] = match ch {
            '1' => [
                0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
            ],
            '2' => [
                0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111,
            ],
            '3' => [
                0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110,
            ],
            '4' => [
                0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
            ],
            '5' => [
                0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
            ],
            '6' => [
                0b01110, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b01110,
            ],
            '7' => [
                0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
            ],
            '8' => [
                0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
            ],
            '9' => [
                0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
            ],
            '+' => [
                0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
            ],
            _ => [0; 7],
        };
        for (ry, bits) in rows.iter().enumerate() {
            for rx in 0..5 {
                if (bits >> (4 - rx)) & 1 == 1 {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            paint(rgba, ox + rx * 2 + dx, oy + ry * 2 + dy);
                        }
                    }
                }
            }
        }
    };
    if count > 9 {
        draw_glyph(&mut rgba, '9', 6, 9);
        draw_glyph(&mut rgba, '+', 17, 9);
    } else {
        let ch = char::from_digit(count, 10).unwrap_or('9');
        draw_glyph(&mut rgba, ch, 11, 9);
    }
    Some(Image::new_owned(rgba, SIZE as u32, SIZE as u32))
}

fn build_tray(
    app: &AppHandle,
    tag: Option<&str>,
    lang: &str,
) -> Result<(MenuItem<Wry>, MenuItem<Wry>)> {
    let (open_text, quit_text) = tray_labels(lang, tag);
    let open = MenuItemBuilder::with_id("open", open_text).build(app)?;
    let quit = MenuItemBuilder::with_id("quit", quit_text).build(app)?;
    let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;

    let mut builder = TrayIconBuilder::with_id("tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok((open, quit))
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn open_external(app: &AppHandle, url: &Url) {
    if matches!(url.scheme(), "javascript" | "data" | "vbscript" | "file") {
        return;
    }
    let _ = app.opener().open_url(url.as_str(), None::<&str>);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_tags_and_titles_distinguish_parallel_builds() {
        let normal = Some(OsStr::new("VeRrE-DeSkToP"));
        let pid_tag = format!("pid-{}", std::process::id());
        for configured in [None, Some(OsStr::new("")), Some(OsStr::new("0"))] {
            assert_eq!(instance_tag(false, configured, normal), None);
            assert_eq!(
                instance_tag(true, configured, normal).as_deref(),
                Some(pid_tag.as_str())
            );
        }
        assert_eq!(
            instance_tag(false, Some(OsStr::new("1")), normal).as_deref(),
            Some(pid_tag.as_str())
        );
        let tag = instance_tag(false, None, Some(OsStr::new("verre-desktop-new build!_2")));
        assert_eq!(tag.as_deref(), Some("verre-desktop-new-build-_2"));
        assert_eq!(
            instance_title("Verre", tag.as_deref()),
            "Verre (new-build-_2)"
        );
        assert_eq!(
            instance_title("Open Verre", Some("verre-dev")),
            "Open Verre (dev)"
        );
        assert_eq!(instance_title("Verre", Some("preview")), "Verre (preview)");
        assert_eq!(instance_title("Verre", None), "Verre");
        assert_eq!(instance_title("Open Verre", None), "Open Verre");
    }

    #[test]
    fn tray_labels_and_badge_icon_match_language_and_counts() {
        assert_eq!(resolve_desktop_lang(Some("zh")), "zh");
        assert_eq!(resolve_desktop_lang(Some("en")), "en");
        assert_eq!(
            tray_labels("zh", Some("verre-dev")),
            ("打开 Verre (dev)".into(), "退出".into())
        );
        assert_eq!(
            tray_labels("en", None),
            ("Open Verre".into(), "Quit".into())
        );
        assert!(render_badge_icon(0).is_none());
        let single = render_badge_icon(3).unwrap();
        assert_eq!((single.width(), single.height()), (32, 32));
        assert_eq!(single.rgba().len(), 32 * 32 * 4);
        assert!(single.rgba().as_chunks::<4>().0.iter().any(|px| px[3] > 0));
        let overflow = render_badge_icon(14).unwrap();
        assert_eq!(overflow.rgba().len(), 32 * 32 * 4);
        assert!(overflow
            .rgba()
            .as_chunks::<4>()
            .0
            .contains(&[255, 255, 255, 255]));
    }
}

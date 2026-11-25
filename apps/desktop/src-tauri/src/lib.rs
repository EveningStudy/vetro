mod core;

use std::error::Error;
use std::ffi::OsStr;
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
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
use vetro_core::WindowTheme;

use crate::core::{CoreHandle, ShutdownHandle};

const MAIN_WINDOW: &str = "main";

struct CoreShutdown(Mutex<Option<ShutdownHandle>>);

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
        .setup(move |app| {
            let tag = tag.as_deref();
            let handle_app = app.handle().clone();
            let core = tauri::async_runtime::block_on(crate::core::start());
            let close_to_tray = core
                .as_ref()
                .map(|handle| handle.close_to_tray)
                .unwrap_or(false);
            let window = match core {
                Ok(mut handle) => {
                    app.manage(CoreShutdown(Mutex::new(handle.shutdown.take())));
                    core_window(&handle_app, handle, tag)?
                }
                Err(err) => error_window(&handle_app, &format!("{err:#}"), tag)?,
            };
            watch_window_close(&window, close_to_tray);
            build_tray(&handle_app, tag)?;
            Ok(())
        })
        .build(tauri::generate_context!())?;
    app.run(|app, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
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
    });
    Ok(())
}

fn side_by_side_tag() -> Option<String> {
    let requested = std::env::args_os()
        .skip(1)
        .any(|arg| arg == "--new-instance");
    let configured = std::env::var_os("VETRO_MULTI_INSTANCE");
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
    if stem.eq_ignore_ascii_case("vetro-desktop") {
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
        .strip_prefix("vetro-desktop-")
        .or_else(|| tag.strip_prefix("vetro-"))
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

    let window = builder
        .initialization_script(format!(
            r#"window.__VETRO_DESKTOP__ = {{ platform: "{platform}", chrome: "{chrome}", maximized: false }};"#
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
    if let Some(mut notifications) = handle.notify_rx {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                match notifications.recv().await {
                    Ok(params) => {
                        let result = app
                            .notification()
                            .builder()
                            .title(params.title)
                            .body(params.body)
                            .show();
                        if let Err(error) = result {
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

fn error_window(app: &AppHandle, message: &str, tag: Option<&str>) -> Result<WebviewWindow> {
    let script = format!(
        "window.__VETRO_ERROR__ = {};",
        serde_json::to_string(message)?
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
        .title(instance_title("Vetro", tag))
        .inner_size(1280.0, 820.0)
        .min_inner_size(900.0, 600.0)
        .center();
    #[cfg(target_os = "macos")]
    {
        builder.incognito(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(dir) = vetro_home(app) {
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
fn vetro_home(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("VETRO_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    app.path().home_dir().ok().map(|home| home.join(".vetro"))
}

fn watch_window_close(window: &WebviewWindow, close_to_tray: bool) {
    let w = window.clone();
    window.clone().on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            if close_to_tray {
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
                    r#"if (window.__VETRO_DESKTOP__) {{ window.__VETRO_DESKTOP__.maximized = {current}; }}
window.dispatchEvent(new CustomEvent("vetro:window-state", {{ detail: {{ maximized: {current} }} }}));"#
                ));
            }
        }
    });
}

fn build_tray(app: &AppHandle, tag: Option<&str>) -> Result<()> {
    let open = MenuItemBuilder::with_id("open", instance_title("Open Vetro", tag)).build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
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
    Ok(())
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
        let normal = Some(OsStr::new("VeTrO-DeSkToP"));
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
        let tag = instance_tag(false, None, Some(OsStr::new("vetro-desktop-new build!_2")));
        assert_eq!(tag.as_deref(), Some("vetro-desktop-new-build-_2"));
        assert_eq!(
            instance_title("Vetro", tag.as_deref()),
            "Vetro (new-build-_2)"
        );
        assert_eq!(
            instance_title("Open Vetro", Some("vetro-dev")),
            "Open Vetro (dev)"
        );
        assert_eq!(instance_title("Vetro", Some("preview")), "Vetro (preview)");
        assert_eq!(instance_title("Vetro", None), "Vetro");
        assert_eq!(instance_title("Open Vetro", None), "Open Vetro");
    }
}

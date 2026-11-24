mod core;

use std::error::Error;
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::NewWindowResponse;
use tauri::{
    AppHandle, Manager, Theme, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
    Wry,
};
use tauri_plugin_opener::OpenerExt;

use crate::core::CoreHandle;

const MAIN_WINDOW: &str = "main";

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app = app.handle().clone();
            let core = tauri::async_runtime::block_on(crate::core::start());
            let close_to_tray = core
                .as_ref()
                .map(|handle| handle.close_to_tray)
                .unwrap_or(false);
            let window = match core {
                Ok(handle) => core_window(&app, handle)?,
                Err(err) => error_window(&app, &format!("{err:#}"))?,
            };
            watch_window_close(&window, close_to_tray);
            build_tray(&app)?;
            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

fn core_window(app: &AppHandle, mut handle: CoreHandle) -> Result<WebviewWindow> {
    let url = Url::parse(&handle.url)?;
    let origin = url.origin();
    let initial_theme = handle
        .theme_rx
        .as_mut()
        .and_then(|theme| native_theme(theme.borrow_and_update().as_deref()));
    let builder = window_builder(app, WebviewUrl::External(url)).theme(initial_theme);
    let builder = if initial_theme == Some(Theme::Dark) {
        builder.background_color(tauri::window::Color(9, 9, 11, 255))
    } else {
        builder
    };
    let window = builder
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
    if let Some(mut theme) = handle.theme_rx {
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            while theme.changed().await.is_ok() {
                let mapped = native_theme(theme.borrow_and_update().as_deref());
                let _ = window.set_theme(mapped);
            }
        });
    }
    Ok(window)
}

fn native_theme(pref: Option<&str>) -> Option<Theme> {
    match pref {
        Some("dark") => Some(Theme::Dark),
        Some("light") => Some(Theme::Light),
        _ => None,
    }
}

fn error_window(app: &AppHandle, message: &str) -> Result<WebviewWindow> {
    let script = format!(
        "window.__VETRO_ERROR__ = {};",
        serde_json::to_string(message)?
    );
    let window = window_builder(app, WebviewUrl::App("error.html".into()))
        .initialization_script(script)
        .build()?;
    Ok(window)
}

fn window_builder(app: &AppHandle, url: WebviewUrl) -> WebviewWindowBuilder<'_, Wry, AppHandle> {
    let builder = WebviewWindowBuilder::new(app, MAIN_WINDOW, url)
        .title("Vetro")
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
            return builder.data_directory(dir.join("webview"));
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

fn build_tray(app: &AppHandle) -> Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open Vetro").build(app)?;
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

use anyhow::{anyhow, Context};
use vetro_core::{home::Home, registry::Registry, server::Server, Core};

/// Handle for the running Vetro core. The shell only needs the URL to point
/// the window at; everything else is internal to the core.
pub struct CoreHandle {
    pub url: String,
    pub close_to_tray: bool,
}

/// Starts the core inside this process and returns its loopback URL.
/// `VETRO_DESKTOP_URL` points the window at an already running core instead (development).
pub async fn start() -> anyhow::Result<CoreHandle> {
    if let Ok(url) = std::env::var("VETRO_DESKTOP_URL") {
        let parsed = tauri::Url::parse(&url)
            .with_context(|| format!("VETRO_DESKTOP_URL is not a valid URL: {url:?}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            anyhow::bail!("VETRO_DESKTOP_URL must be an http(s) URL, got {url:?}");
        }
        return Ok(CoreHandle {
            url,
            close_to_tray: false,
        });
    }
    let home = Home::discover().map_err(|error| anyhow!("{error:?}"))?;
    let settings = home.settings();
    let registry = Registry::from_settings(&settings).map_err(|error| anyhow!("{error:?}"))?;
    let core = Core::new(home, registry);
    let server = Server::bind(core, None)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    let url = server.local_url();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = server.serve(std::future::pending()).await {
            eprintln!("vetro core stopped: {error}");
        }
    });
    Ok(CoreHandle {
        url,
        close_to_tray: settings.close_to_tray,
    })
}

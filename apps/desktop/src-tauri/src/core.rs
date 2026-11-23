use anyhow::Context;

/// Handle for the running Vetro core. The shell only needs the URL to point
/// the window at; everything else is internal to the core.
pub struct CoreHandle {
    pub url: String,
    pub close_to_tray: bool,
}

// Stub: the real body will start `vetro-core` (which serves the UI on
// http://127.0.0.1:<port>) and report its URL. For now the URL comes from the
// environment so the shell can be built and run on its own.
pub async fn start() -> anyhow::Result<CoreHandle> {
    let url = std::env::var("VETRO_DESKTOP_URL").map_err(|_| {
        anyhow::anyhow!("VETRO_DESKTOP_URL is not set; cannot determine the vetro-core URL")
    })?;
    let parsed = tauri::Url::parse(&url)
        .with_context(|| format!("VETRO_DESKTOP_URL is not a valid URL: {url:?}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("VETRO_DESKTOP_URL must be an http(s) URL, got {url:?}");
    }
    Ok(CoreHandle {
        url,
        close_to_tray: std::env::var("VETRO_DESKTOP_CLOSE_TO_TRAY")
            .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes")),
    })
}

use std::sync::{Arc, Weak};

use anyhow::{anyhow, Context};
use tokio::sync::{broadcast, oneshot, watch};
use vetro_core::{home::Home, registry::Registry, server::Server, Core, WindowTheme};
use vetro_proto::rpc::NotifyParams;

pub struct ShutdownHandle {
    trigger: oneshot::Sender<()>,
    done: oneshot::Receiver<()>,
}

impl ShutdownHandle {
    pub async fn stop(self) {
        let _ = self.trigger.send(());
        let _ = self.done.await;
    }
}

/// Handle for the running Vetro core. The shell only needs the URL to point
/// the window at; everything else is internal to the core.
pub struct CoreHandle {
    pub url: String,
    pub embedded_core: Weak<Core>,
    pub theme_rx: Option<watch::Receiver<WindowTheme>>,
    pub notify_rx: Option<broadcast::Receiver<NotifyParams>>,
    pub shutdown: Option<ShutdownHandle>,
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
            embedded_core: Weak::new(),
            theme_rx: None,
            notify_rx: None,
            shutdown: None,
        });
    }
    let home = Home::discover().map_err(|error| anyhow!("{error:?}"))?;
    let settings = home.settings();
    let registry = Registry::from_settings(&settings).map_err(|error| anyhow!("{error:?}"))?;
    let core = Core::new(home, registry);
    let embedded_core = Arc::downgrade(&core);
    let theme_rx = core.theme_watch();
    let notify_rx = core.notify_subscribe();
    let server = Server::bind(core, None)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    let url = server.local_url();
    let (trigger, stop_rx) = oneshot::channel::<()>();
    let (done_tx, done) = oneshot::channel::<()>();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = server
            .serve(async move {
                let _ = stop_rx.await;
            })
            .await
        {
            eprintln!("vetro core stopped: {error}");
        }
        let _ = done_tx.send(());
    });
    Ok(CoreHandle {
        url,
        embedded_core,
        theme_rx: Some(theme_rx),
        notify_rx: Some(notify_rx),
        shutdown: Some(ShutdownHandle { trigger, done }),
    })
}

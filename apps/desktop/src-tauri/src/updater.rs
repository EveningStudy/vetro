use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Weak,
    },
    time::Duration,
};

use tauri::{AppHandle, Manager, Url};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;
use verre_agent_api::AgentError;
use verre_core::{
    updater::{GithubReleasesUpdater, UpdateDriver, UpdateFuture},
    Core,
};
use verre_proto::rpc::{ErrorCode, ServerEvent, UpdateCheckResult, UpdateInstallResult};

use crate::CoreShutdown;

const UPDATER_ENDPOINT: &str =
    "https://github.com/EveningStudy/verre/releases/latest/download/latest.json";
const UPDATER_PUBKEY: Option<&str> = option_env!("VERRE_UPDATER_PUBKEY");

pub struct DesktopUpdater {
    app: AppHandle,
    core: Weak<Core>,
    pending: Mutex<Option<Update>>,
    installing: Arc<AtomicBool>,
    fallback: GithubReleasesUpdater,
}

impl DesktopUpdater {
    pub fn new(app: AppHandle, core: Weak<Core>) -> Self {
        Self {
            app,
            core,
            pending: Mutex::new(None),
            installing: Arc::new(AtomicBool::new(false)),
            fallback: GithubReleasesUpdater,
        }
    }

    fn configured_pubkey() -> Option<&'static str> {
        UPDATER_PUBKEY.map(str::trim).filter(|key| !key.is_empty())
    }
}

struct InstallGuard(Arc<AtomicBool>);

impl Drop for InstallGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl UpdateDriver for DesktopUpdater {
    fn check(&self) -> UpdateFuture<'_, UpdateCheckResult> {
        Box::pin(async move {
            let current = env!("CARGO_PKG_VERSION").to_owned();
            if let Some(pubkey) = Self::configured_pubkey() {
                let endpoint = Url::parse(UPDATER_ENDPOINT)
                    .map_err(|error| AgentError::new(ErrorCode::Internal, error.to_string()))?;
                if let Ok(builder) = self
                    .app
                    .updater_builder()
                    .pubkey(pubkey)
                    .endpoints(vec![endpoint])
                {
                    if let Ok(updater) = builder.build() {
                        if let Ok(checked) = updater.check().await {
                            match checked {
                                Some(update) => {
                                    let version = update.version.clone();
                                    let notes = update
                                        .body
                                        .as_deref()
                                        .map(str::trim)
                                        .filter(|text| !text.is_empty())
                                        .map(str::to_owned);
                                    let published_at = update.date.map(|d| d.to_string());
                                    *self.pending.lock().await = Some(update);
                                    return Ok(UpdateCheckResult {
                                        current_version: current,
                                        latest_version: Some(version.clone()),
                                        available: true,
                                        can_auto_install: true,
                                        release_url: Some(format!(
                                            "https://github.com/EveningStudy/verre/releases/tag/v{}",
                                            version.trim_start_matches(['v', 'V'])
                                        )),
                                        notes,
                                        published_at,
                                    });
                                }
                                None => {
                                    *self.pending.lock().await = None;
                                    return Ok(UpdateCheckResult {
                                        current_version: current.clone(),
                                        latest_version: Some(current),
                                        available: false,
                                        can_auto_install: false,
                                        release_url: Some(
                                            "https://github.com/EveningStudy/verre/releases/latest"
                                                .to_owned(),
                                        ),
                                        notes: None,
                                        published_at: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            *self.pending.lock().await = None;
            self.fallback.check().await
        })
    }

    fn install(
        &self,
        publish: Arc<dyn Fn(ServerEvent) + Send + Sync>,
    ) -> UpdateFuture<'_, UpdateInstallResult> {
        Box::pin(async move {
            if self
                .installing
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(AgentError::new(
                    ErrorCode::Busy,
                    "An update installation is already in progress",
                ));
            }
            let guard = InstallGuard(Arc::clone(&self.installing));
            let mut update = self.pending.lock().await.take();
            if update.is_none() {
                if let Some(pubkey) = Self::configured_pubkey() {
                    if let Ok(endpoint) = Url::parse(UPDATER_ENDPOINT) {
                        if let Ok(builder) = self
                            .app
                            .updater_builder()
                            .pubkey(pubkey)
                            .endpoints(vec![endpoint])
                        {
                            if let Ok(updater) = builder.build() {
                                if let Ok(Some(fresh)) = updater.check().await {
                                    update = Some(fresh);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(update) = update {
                publish(ServerEvent::UpdateProgress {
                    stage: "downloading".into(),
                    downloaded_bytes: 0,
                    total_bytes: None,
                    done: false,
                    error: None,
                });
                let app = self.app.clone();
                let core = self.core.clone();
                tauri::async_runtime::spawn(async move {
                    let _guard = guard;
                    let downloaded = Arc::new(AtomicU64::new(0));
                    let last_emitted = Arc::new(AtomicU64::new(0));
                    let total_bytes = Arc::new(AtomicU64::new(0));
                    let publish_chunk = Arc::clone(&publish);
                    let downloaded_chunk = Arc::clone(&downloaded);
                    let last_emitted_chunk = Arc::clone(&last_emitted);
                    let total_chunk = Arc::clone(&total_bytes);

                    let bytes = match update
                        .download(
                            move |chunk, total| {
                                let bytes = downloaded_chunk
                                    .fetch_add(chunk as u64, Ordering::AcqRel)
                                    + chunk as u64;
                                if let Some(total) = total {
                                    total_chunk.store(total, Ordering::Release);
                                }
                                let prev = last_emitted_chunk.load(Ordering::Acquire);
                                if bytes.saturating_sub(prev) >= 131_072 {
                                    last_emitted_chunk.store(bytes, Ordering::Release);
                                    publish_chunk(ServerEvent::UpdateProgress {
                                        stage: "downloading".into(),
                                        downloaded_bytes: bytes,
                                        total_bytes: total,
                                        done: false,
                                        error: None,
                                    });
                                }
                            },
                            || {},
                        )
                        .await
                    {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            let final_total = total_bytes.load(Ordering::Acquire);
                            publish(ServerEvent::UpdateProgress {
                                stage: "error".into(),
                                downloaded_bytes: downloaded.load(Ordering::Acquire),
                                total_bytes: (final_total > 0).then_some(final_total),
                                done: true,
                                error: Some(format!("Update download failed: {error}")),
                            });
                            return;
                        }
                    };

                    let final_bytes = downloaded.load(Ordering::Acquire);
                    let final_total = total_bytes.load(Ordering::Acquire);
                    let known_total = (final_total > 0).then_some(final_total);
                    publish(ServerEvent::UpdateProgress {
                        stage: "installing".into(),
                        downloaded_bytes: final_bytes,
                        total_bytes: known_total,
                        done: false,
                        error: None,
                    });

                    // Release managed child agents and native voice libraries before running the installer.
                    if let Some(core) = core.upgrade() {
                        let _ =
                            tokio::time::timeout(Duration::from_secs(5), core.prepare_for_update())
                                .await;
                    }

                    let install_res =
                        tokio::task::spawn_blocking(move || update.install(&bytes)).await;
                    if let Err(error) = install_res
                        .map_err(|error| error.to_string())
                        .and_then(|res| res.map_err(|error| error.to_string()))
                    {
                        publish(ServerEvent::UpdateProgress {
                            stage: "error".into(),
                            downloaded_bytes: final_bytes,
                            total_bytes: known_total,
                            done: true,
                            error: Some(format!("Update installation failed: {error}")),
                        });
                        return;
                    }

                    publish(ServerEvent::UpdateProgress {
                        stage: "restarting".into(),
                        downloaded_bytes: final_bytes,
                        total_bytes: known_total,
                        done: true,
                        error: None,
                    });

                    tokio::time::sleep(Duration::from_millis(200)).await;
                    if let Some(state) = app.try_state::<CoreShutdown>() {
                        let shutdown = state.0.lock().unwrap().take();
                        if let Some(shutdown) = shutdown {
                            let _ =
                                tokio::time::timeout(Duration::from_secs(3), shutdown.stop()).await;
                        }
                    }
                    std::mem::forget(_guard);
                    app.restart();
                });

                return Ok(UpdateInstallResult {
                    restarting: true,
                    opened_external: false,
                });
            }

            let check = self.fallback.check().await?;
            let url = check
                .release_url
                .unwrap_or_else(|| "https://github.com/EveningStudy/verre/releases/latest".into());
            self.app
                .opener()
                .open_url(&url, None::<&str>)
                .map_err(|error| AgentError::new(ErrorCode::Internal, error.to_string()))?;
            Ok(UpdateInstallResult {
                restarting: false,
                opened_external: true,
            })
        })
    }
}

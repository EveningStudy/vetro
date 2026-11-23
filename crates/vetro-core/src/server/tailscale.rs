use std::{env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use serde_json::Value;
use vetro_agent_api::{
    process::{Process, SpawnSpec},
    Result, WireLog,
};
use vetro_proto::rpc::TailscaleStatus;

use crate::{bad_request, internal};

const PORT: &str = "8443";
pub(super) const HTTPS_PORT: u16 = 8443;
const TIMEOUT: Duration = Duration::from_secs(10);
const FLAGS: &[&str] = &["serve", "--https=8443"];

pub(super) struct Tailscale(PathBuf);

impl Tailscale {
    pub(super) fn find() -> Option<Self> {
        let name = if cfg!(windows) {
            "tailscale.exe"
        } else {
            "tailscale"
        };
        env::var_os("PATH")
            .into_iter()
            .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
            .map(|dir| dir.join(name))
            .chain([
                PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe"),
                PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"),
                PathBuf::from("/usr/local/bin/tailscale"),
                PathBuf::from("/usr/bin/tailscale"),
            ])
            .find(|path| path.is_absolute() && path.is_file())
            .map(Self)
    }

    pub(super) async fn status(&self) -> TailscaleStatus {
        let info = self.json(&["status", "--json"]).await.unwrap_or_default();
        let dns_name = info["Self"]["DNSName"]
            .as_str()
            .map(|name| name.trim_end_matches('.').to_ascii_lowercase())
            .filter(|name| {
                !name.is_empty()
                    && name.split('.').all(|label| {
                        !label.is_empty()
                            && label
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    })
            });
        TailscaleStatus {
            installed: true,
            running: info["BackendState"] == "Running",
            dns_name,
            serve_url: None,
        }
    }

    fn launch(&self, args: &[&str]) -> Result<(Process, vetro_agent_api::process::Stdout)> {
        let mut spec = SpawnSpec::new(&self.0);
        spec.args.extend(args.iter().map(Into::into));
        Process::spawn(spec, Arc::new(WireLog::new(4096))).map_err(internal)
    }

    async fn json(&self, args: &[&str]) -> Result<Value> {
        let (process, mut lines) = self.launch(args)?;
        tokio::time::timeout(TIMEOUT, async {
            let mut text = String::new();
            while let Some(line) = lines.recv().await {
                text.push_str(&line.map_err(internal)?);
                text.push('\n');
            }
            if !process.wait().await.map_err(internal)?.success() {
                return Err(bad_request("Tailscale could not read its status"));
            }
            serde_json::from_str(&text).map_err(internal)
        })
        .await
        .map_err(internal)?
    }

    pub(super) async fn start(&self, dns: &str, address: SocketAddr) -> Result<Serve> {
        const STATUS: &[&str] = &["serve", "status", "--json"];
        if occupied(&self.json(STATUS).await?) {
            return Err(bad_request("Tailscale Serve port 8443 is already in use"));
        }
        let target = format!("http://{address}");
        let authority = format!("{dns}:{PORT}");
        let (process, output) = self.launch(&[FLAGS[0], FLAGS[1], &target])?;
        drop(output);
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let config = tokio::select! {
                    result = process.wait() => {
                        result.map_err(internal)?;
                        return Err(bad_request("Tailscale Serve exited before forwarding port 8443"));
                    }
                    config = self.json(STATUS) => config?,
                };
                if forwards(&config, &authority, &target) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .map_err(|_| bad_request("Timed out waiting for Tailscale Serve on port 8443"))??;
        Ok(Serve {
            process,
            url: format!("https://{authority}"),
        })
    }
}

pub(super) struct Serve {
    pub(super) process: Process,
    pub(super) url: String,
}

fn configurations(config: &Value) -> impl Iterator<Item = &Value> {
    std::iter::once(config).chain(
        config["Foreground"]
            .as_object()
            .into_iter()
            .flat_map(|map| map.values()),
    )
}

fn occupied(config: &Value) -> bool {
    configurations(config).any(|config| {
        config["TCP"].get(PORT).is_some()
            || ["Web", "AllowFunnel"].iter().any(|key| {
                config[key]
                    .as_object()
                    .is_some_and(|entries| entries.keys().any(|host| host.ends_with(":8443")))
            })
    })
}

fn forwards(config: &Value, authority: &str, target: &str) -> bool {
    configurations(config).any(|config| {
        config["TCP"][PORT]["HTTPS"] == true
            && config["Web"][authority]["Handlers"]["/"]["Proxy"]
                .as_str()
                .is_some_and(|url| url.trim_end_matches('/') == target)
    })
}

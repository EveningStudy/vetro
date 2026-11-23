use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use vetro_agent_acp::AcpAgent;
use vetro_agent_api::{
    async_trait, Agent, AgentOptions, EventSink, Result, Session, SessionPage, SessionRecord,
    StartRequest, Transcript,
};
use vetro_proto::AgentInfo;
use vetro_store_antigravity::Store;

use crate::{bad_request, internal};

pub(super) struct Antigravity {
    acp: AcpAgent,
    roots: [PathBuf; 2],
}

impl Antigravity {
    pub(super) fn new(acp: AcpAgent, home: PathBuf) -> Self {
        let gemini = home.join(".gemini");
        Self {
            acp,
            roots: [gemini.join("antigravity"), gemini.join("antigravity-cli")],
        }
    }

    async fn acp_list(&self) -> Result<Vec<SessionRecord>> {
        let mut sessions = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let page = self.acp.list(cursor).await?;
            sessions.extend(page.sessions);
            let Some(next) = page.next_cursor else {
                return Ok(sessions);
            };
            if !seen.insert(next.clone()) {
                return Err(internal("ACP returned a repeated history cursor"));
            }
            cursor = Some(next);
        }
    }
}

#[async_trait]
impl Agent for Antigravity {
    fn id(&self) -> &str {
        self.acp.id()
    }

    async fn info(&self) -> AgentInfo {
        let mut info = self.acp.info().await;
        info.capabilities.list_sessions |= self.roots.iter().any(|path| path.is_dir());
        info
    }

    async fn authenticate(&self, method_id: &str) -> Result<()> {
        self.acp.authenticate(method_id).await
    }

    async fn options(&self, cwd: Option<&Path>) -> Result<AgentOptions> {
        self.acp.options(cwd).await
    }

    async fn list(&self, cursor: Option<String>) -> Result<SessionPage> {
        if cursor.is_some() {
            return Err(bad_request("Antigravity does not return a list cursor"));
        }
        let roots = self.roots.clone();
        let disk = tokio::task::spawn_blocking(move || {
            roots
                .into_iter()
                .map(|root| Store::open(root).list())
                .collect::<Vec<_>>()
        });
        let (acp, disk) = tokio::join!(self.acp_list(), disk);
        let disk = disk.map_err(internal)?;
        let mut disk_error = None;
        let mut disk_rows = Vec::new();
        for result in disk {
            match result {
                Ok(rows) => disk_rows.extend(rows),
                Err(error) => disk_error = Some(error),
            }
        }
        let mut sessions: BTreeMap<_, _> = match acp {
            Ok(rows) => rows
                .into_iter()
                .map(|record| (record.native_id.clone(), record))
                .collect(),
            Err(error) if disk_rows.is_empty() => return Err(disk_error.unwrap_or(error)),
            Err(_) => BTreeMap::new(),
        };
        for mut record in disk_rows {
            if let Some(acp) = sessions.remove(&record.native_id) {
                record.resumable = acp.resumable;
                record.model = record.model.or(acp.model);
                record.updated_at = record.updated_at.max(acp.updated_at);
            }
            sessions.insert(record.native_id.clone(), record);
        }
        let mut sessions: Vec<_> = sessions.into_values().collect();
        sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.native_id.cmp(&right.native_id))
        });
        Ok(SessionPage {
            sessions,
            next_cursor: None,
        })
    }

    async fn read(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript> {
        let roots = self.roots.clone();
        let id = native_id.to_owned();
        let stored = tokio::task::spawn_blocking(move || {
            roots.into_iter().find_map(|root| {
                Store::open(root)
                    .read(&id)
                    .ok()
                    .filter(|transcript| !transcript.items.is_empty())
            })
        })
        .await
        .map_err(internal)?;
        match stored {
            Some(transcript) => Ok(transcript),
            None => self.acp.read(native_id, cwd).await,
        }
    }

    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
        self.acp.start(req, sink).await
    }

    async fn attach(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        self.acp.attach(native_id, cwd, sink).await
    }

    async fn shutdown(&self) {
        self.acp.shutdown().await;
    }
}

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use vetro_agent_acp::AcpAgent;
use vetro_agent_api::{
    async_trait, Agent, AgentOptions, EventSink, Result, Session, SessionPage, SessionRecord,
    StartRequest, Transcript,
};
use vetro_proto::{rpc::AuthenticateResult, AgentInfo};
use vetro_store_antigravity::Store;

use crate::{bad_request, internal};

pub(super) struct Antigravity {
    acp: AcpAgent,
    roots: [PathBuf; 2],
    acp_records: Mutex<Vec<SessionRecord>>,
}

impl Antigravity {
    pub(super) fn new(acp: AcpAgent, home: PathBuf) -> Self {
        let gemini = home.join(".gemini");
        Self {
            acp,
            roots: [gemini.join("antigravity"), gemini.join("antigravity-cli")],
            acp_records: Mutex::default(),
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

    async fn list_combined(&self, use_acp: bool) -> Result<SessionPage> {
        let roots = self.roots.clone();
        let disk = tokio::task::spawn_blocking(move || roots.map(|root| Store::open(root).list()));
        let acp = if use_acp {
            self.acp_list().await.map(Some)
        } else {
            Ok(None)
        };
        if let Ok(Some(records)) = &acp {
            self.acp_records.lock().unwrap().clone_from(records);
        }
        let mut sessions: BTreeMap<_, _> = self
            .acp_records
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|record| (record.native_id.clone(), record))
            .collect();
        let mut disk_error = None;
        for result in disk.await.map_err(internal)? {
            match result {
                Ok(records) => {
                    for mut record in records {
                        if let Some(acp) = sessions.get(&record.native_id) {
                            record.resumable = acp.resumable;
                            record.model = record.model.or_else(|| acp.model.clone());
                            record.updated_at = record.updated_at.max(acp.updated_at);
                        }
                        sessions.insert(record.native_id.clone(), record);
                    }
                }
                Err(error) => disk_error = Some(error),
            }
        }
        if let Err(error) = acp {
            if sessions.is_empty() {
                return Err(disk_error.unwrap_or(error));
            }
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

    async fn authenticate(&self, method_id: &str) -> Result<AuthenticateResult> {
        self.acp.authenticate(method_id).await
    }

    fn options_need_session(&self) -> bool {
        true
    }

    async fn options(
        &self,
        cwd: Option<&Path>,
        selected: &BTreeMap<String, String>,
    ) -> Result<AgentOptions> {
        self.acp.options(cwd, selected).await
    }

    async fn prepare_options(
        &self,
        cwd: Option<&Path>,
        selected: &BTreeMap<String, String>,
    ) -> Result<AgentOptions> {
        self.acp.prepare_options(cwd, selected).await
    }

    async fn list(&self, cursor: Option<String>) -> Result<SessionPage> {
        if cursor.is_some() {
            return Err(bad_request("Antigravity does not return a list cursor"));
        }
        self.list_combined(true).await
    }

    async fn list_without_runtime(&self) -> Result<Option<SessionPage>> {
        self.list_combined(false).await.map(Some)
    }

    async fn read(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript> {
        match self.read_without_runtime(native_id).await? {
            Some(transcript) => Ok(transcript),
            None => self.acp.read(native_id, cwd).await,
        }
    }

    async fn read_without_runtime(&self, native_id: &str) -> Result<Option<Transcript>> {
        let roots = self.roots.clone();
        let id = native_id.to_owned();
        tokio::task::spawn_blocking(move || {
            roots.into_iter().find_map(|root| {
                Store::open(root)
                    .read(&id)
                    .ok()
                    .filter(|transcript| !transcript.items.is_empty())
            })
        })
        .await
        .map_err(internal)
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

    async fn maintain(&self) {
        self.acp.maintain().await;
    }
}

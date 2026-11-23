use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use vetro_agent_api::*;
use vetro_proto::{rpc::ErrorCode, *};

pub(crate) struct Fake {
    pub cwd: PathBuf,
    pub prompts: Arc<Mutex<Vec<Vec<ContentBlock>>>>,
    pub attaches: AtomicUsize,
    pub detaches: Arc<AtomicUsize>,
    pub shutdowns: AtomicUsize,
    pub authenticated: Arc<AtomicBool>,
}

#[async_trait]
impl Agent for Fake {
    fn id(&self) -> &str {
        "fake"
    }

    async fn info(&self) -> AgentInfo {
        AgentInfo {
            id: self.id().into(),
            name: "Fake agent".into(),
            kind: AgentKind::Acp,
            installed: true,
            path: None,
            version: Some("test".into()),
            auth: AgentAuth {
                state: AuthState::Unknown,
                methods: vec![AuthMethod {
                    id: "fake-login".into(),
                    name: "Fake login".into(),
                    description: None,
                }],
                account: None,
            },
            capabilities: AgentCapabilities {
                resume: true,
                list_sessions: true,
                ..Default::default()
            },
            error: None,
        }
    }

    async fn authenticate(&self, method_id: &str) -> Result<()> {
        assert_eq!(method_id, "fake-login");
        self.authenticated.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn options(&self, _: Option<&Path>) -> Result<AgentOptions> {
        Ok(AgentOptions {
            options: vec![model("native")],
            commands: Vec::new(),
        })
    }

    async fn list(&self, cursor: Option<String>) -> Result<SessionPage> {
        let (native_id, updated_at, next_cursor, resumable) = match cursor.as_deref() {
            None => ("history", 2, Some("second".into()), true),
            Some("second") => ("readonly", 1, None, false),
            other => panic!("Unexpected native cursor: {other:?}"),
        };
        Ok(SessionPage {
            sessions: vec![SessionRecord {
                native_id: native_id.into(),
                title: format!("Fake {native_id}"),
                preview: None,
                cwd: Some(self.cwd.clone()),
                created_at: Some(1),
                updated_at,
                source: ThreadSource::Cli,
                origin: None,
                model: None,
                resumable,
                parent_native_id: None,
            }],
            next_cursor,
        })
    }

    async fn read(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript> {
        assert_eq!(cwd, Some(self.cwd.as_path()));
        if !matches!(native_id, "history" | "readonly") {
            return Err(AgentError::new(ErrorCode::NotFound, "Unknown fake session"));
        }
        Ok(Transcript {
            items: (1..=5)
                .map(|number| {
                    draft(
                        &format!("history-{number}"),
                        ItemKind::AgentMessage {
                            text: format!("History item {number}"),
                            streaming: false,
                        },
                    )
                })
                .collect(),
        })
    }

    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
        require_auth(&self.authenticated)?;
        assert_eq!(req.options.get("model").map(String::as_str), Some("chosen"));
        let session = self.session("live", sink);
        session.input(req.content);
        session.item(
            "message",
            ItemKind::AgentMessage {
                text: String::new(),
                streaming: true,
            },
        );
        session.event(SessionEvent::TextDelta {
            item_id: "message".into(),
            delta: "Fake ".into(),
        });
        session.event(SessionEvent::TextDelta {
            item_id: "message".into(),
            delta: "stream".into(),
        });
        session.event(SessionEvent::Title("Agent title".into()));
        session.event(SessionEvent::Options(vec![model("chosen")]));
        session.item(
            "permission",
            ItemKind::Request(InteractionRequest {
                request_id: "permission".into(),
                call_id: None,
                title: "Confirm?".into(),
                detail: None,
                body: RequestBody::Permission {
                    options: vec![PermissionOption {
                        id: "allow".into(),
                        label: "Allow once".into(),
                        kind: PermissionOptionKind::AllowOnce,
                    }],
                },
                resolution: None,
            }),
        );
        Ok(Arc::new(session))
    }

    async fn attach(
        &self,
        id: &str,
        _: Option<&Path>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        self.attaches.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(self.session(id, sink)))
    }

    async fn shutdown(&self) {
        self.shutdowns.fetch_add(1, Ordering::Relaxed);
    }
}

impl Fake {
    fn session(&self, id: &str, sink: EventSink) -> FakeSession {
        FakeSession {
            id: id.into(),
            sink,
            prompts: self.prompts.clone(),
            detaches: self.detaches.clone(),
            log: Arc::new(WireLog::default()),
            authenticated: self.authenticated.clone(),
        }
    }
}

struct FakeSession {
    id: String,
    sink: EventSink,
    prompts: Arc<Mutex<Vec<Vec<ContentBlock>>>>,
    detaches: Arc<AtomicUsize>,
    log: Arc<WireLog>,
    authenticated: Arc<AtomicBool>,
}

impl FakeSession {
    fn event(&self, event: SessionEvent) {
        self.sink.send(event).unwrap();
    }

    fn item(&self, id: &str, kind: ItemKind) {
        self.event(SessionEvent::Item(draft(id, kind)));
    }

    fn input(&self, content: Vec<ContentBlock>) {
        let mut prompts = self.prompts.lock().unwrap();
        self.item(
            &format!("user-{}", prompts.len()),
            ItemKind::UserMessage {
                content: content.clone(),
            },
        );
        prompts.push(content);
        self.event(SessionEvent::Status(ThreadStatus::Running));
    }

    fn end(&self, id: &str, stop_reason: StopReason) {
        self.item(
            id,
            ItemKind::TurnEnd {
                stop_reason,
                usage: None,
                duration_ms: None,
            },
        );
    }
}

#[async_trait]
impl Session for FakeSession {
    fn native_id(&self) -> &str {
        &self.id
    }

    async fn send(&self, content: Vec<ContentBlock>) -> Result<()> {
        require_auth(&self.authenticated)?;
        self.input(content);
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        self.end("interrupt", StopReason::Cancelled);
        Ok(())
    }

    async fn set_option(&self, _: &str, value: &str) -> Result<Vec<ConfigOption>> {
        Ok(vec![model(value)])
    }

    async fn respond(&self, id: &str, resolution: RequestResolution) -> Result<()> {
        assert_eq!(id, "permission");
        assert_eq!(
            resolution,
            RequestResolution::Selected {
                option_id: "allow".into()
            }
        );
        self.item(
            "message",
            ItemKind::AgentMessage {
                text: "Fake stream".into(),
                streaming: false,
            },
        );
        self.end("end", StopReason::EndTurn);
        Ok(())
    }

    async fn detach(&self) {
        self.detaches.fetch_add(1, Ordering::Relaxed);
    }

    fn wire_log(&self) -> Arc<WireLog> {
        self.log.clone()
    }
}

fn draft(id: &str, kind: ItemKind) -> ItemDraft {
    ItemDraft {
        id: id.into(),
        ts: None,
        turn_id: Some("turn".into()),
        kind,
    }
}

fn model(value: &str) -> ConfigOption {
    ConfigOption {
        id: "model".into(),
        name: "Model".into(),
        description: None,
        category: ConfigCategory::Model,
        current_value: value.into(),
        choices: Vec::new(),
    }
}

fn require_auth(authenticated: &AtomicBool) -> Result<()> {
    if authenticated.load(Ordering::Relaxed) {
        Ok(())
    } else {
        Err(AgentError::new(
            ErrorCode::AuthRequired,
            "Fixture requires login",
        ))
    }
}

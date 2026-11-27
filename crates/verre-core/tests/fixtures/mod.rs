use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use verre_agent_api::*;
use verre_proto::{
    rpc::{AuthenticateResult, ErrorCode},
    *,
};

pub(crate) struct Fake {
    pub starts: AtomicUsize,
    pub cwd: PathBuf,
    pub prompts: Arc<Mutex<Vec<Vec<ContentBlock>>>>,
    pub attaches: AtomicUsize,
    pub detaches: Arc<AtomicUsize>,
    pub shutdowns: AtomicUsize,
    pub background: Arc<AtomicBool>,
    pub authenticated: Arc<AtomicBool>,
    pub history_path: PathBuf,
    pub reads: AtomicUsize,
    pub option_reads: AtomicUsize,
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
                state: if self.authenticated.load(Ordering::Relaxed) {
                    AuthState::Ok
                } else {
                    AuthState::Required
                },
                methods: vec![
                    AuthMethod {
                        id: "fake-login".into(),
                        name: "Fake login".into(),
                        description: None,
                    },
                    AuthMethod {
                        id: "fake-device-code".into(),
                        name: "Fake device code".into(),
                        description: None,
                    },
                ],
                account: None,
                provider: None,
                endpoint: None,
            },
            capabilities: AgentCapabilities {
                resume: true,
                list_sessions: true,
                image_input: true,
                ..Default::default()
            },
            error: None,
            rate_limits: vec![
                RateLimit {
                    label: "five_hour".into(),
                    used_percent: 20.0,
                    resets_at: None,
                },
                RateLimit {
                    label: "weekly".into(),
                    used_percent: 45.0,
                    resets_at: None,
                },
            ],
            rate_limits_updated_at: None,
        }
    }

    async fn authenticate(&self, method_id: &str) -> Result<AuthenticateResult> {
        if method_id == "fake-device-code" {
            return Ok(AuthenticateResult {
                url: Some("https://example.test/device".into()),
                code: Some("TEST-CODE".into()),
                message: None,
            });
        }
        assert_eq!(method_id, "fake-login");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        self.authenticated.store(true, Ordering::Relaxed);
        Ok(AuthenticateResult::default())
    }

    fn process_auth(&self) -> Option<AuthState> {
        self.authenticated
            .load(Ordering::Relaxed)
            .then_some(AuthState::Ok)
    }

    fn options_need_session(&self) -> bool {
        true
    }

    async fn options(
        &self,
        _: Option<&Path>,
        _: &BTreeMap<String, String>,
    ) -> Result<AgentOptions> {
        self.option_reads.fetch_add(1, Ordering::Relaxed);
        Ok(AgentOptions {
            options: vec![model("native")],
            commands: vec![SlashCommand {
                name: "help".into(),
                description: None,
                input_hint: None,
            }],
        })
    }

    async fn list(&self, cursor: Option<String>) -> Result<SessionPage> {
        let (native_id, updated_at, next_cursor, resumable) = match cursor.as_deref() {
            None => (
                "history",
                std::fs::metadata(&self.history_path).unwrap().len() as i64,
                Some("second".into()),
                true,
            ),
            Some("second") => ("readonly", 1, None, false),
            other => panic!("Unexpected native cursor: {other:?}"),
        };
        let mut sessions = vec![SessionRecord {
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
            internal: false,
        }];
        if cursor.is_none() {
            if let Ok(metadata) = std::fs::metadata(self.history_path.with_file_name("fake-child"))
            {
                sessions.push(SessionRecord {
                    native_id: "external-child".into(),
                    title: "External child".into(),
                    updated_at: metadata.len() as i64,
                    source: ThreadSource::Exec,
                    ..sessions[0].clone()
                });
            }
        }
        Ok(SessionPage {
            sessions,
            next_cursor,
            incomplete: false,
        })
    }

    async fn read(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript> {
        // Temp paths can differ in spelling (8.3 short names on Windows, symlinks on macOS).
        assert_eq!(
            cwd.map(|path| path.canonicalize().unwrap()),
            Some(self.cwd.canonicalize().unwrap())
        );
        if !matches!(native_id, "history" | "readonly" | "external-child") {
            return Err(AgentError::new(ErrorCode::NotFound, "Unknown fake session"));
        }
        self.reads.fetch_add(1, Ordering::Relaxed);
        let mut items: Vec<_> = (1..=5)
            .map(|number| {
                draft(
                    &format!("history-{number}"),
                    ItemKind::AgentMessage {
                        text: format!("History item {number}"),
                        streaming: false,
                    },
                )
            })
            .collect();
        if matches!(native_id, "history" | "external-child") {
            let path = if native_id == "history" {
                self.history_path.clone()
            } else {
                self.history_path.with_file_name("fake-child")
            };
            items.extend(
                std::fs::read_to_string(path)
                    .unwrap()
                    .lines()
                    .filter(|line| !line.is_empty())
                    .enumerate()
                    .map(|(index, line)| {
                        draft(
                            &format!("external-{index}"),
                            ItemKind::AgentMessage {
                                text: line.into(),
                                streaming: false,
                            },
                        )
                    }),
            );
        }
        Ok(Transcript {
            items,
            ..Transcript::default()
        })
    }

    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
        require_auth(&self.authenticated)?;
        let count = self.starts.fetch_add(1, Ordering::Relaxed) + 1;
        let selected = req.options.get("model").map_or("native", String::as_str);
        let id = if count == 1 {
            "live".into()
        } else {
            format!("live-{count}")
        };
        let session = self.session(&id, sink);
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
        session.event(SessionEvent::Options(vec![model(selected)]));
        session.item(
            "child-tool",
            ItemKind::ToolCall(ToolCall {
                call_id: "spawn".into(),
                kind: ToolKind::Agent,
                name: Some("Task".into()),
                title: "Running external child".into(),
                status: ToolStatus::InProgress,
                input: None,
                output: Some(serde_json::json!({ "session_id": "external-child" })),
                content: Vec::new(),
                locations: Vec::new(),
                child_thread_ids: Vec::new(),
            }),
        );
        session.request_permission();
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
            background: self.background.clone(),
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
    background: Arc<AtomicBool>,
}

impl FakeSession {
    fn event(&self, event: SessionEvent) {
        self.sink.send(event).unwrap();
    }

    fn item(&self, id: &str, kind: ItemKind) {
        self.event(SessionEvent::Item(draft(id, kind)));
    }

    fn request_permission(&self) {
        self.item(
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
        let permission = content.iter().any(
            |block| matches!(block, ContentBlock::Text { text } if text == "Request approval before stop"),
        );
        self.input(content);
        if permission {
            self.request_permission();
        }
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
        self.item(
            "end",
            ItemKind::TurnEnd {
                stop_reason: StopReason::EndTurn,
                usage: Some(TokenUsage {
                    input: Some(12),
                    output: Some(7),
                    ..Default::default()
                }),
                duration_ms: Some(4_000),
            },
        );
        Ok(())
    }

    async fn detach(&self) {
        self.detaches.fetch_add(1, Ordering::Relaxed);
    }

    fn wire_log(&self) -> Arc<WireLog> {
        self.log.clone()
    }

    fn background_processes(&self) -> usize {
        usize::from(self.background.load(Ordering::Relaxed))
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
        choices: ["native", "chosen"]
            .into_iter()
            .map(|value| ConfigChoice {
                value: value.into(),
                name: value.into(),
                description: None,
                group: None,
                danger: false,
            })
            .collect(),
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

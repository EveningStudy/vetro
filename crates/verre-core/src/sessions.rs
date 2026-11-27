use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use tokio::sync::{mpsc, Mutex as AsyncMutex};
use verre_agent_api::{
    AgentError, ItemDraft, Result, Session, SessionEvent, StartRequest, Transcript,
};
use verre_proto::{rpc::*, *};

use crate::{bad_request, internal, links::Projection, not_found, now, page_limit, Core};

const DORMANT_THREADS: usize = 64;
const RETAINED_EVENTS: usize = 256;
const RETAINED_SENDS: usize = 256;

#[derive(Default)]
pub(crate) struct Sessions(Mutex<HashMap<String, Arc<Thread>>>);

pub(crate) struct Thread {
    operations: AsyncMutex<()>,
    state: Mutex<Timeline>,
    core: Weak<Core>,
}

struct Timeline {
    summary: ThreadSummary,
    session: Option<Arc<dyn Session>>,
    generation: u64,
    last_activity: Instant,
    last_access: Instant,
    raw_saved_at: Option<Instant>,
    status: ThreadStatus,
    pending: HashMap<String, usize>,
    queue: VecDeque<QueuedMessage>,
    queue_seq: u64,
    accepted_sends: VecDeque<(String, String)>,
    options: Vec<ConfigOption>,
    commands: Vec<SlashCommand>,
    usage: ThreadUsage,
    items: Vec<Item>,
    by_id: HashMap<String, usize>,
    seq: u64,
    events: VecDeque<Arc<ServerEvent>>,
    children: HashMap<String, Vec<String>>,
    native_children: HashMap<String, Vec<String>>,
    index_dirty: bool,
    history_reload_needed: bool,
}

impl Sessions {
    pub(crate) fn get(&self, id: &str) -> Option<Arc<Thread>> {
        self.0.lock().unwrap().get(id).cloned()
    }

    fn insert(&self, thread: Arc<Thread>) -> Arc<Thread> {
        let id = thread.state.lock().unwrap().summary.id.clone();
        let mut threads = self.0.lock().unwrap();
        let thread = threads.entry(id).or_insert(thread).clone();
        Self::trim(&mut threads);
        thread
    }

    fn trim(threads: &mut HashMap<String, Arc<Thread>>) {
        let mut dormant: Vec<_> = threads
            .iter()
            .filter(|(_, thread)| Arc::strong_count(thread) == 1)
            .filter_map(|(id, thread)| {
                let state = thread.state.lock().unwrap();
                state
                    .session
                    .is_none()
                    .then(|| (state.last_access.max(state.last_activity), id.clone()))
            })
            .collect();
        let evict = dormant.len().saturating_sub(DORMANT_THREADS);
        if evict > 0 {
            dormant.sort_unstable();
            for (_, id) in dormant.into_iter().take(evict) {
                threads.remove(&id);
            }
        }
    }

    pub(crate) async fn maintenance(&self, idle: Option<Duration>) {
        let mut tasks = tokio::task::JoinSet::new();
        for thread in self.snapshot() {
            tasks.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(5), thread.maintain(idle)).await;
            });
        }
        while tasks.join_next().await.is_some() {}
        Self::trim(&mut self.0.lock().unwrap());
    }

    pub(crate) fn snapshot(&self) -> Vec<Arc<Thread>> {
        self.0.lock().unwrap().values().cloned().collect()
    }

    pub(crate) fn has_live(&self, agent_id: &str) -> bool {
        self.0.lock().unwrap().values().any(|thread| {
            let state = thread.state.lock().unwrap();
            state.summary.agent_id == agent_id && state.session.is_some()
        })
    }

    pub(crate) fn has_active_work(&self) -> bool {
        self.snapshot().into_iter().any(|thread| {
            let busy = matches!(
                thread.state.lock().unwrap().summary.status,
                ThreadStatus::Running | ThreadStatus::AwaitingInput
            );
            busy || thread.background_processes() > 0
        })
    }

    pub(crate) fn has_open(&self, agent_id: &str) -> bool {
        self.0.lock().unwrap().values().any(|thread| {
            Arc::strong_count(thread) > 1
                && thread.state.lock().unwrap().summary.agent_id == agent_id
        })
    }
}

impl Core {
    pub async fn open_thread(
        self: &Arc<Self>,
        id: &str,
        limit: Option<u32>,
    ) -> Result<ThreadOpenResult> {
        Ok(self.open(id, limit).await?.0)
    }

    pub(crate) async fn open(
        self: &Arc<Self>,
        id: &str,
        limit: Option<u32>,
    ) -> Result<(ThreadOpenResult, Arc<Thread>)> {
        let thread = self.thread(id).await.inspect_err(|error| {
            self.home
                .log(&format!("Opening {id} failed ({:?}): {error}", error.code));
        })?;
        let mut snapshot = thread.snapshot(limit);
        if !snapshot.thread.live {
            let cached = self.cached_thread_options(&snapshot.thread);
            if !cached.options.is_empty() {
                snapshot.options = cached.options;
            }
            if !cached.commands.is_empty() {
                snapshot.commands = cached.commands;
            }
        }
        verre_agent_api::normalize_option_danger(&mut snapshot.options);
        Ok((snapshot, thread))
    }

    pub(crate) async fn thread(self: &Arc<Self>, id: &str) -> Result<Arc<Thread>> {
        if let Some(thread) = self.sessions.get(id) {
            if thread.history_reload_needed() {
                thread.retry_history_reload().await?;
            }
            return Ok(thread);
        }
        let (agent_id, native_id) = split_id(id)?;
        let registry = self.registry();
        let agent = registry.get(agent_id)?;
        let indexed = self.store.get(id)?;
        let unknown = indexed.is_none();
        let summary = indexed.unwrap_or_else(|| {
            let mut summary = new_summary(agent_id, native_id, native_id.into(), None);
            summary.source = ThreadSource::Unknown;
            summary
        });
        let transcript = self
            .user_agent_call_on(&registry, agent_id, || {
                self.read_history_items(&agent, &summary)
            })
            .await?;
        let cached = self.cached_thread_options(&summary);
        let mut state = Timeline::new(summary);
        state.options = cached.options;
        state.commands = cached.commands;
        state.set_children(self.store.children(id)?);
        if unknown && !matches!(agent_id, "claude" | "codex") {
            state.summary.resumable = self.agent_info(agent_id).await?.capabilities.resume;
        }
        for draft in transcript.items {
            state.item(draft);
        }
        state.merge_history_usage(transcript.usage);
        state.update_status();
        let candidate = Arc::new(Thread::new(self, state));
        Ok(self.sessions.insert(candidate))
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        params: ThreadStartParams,
    ) -> Result<(ThreadSummary, Arc<Thread>)> {
        self.check_agent_enabled(&params.agent_id)?;
        let registry = self.registry();
        let agent = registry.get(&params.agent_id)?;
        let title: String = params
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text { text } if !text.trim().is_empty() => Some(
                    text.trim()
                        .lines()
                        .next()
                        .unwrap()
                        .chars()
                        .take(100)
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_else(|| "New thread".into());
        let cwd = if params.cwd.trim().is_empty() {
            self.home.create_scratch_dir(&title)?
        } else {
            crate::files::directory(Path::new(&params.cwd))?
        };
        let (sink, events) = mpsc::unbounded_channel();
        let session = self
            .user_agent_call_on(&registry, &params.agent_id, || {
                agent.start(
                    StartRequest {
                        cwd: cwd.clone(),
                        content: params.content.clone(),
                        options: params.options.clone(),
                    },
                    sink.clone(),
                )
            })
            .await?;
        drop(sink);
        if session.native_id().is_empty() {
            session.detach().await;
            return Err(internal("Agent returned an empty native session id"));
        }
        let mut summary = new_summary(
            &params.agent_id,
            session.native_id(),
            title,
            Some(cwd.to_string_lossy().into_owned()),
        );
        summary.live = true;
        summary.status = ThreadStatus::Running;
        if let Err(error) = self.home.change_state(|state| {
            let overlay = state.threads.entry(summary.id.clone()).or_default();
            overlay.created_here = true;
            if !params.options.is_empty() {
                overlay.options = params.options;
            }
        }) {
            self.home
                .log(&format!("Could not save thread choices: {error}"));
        }
        if let Some(options) = session.initial_options() {
            if let Err(error) = self
                .remember_initial_options(&params.agent_id, &cwd, options)
                .await
            {
                self.home
                    .log(&format!("Could not cache new session options: {error}"));
            }
        }
        let cached = self.cached_thread_options(&summary);
        let mut state = Timeline::new(summary.clone());
        state.set_options(&cached.options);
        state.commands = cached.commands;
        state.session = Some(session.clone());
        let summary = state.summary.clone();
        let candidate = Arc::new(Thread::new(self, state));
        let thread = self.sessions.insert(candidate.clone());
        if !Arc::ptr_eq(&thread, &candidate) {
            session.detach().await;
            return Err(internal("Agent reused a known native session id"));
        }
        self.thread_changed(summary.clone());
        self.publish_projects();
        thread.listen(0, events);
        Ok((self.overlay(summary), thread))
    }

    pub(crate) async fn fork_thread(
        self: &Arc<Self>,
        params: ThreadForkParams,
    ) -> Result<(ThreadSummary, Arc<Thread>)> {
        let source = self.thread(&params.thread_id).await?;
        let (source_summary, source_items, source_commands) = {
            let state = source.state.lock().unwrap();
            if state.active() || !state.pending.is_empty() {
                return Err(AgentError::new(
                    ErrorCode::Busy,
                    "Wait for the current turn to finish before forking",
                ));
            }
            (
                state.summary.clone(),
                state.items.clone(),
                state.commands.clone(),
            )
        };
        if !source_summary.resumable {
            return Err(AgentError::new(
                ErrorCode::NotResumable,
                "Thread is read-only",
            ));
        }
        self.check_agent_enabled(&source_summary.agent_id)?;
        let registry = self.registry();
        let agent = registry.get(&source_summary.agent_id)?;
        let (sink, events) = mpsc::unbounded_channel();
        let session = self
            .user_agent_call_on(&registry, &source_summary.agent_id, || {
                agent.fork(
                    &source_summary.native_id,
                    source_summary.cwd.as_deref().map(Path::new),
                    params.turn_id.as_deref(),
                    sink.clone(),
                )
            })
            .await?;
        drop(sink);
        if session.native_id().is_empty() || session.native_id() == source_summary.native_id {
            session.detach().await;
            return Err(internal("Agent returned an invalid forked session id"));
        }
        let mut summary = new_summary(
            &source_summary.agent_id,
            session.native_id(),
            source_summary.title.clone(),
            source_summary.cwd.clone(),
        );
        summary.model.clone_from(&source_summary.model);
        summary.live = true;
        summary.status = ThreadStatus::Idle;
        self.remember_created_here(&summary.id);
        let native_items = self
            .read_history_items(&agent, &summary)
            .await
            .map(|transcript| transcript.items)
            .unwrap_or_default();
        let mut state = Timeline::new(summary.clone());
        let cached = self.cached_thread_options(&summary);
        let initial_options = session.current_options().unwrap_or(cached.options);
        state.set_options(&initial_options);
        state.commands = if !source_commands.is_empty() {
            source_commands
        } else {
            cached.commands
        };
        if !native_items.is_empty() {
            for draft in native_items {
                state.item(draft);
            }
        } else {
            let cut = params.turn_id.as_deref();
            for item in source_items {
                let stop = cut.is_some_and(|target| {
                    matches!(item.kind, ItemKind::TurnEnd { .. })
                        && item.turn_id.as_deref() == Some(target)
                });
                state.item(ItemDraft {
                    id: item.id,
                    ts: item.ts,
                    turn_id: item.turn_id,
                    kind: item.kind,
                });
                if stop {
                    break;
                }
            }
        }
        state.session = Some(session.clone());
        state.update_status();
        let summary = state.summary.clone();
        let candidate = Arc::new(Thread::new(self, state));
        let thread = self.sessions.insert(candidate.clone());
        if !Arc::ptr_eq(&thread, &candidate) {
            session.detach().await;
            return Err(internal("Agent reused a known native session id"));
        }
        self.thread_changed(summary.clone());
        self.publish_projects();
        thread.listen(0, events);
        Ok((self.overlay(summary), thread))
    }
}

impl Thread {
    pub(crate) fn events_after(&self, seq: u64) -> Option<Vec<Arc<ServerEvent>>> {
        let state = self.state.lock().unwrap();
        let count = state.seq.saturating_sub(seq) as usize;
        let start = state.events.len().checked_sub(count)?;
        Some(state.events.iter().skip(start).cloned().collect())
    }

    fn new(core: &Arc<Core>, state: Timeline) -> Self {
        Self {
            operations: AsyncMutex::new(()),
            state: Mutex::new(state),
            core: Arc::downgrade(core),
        }
    }

    fn core(&self) -> Arc<Core> {
        self.core
            .upgrade()
            .expect("core outlives thread operations")
    }

    pub(crate) fn is_live(&self) -> bool {
        self.state.lock().unwrap().session.is_some()
    }

    pub(crate) fn history_reload_needed(&self) -> bool {
        self.state.lock().unwrap().history_reload_needed
    }

    pub(crate) fn live_overlay_into(&self, summary: &mut ThreadSummary) {
        let state = self.state.lock().unwrap();
        let live = &state.summary;
        if state.session.is_some() || live.status != ThreadStatus::Idle {
            summary.live = live.live;
            summary.status = live.status;
            if live.model.is_some() {
                summary.model.clone_from(&live.model);
            }
            summary.updated_at = summary.updated_at.max(live.updated_at);
        }
    }

    pub(crate) fn take_projection(&self) -> Option<(ThreadSummary, Projection)> {
        let mut state = self.state.lock().unwrap();
        if !std::mem::take(&mut state.index_dirty) {
            return None;
        }
        let projection = Projection::from_items(
            &state.summary,
            state
                .items
                .iter()
                .map(|item| (&item.kind, item.ts, item.turn_id.as_deref())),
            Some(&state.native_children),
        );
        Some((state.summary.clone(), projection))
    }

    pub(crate) fn refresh_history(&self, mut summary: ThreadSummary, transcript: Transcript) {
        let core = self.core();
        let Ok(children) = core.store.children(&summary.id) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if state.session.is_some() || state.index_dirty || state.history_reload_needed {
            return;
        }
        state.set_children(children);
        if summary.created_at.is_none() {
            summary.created_at = state.summary.created_at;
        }
        let old = state.summary.clone();
        let usage_before = state.usage.clone();
        state.status = summary.status;
        state.summary = summary;
        for event in state.merge_history(transcript.items) {
            state.emit(&core, event);
        }
        state.merge_history_usage(transcript.usage);
        if state.usage != usage_before {
            let usage = state.usage.clone();
            state.emit(&core, ThreadEvent::Usage { usage });
        }
        state.update_status();
        if state.summary != old {
            let thread = core.overlay(state.summary.clone());
            state.emit(&core, ThreadEvent::Summary { thread });
        }
    }

    pub(crate) fn refresh_links(&self, summary: &ThreadSummary) {
        let core = self.core();
        let Ok(children) = core.store.children(&summary.id) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        let summary_changed = state.summary.parent != summary.parent
            || state.summary.child_count != summary.child_count
            || state.summary.internal != summary.internal;
        state.summary.parent.clone_from(&summary.parent);
        state.summary.child_count = summary.child_count;
        state.summary.internal = summary.internal;
        if summary_changed {
            let thread = core.overlay(state.summary.clone());
            state.emit(&core, ThreadEvent::Summary { thread });
        }
        let previous = std::mem::take(&mut state.children);
        state.set_children(children);
        if previous == state.children {
            return;
        }
        let Timeline {
            items, children, ..
        } = &mut *state;
        let updated: Vec<_> = items
            .iter_mut()
            .filter_map(|item| {
                let ItemKind::ToolCall(call) = &mut item.kind else {
                    return None;
                };
                let current = children
                    .get(&call.call_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                if current == call.child_thread_ids {
                    return None;
                }
                call.child_thread_ids = current.to_vec();
                Some(item.clone())
            })
            .collect();
        for item in updated {
            state.emit(&core, ThreadEvent::ItemUpdated { item });
        }
    }

    pub(crate) fn snapshot(&self, limit: Option<u32>) -> ThreadOpenResult {
        let mut state = self.state.lock().unwrap();
        state.last_access = Instant::now();
        let page = state.page(state.items.len(), limit);
        ThreadOpenResult {
            thread: self.core().overlay(state.summary.clone()),
            items: page.items,
            has_more: page.has_more,
            seq: state.seq,
            options: state.options.clone(),
            commands: state.commands.clone(),
            usage: state.usage.clone(),
            queue: state.queue.iter().cloned().collect(),
        }
    }

    pub(crate) fn items(&self, before: u64, limit: Option<u32>) -> ThreadItemsResult {
        let mut state = self.state.lock().unwrap();
        state.last_access = Instant::now();
        state.page(
            state.items.partition_point(|item| item.ordinal < before),
            limit,
        )
    }

    fn listen(
        self: &Arc<Self>,
        generation: u64,
        mut events: mpsc::UnboundedReceiver<SessionEvent>,
    ) {
        let thread = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let Some(thread) = thread.upgrade() else {
                    return;
                };
                if event == SessionEvent::AuthRequired {
                    let id = {
                        let state = thread.state.lock().unwrap();
                        if state.generation != generation || state.session.is_none() {
                            return;
                        }
                        state.summary.agent_id.clone()
                    };
                    if let Some(core) = thread.core.upgrade() {
                        core.set_agent_auth(&id, AuthState::Required).await;
                    }
                    continue;
                }
                if thread.core.upgrade().is_none() || !thread.handle(generation, event) {
                    return;
                }
                if thread.ready_to_send() {
                    let thread = thread.clone();
                    tokio::spawn(async move { thread.send_queued().await });
                }
            }
            if let Some(thread) = thread.upgrade() {
                if thread.core.upgrade().is_some() {
                    thread.handle(generation, SessionEvent::Closed { error: None });
                }
            }
        });
    }

    fn handle(&self, generation: u64, event: SessionEvent) -> bool {
        let Some(core) = self.core.upgrade() else {
            return false;
        };
        let mut state = self.state.lock().unwrap();
        if state.generation != generation || state.session.is_none() {
            return false;
        }
        state.last_activity = Instant::now();
        let before = state.summary.clone();
        let usage_before = state.usage.clone();
        let mut closed_session = None;
        let mut bump_updated = true;
        let event = match event {
            SessionEvent::Item(draft) => {
                state.index_dirty |= matches!(
                    draft.kind,
                    ItemKind::UserMessage { .. }
                        | ItemKind::AgentMessage { .. }
                        | ItemKind::ToolCall(_)
                        | ItemKind::TurnEnd { .. }
                );
                if matches!(
                    draft.kind,
                    ItemKind::TurnEnd {
                        stop_reason: StopReason::Cancelled,
                        ..
                    }
                ) {
                    state.cancel_pending(&core, |item| {
                        item.turn_id.is_none() || item.turn_id == draft.turn_id
                    });
                }
                Some(state.item(draft))
            }
            SessionEvent::TextDelta { item_id, delta } => {
                state.index_dirty = true;
                state.delta(item_id, delta)
            }
            SessionEvent::Status(status) => {
                state.status = status;
                None
            }
            SessionEvent::Title(title) => {
                state.summary.title = title;
                state.summary.internal = crate::storage::classified(&state.summary);
                None
            }
            SessionEvent::Model(model) => {
                state.summary.model = model;
                None
            }
            SessionEvent::Options(options) => {
                if state.options == options {
                    return true;
                }
                state.set_options(&options);
                Some(ThreadEvent::Options { options })
            }
            SessionEvent::Commands(commands) => {
                state.commands.clone_from(&commands);
                Some(ThreadEvent::Commands { commands })
            }
            SessionEvent::Usage(usage) => {
                if !usage.rate_limits.is_empty() {
                    core.record_agent_rate_limits(
                        &state.summary.agent_id,
                        usage.rate_limits.clone(),
                    );
                }
                state.usage = usage;
                None
            }
            SessionEvent::AuthRequired => None,
            SessionEvent::Closed { error } => {
                let was_active =
                    state.active() || !state.pending.is_empty() || !state.queue.is_empty();
                let shutting_down = core.is_shutting_down();
                closed_session = state.session.take();
                state.summary.live = false;
                state.generation += 1;
                if !state.queue.is_empty() {
                    state.queue.clear();
                    state.emit(
                        &core,
                        ThreadEvent::Queue {
                            messages: Vec::new(),
                        },
                    );
                }
                state.cancel_pending(&core, |_| true);
                if error.is_some() && was_active && !shutting_down {
                    state.status = ThreadStatus::Error;
                    error.map(|text| state.notice(text))
                } else {
                    if let Some(text) = &error {
                        core.home
                            .log(&format!("Idle session {} closed: {text}", state.summary.id));
                    }
                    state.status = ThreadStatus::Idle;
                    bump_updated = false;
                    None
                }
            }
        };
        if let Some(event) = event {
            state.emit(&core, event);
        }
        if state.usage != usage_before {
            let usage = state.usage.clone();
            state.emit(&core, ThreadEvent::Usage { usage });
        }
        state.update_status();
        if before != state.summary {
            if bump_updated {
                state.changed(&core);
            } else {
                state.changed_without_bump(&core);
            }
        }
        let still_live = state.session.is_some();
        if let Some(session) = closed_session {
            self.persist_log(&core, &state.summary.id, &session);
        }
        still_live
    }

    async fn attach(self: &Arc<Self>, change: Option<(&str, &str)>) -> Result<Arc<dyn Session>> {
        let summary = {
            let state = self.state.lock().unwrap();
            if let Some(session) = &state.session {
                let Some((id, _)) = change else {
                    return Ok(session.clone());
                };
                if !state.can_release_idle(Duration::ZERO) {
                    return Err(AgentError::new(
                        ErrorCode::Busy,
                        format!("Wait for the current turn to finish before changing {id}"),
                    ));
                }
            }
            state.summary.clone()
        };
        if change.is_some() && self.background_processes() > 0 {
            return Err(AgentError::new(
                ErrorCode::BackgroundWork,
                "Wait for the agent's background processes before restarting the session",
            ));
        }
        if !summary.resumable {
            return Err(AgentError::new(
                ErrorCode::NotResumable,
                "Thread is read-only",
            ));
        }
        let core = self.core();
        core.check_agent_enabled(&summary.agent_id)?;
        let (sink, events) = mpsc::unbounded_channel();
        let registry = core.registry();
        let agent = registry.get(&summary.agent_id)?;
        if change.is_some_and(|(id, _)| agent.resume_option_requires_detach(id)) {
            self.release(false, Some(Duration::ZERO)).await?;
        }
        let mut saved = core.home.read_state(|state| {
            state
                .threads
                .get(&summary.id)
                .map(|overlay| overlay.options.clone())
                .unwrap_or_default()
        });
        if summary.agent_id == "codex" && saved.contains_key("permissions") {
            saved.remove("sandbox");
        }
        if let Some((id, value)) = change {
            remove_incompatible_option(&summary.id, &mut saved, id);
            saved.insert(id.to_owned(), value.to_owned());
        }
        let (launch, mut choices): (Vec<_>, Vec<_>) = saved
            .into_iter()
            .partition(|(id, _)| agent.resume_option_at_launch(id));
        let launch: BTreeMap<_, _> = launch.into_iter().collect();
        let result = core
            .user_agent_call_on(&registry, &summary.agent_id, || {
                agent.attach_with_options(
                    &summary.native_id,
                    summary.cwd.as_deref().map(Path::new),
                    launch.clone(),
                    sink.clone(),
                )
            })
            .await;
        if let Err(error) = &result {
            if change.is_none() && error.code == ErrorCode::NotResumable {
                let mut state = self.state.lock().unwrap();
                state.summary.resumable = false;
                state.changed(&core);
            }
        }
        let session = result?;
        if session.native_id() != summary.native_id {
            session.detach().await;
            return Err(internal("Agent attached to a different native session"));
        }
        let requested_options = if let Some((id, value)) = change {
            let options = session.current_options().unwrap_or_default();
            if !options
                .iter()
                .any(|option| option.id == id && option.current_value == value)
            {
                session.detach().await;
                return Err(bad_request(format!("Agent did not apply {id}={value}")));
            }
            if let Err(error) = Self::save_option(&core, &summary.id, id, value) {
                session.detach().await;
                return Err(error);
            }
            Some(options)
        } else {
            None
        };
        choices.sort_by_key(|(id, _)| match id.as_str() {
            "model" => 0,
            "mode" | "permissions" | "approvalPolicy" | "sandbox" => 1,
            _ => 2,
        });
        let mut restored = None;
        let mut stale = Vec::new();
        for (id, value) in choices {
            match core
                .user_agent_call_on(&registry, &summary.agent_id, || {
                    session.set_option(&id, &value)
                })
                .await
            {
                Ok(options) => restored = Some(options),
                Err(error) => {
                    core.home.log(&format!(
                        "Skipping unavailable saved option {id}={value} on {}: {error}",
                        summary.id
                    ));
                    stale.push((id, value));
                }
            }
        }
        if !stale.is_empty() {
            let stale_keys: Vec<String> = stale.iter().map(|(id, _)| id.clone()).collect();
            let _ = core.home.change_state(|state| {
                if let Some(overlay) = state.threads.get_mut(&summary.id) {
                    for key in &stale_keys {
                        overlay.options.remove(key);
                    }
                }
            });
            let note = stale
                .into_iter()
                .map(|(id, value)| format!("{id}={value}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut state = self.state.lock().unwrap();
            let event = state.info_notice(format!(
                "Skipped unavailable saved options on resume: {note}"
            ));
            state.emit(&core, event);
        }
        if let Some(options) = restored {
            let _ = sink.send(SessionEvent::Options(options));
        }
        drop(sink);
        let previous = {
            let mut state = self.state.lock().unwrap();
            let previous = state.session.replace(session.clone());
            if let Some(previous) = &previous {
                state.generation += 1;
                self.persist_log(&core, &summary.id, previous);
            }
            if let Some(options) = requested_options {
                state.set_options(&options);
                state.emit(&core, ThreadEvent::Options { options });
            }
            state.summary.live = true;
            state.last_activity = Instant::now();
            state.raw_saved_at = None;
            state.changed(&core);
            self.listen(state.generation, events);
            previous
        };
        if let Some(previous) = previous {
            previous.detach().await;
        }
        Ok(session)
    }

    pub(crate) async fn confirm_send(&self, key: &(String, String)) -> bool {
        let _operation = self.operations.lock().await;
        self.state.lock().unwrap().accepted_sends.contains(key)
    }

    pub(crate) async fn send(
        self: &Arc<Self>,
        content: Vec<ContentBlock>,
        attempt: Option<(String, String)>,
    ) -> Result<()> {
        let _operation = self.operations.lock().await;
        if attempt
            .as_ref()
            .is_some_and(|key| self.state.lock().unwrap().accepted_sends.contains(key))
        {
            return Ok(());
        }
        let core = self.core();
        let agent_id = self.state.lock().unwrap().summary.agent_id.clone();
        let steer = core
            .agent_info(&agent_id)
            .await?
            .capabilities
            .mid_turn_input;
        {
            let mut state = self.state.lock().unwrap();
            let awaiting_input = !state.pending.is_empty();
            let queue = state.session.as_ref().is_some_and(|session| {
                !state.queue.is_empty()
                    || ((state.active() || awaiting_input)
                        && !(steer && session.can_send_during_turn(&content, awaiting_input)))
            });
            if queue {
                if state.queue.len() >= 32 {
                    return Err(AgentError::new(
                        ErrorCode::Busy,
                        "Thread input queue is full",
                    ));
                }
                state.queue_seq += 1;
                let id = format!("q:{}", state.queue_seq);
                state.queue.push_back(QueuedMessage { id, content });
                state.record_send(attempt);
                let messages = state.queue.iter().cloned().collect();
                state.emit(&core, ThreadEvent::Queue { messages });
                return Ok(());
            }
        }
        let session = self.attach(None).await?;
        self.send_now(&core, session, content).await?;
        self.state.lock().unwrap().record_send(attempt);
        Ok(())
    }

    async fn send_now(
        &self,
        core: &Core,
        session: Arc<dyn Session>,
        content: Vec<ContentBlock>,
    ) -> Result<()> {
        let (generation, was_active) = {
            let mut state = self.state.lock().unwrap();
            state.last_activity = Instant::now();
            let was_active = state.active();
            state.status = ThreadStatus::Running;
            state.update_status();
            state.changed(core);
            (state.generation, was_active)
        };
        let agent_id = self.state.lock().unwrap().summary.agent_id.clone();
        if let Err(error) = core
            .user_agent_call(&agent_id, || session.send(content.clone()))
            .await
        {
            let agent_id = {
                let mut state = self.state.lock().unwrap();
                if generation == state.generation {
                    if !was_active && state.status == ThreadStatus::Running {
                        state.status = ThreadStatus::Error;
                    }
                    let notice = state.notice(error.message.clone());
                    state.emit(core, notice);
                    state.update_status();
                    state.changed(core);
                }
                state.summary.agent_id.clone()
            };
            return core.auth_result(&agent_id, Err(error)).await;
        }
        Ok(())
    }

    fn ready_to_send(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.session.is_some() && !state.active() && !state.queue.is_empty()
    }

    async fn send_queued(&self) {
        let _operation = self.operations.lock().await;
        let Some(core) = self.core.upgrade() else {
            return;
        };
        loop {
            let (session, content) = {
                let mut state = self.state.lock().unwrap();
                if state.active() || state.queue.is_empty() {
                    return;
                }
                let Some(session) = state.session.clone() else {
                    return;
                };
                let queued = state.queue.pop_front().unwrap();
                let messages = state.queue.iter().cloned().collect();
                state.emit(&core, ThreadEvent::Queue { messages });
                (session, queued.content)
            };
            if self.send_now(&core, session, content).await.is_ok() {
                return;
            }
        }
    }

    pub(crate) fn cancel_queued(&self, queue_id: &str) -> bool {
        let core = self.core();
        let mut state = self.state.lock().unwrap();
        let before = state.queue.len();
        state.queue.retain(|entry| entry.id != queue_id);
        let removed = state.queue.len() != before;
        if removed {
            let messages = state.queue.iter().cloned().collect();
            state.emit(&core, ThreadEvent::Queue { messages });
        }
        removed
    }

    pub(crate) async fn interrupt(&self) -> Result<()> {
        let _operation = self.operations.lock().await;
        let core = self.core();
        let session = {
            let mut state = self.state.lock().unwrap();
            if !state.queue.is_empty() {
                state.queue.clear();
                state.emit(
                    &core,
                    ThreadEvent::Queue {
                        messages: Vec::new(),
                    },
                );
            }
            state.session.clone()
        };
        if let Some(session) = session {
            session.interrupt().await?;
        }
        Ok(())
    }

    pub(crate) async fn set_option(
        self: &Arc<Self>,
        params: ThreadSetOptionParams,
    ) -> Result<OptionsResult> {
        let _operation = self.operations.lock().await;
        let core = self.core();
        let (agent_id, live) = {
            let state = self.state.lock().unwrap();
            (state.summary.agent_id.clone(), state.session.is_some())
        };
        let registry = core.registry();
        let agent = registry.get(&agent_id)?;
        if !agent.resume_option_requires_detach(&params.option_id)
            && (live || !agent.resume_option_at_launch(&params.option_id))
        {
            let session = self.attach(None).await?;
            let result = core
                .user_agent_call_on(&registry, &agent_id, || {
                    session.set_option(&params.option_id, &params.value)
                })
                .await;
            match result {
                Ok(options) => {
                    Self::save_option(&core, &params.thread_id, &params.option_id, &params.value)?;
                    let generation = self.state.lock().unwrap().generation;
                    self.handle(generation, SessionEvent::Options(options.clone()));
                    return Ok(OptionsResult { options });
                }
                Err(error) if agent.retry_option_on_resume(&params.option_id, &error) => {}
                Err(error) => return Err(error),
            }
        }
        self.attach(Some((&params.option_id, &params.value)))
            .await?;
        let options = self.state.lock().unwrap().options.clone();
        Ok(OptionsResult { options })
    }

    fn save_option(core: &Core, thread_id: &str, id: &str, value: &str) -> Result<()> {
        core.home.change_state(|state| {
            let options = &mut state
                .threads
                .entry(thread_id.to_owned())
                .or_default()
                .options;
            remove_incompatible_option(thread_id, options, id);
            options.insert(id.to_owned(), value.to_owned());
        })
    }

    pub(crate) async fn respond(self: &Arc<Self>, params: ThreadRespondParams) -> Result<()> {
        let _operation = self.operations.lock().await;
        let (session, generation) = {
            let state = self.state.lock().unwrap();
            if !state.pending.contains_key(&params.request_id) {
                return Err(not_found("Request is no longer pending"));
            }
            (
                state
                    .session
                    .clone()
                    .ok_or_else(|| bad_request("Thread is not attached"))?,
                state.generation,
            )
        };
        session
            .respond(&params.request_id, params.resolution.clone())
            .await?;
        let core = self.core();
        {
            let mut state = self.state.lock().unwrap();
            if state.generation == generation {
                if let Some(event) = state.resolve(&params.request_id, params.resolution) {
                    state.emit(&core, event);
                    state.update_status();
                    state.changed(&core);
                }
            }
        }
        if self.ready_to_send() {
            let thread = self.clone();
            tokio::spawn(async move { thread.send_queued().await });
        }
        Ok(())
    }

    pub(crate) fn overlay_changed(&self) -> ThreadSummary {
        let core = self.core();
        let mut state = self.state.lock().unwrap();
        let thread = core.overlay(state.summary.clone());
        state.emit(
            &core,
            ThreadEvent::Summary {
                thread: thread.clone(),
            },
        );
        core.publish(ServerEvent::ThreadsUpserted {
            threads: vec![thread.clone()],
        });
        thread
    }

    pub(crate) async fn detach(&self, force: bool) -> Result<()> {
        let _operation = self.operations.lock().await;
        let background = self.background_processes();
        if !force && background > 0 {
            return Err(AgentError::new(
                ErrorCode::BackgroundWork,
                format!(
                    "{background} background process(es) in this agent runtime are still running"
                ),
            ));
        }
        let agent_id = self.state.lock().unwrap().summary.agent_id.clone();
        self.release(false, None).await?;
        let core = self.core();
        core.registry()
            .release_requested(&agent_id, || core.sessions.has_live(&agent_id))
            .await;
        Ok(())
    }

    pub(crate) async fn rollback(&self, before_turn_id: &str) -> Result<ThreadOpenResult> {
        let _operation = self.operations.lock().await;
        let summary = self.state.lock().unwrap().editable_history()?;
        let core = self.core();
        core.check_agent_enabled(&summary.agent_id)?;
        let registry = core.registry();
        let agent = registry.get(&summary.agent_id)?;
        if !agent.history_edit_capabilities().rollback_conversation {
            return Err(bad_request("Agent does not support conversation rollback"));
        }
        let _history_guard = core.history_edits.write().await;
        self.release(false, Some(Duration::ZERO)).await?;
        self.state.lock().unwrap().editable_history()?;
        core.user_agent_call_on(&registry, &summary.agent_id, || {
            agent.rollback_conversation(
                &summary.native_id,
                summary.cwd.as_deref().map(Path::new),
                before_turn_id,
            )
        })
        .await?;
        if let Err(error) = core.home.change_state(|state| {
            state
                .threads
                .entry(summary.id.clone())
                .or_default()
                .history_rolled_back = true;
        }) {
            core.home.log(&format!(
                "Could not record history rollback for {}: {error}",
                summary.id
            ));
        }
        self.reload_after_rollback(&summary).await?;
        Ok(self.snapshot(None))
    }

    async fn retry_history_reload(&self) -> Result<()> {
        let _operation = self.operations.lock().await;
        let summary = {
            let state = self.state.lock().unwrap();
            if !state.history_reload_needed {
                return Ok(());
            }
            state.summary.clone()
        };
        let core = self.core();
        let _history_guard = core.history_edits.write().await;
        self.reload_after_rollback(&summary).await
    }

    async fn reload_after_rollback(&self, summary: &ThreadSummary) -> Result<()> {
        let core = self.core();
        let registry = core.registry();
        let transcript = core
            .user_agent_call_on(&registry, &summary.agent_id, || async {
                let agent = registry.get(&summary.agent_id)?;
                core.read_authoritative_history_items(&agent, summary).await
            })
            .await;
        let mut state = self.state.lock().unwrap();
        if transcript.is_ok() || !state.history_reload_needed {
            state.items.clear();
            state.by_id.clear();
            state.pending.clear();
            state.native_children.clear();
            state.usage = ThreadUsage::default();
            state.status = ThreadStatus::Idle;
            let error = match transcript {
                Ok(transcript) => {
                    for draft in transcript.items {
                        state.item(draft);
                    }
                    state.merge_history_usage(transcript.usage);
                    None
                }
                Err(error) => Some(error),
            };
            state.history_reload_needed = error.is_some();
            state.update_status();
            state.index_dirty = true;
            state.summary.updated_at = state.summary.updated_at.max(now());
            state.events.clear();
            state.seq += 1;
            core.thread_changed(state.summary.clone());
            core.publish(ServerEvent::AppResync {});
            return error.map_or(Ok(()), Err).map_err(rollback_reload_error);
        }
        transcript.map(|_| ()).map_err(rollback_reload_error)
    }

    pub(crate) async fn rewind_files(
        self: &Arc<Self>,
        user_message_id: &str,
        dry_run: bool,
    ) -> Result<RewindFilesResult> {
        let _operation = self.operations.lock().await;
        let summary = self.state.lock().unwrap().editable_history()?;
        let core = self.core();
        core.check_agent_enabled(&summary.agent_id)?;
        let registry = core.registry();
        let capabilities = registry.get(&summary.agent_id)?.history_edit_capabilities();
        if dry_run && !capabilities.preview_rewind_files {
            return Err(bad_request("Agent does not support file rewind previews"));
        }
        if !dry_run && !capabilities.rewind_files {
            return Err(bad_request("Agent does not support file rewind"));
        }
        let session = self.attach(None).await?;
        let _guard = registry.enter(&summary.agent_id, true).await?;
        session.rewind_files(user_message_id, dry_run).await
    }

    pub(crate) fn background_processes(&self) -> usize {
        let session = self.state.lock().unwrap().session.clone();
        session.map_or(0, |session| session.background_processes())
    }

    async fn release(&self, force: bool, idle: Option<Duration>) -> Result<()> {
        let core = self.core();
        let automatic = idle.is_some_and(|idle| !idle.is_zero());
        let agent_id = {
            let state = self.state.lock().unwrap();
            if state.session.is_none() {
                return Ok(());
            }
            if idle.is_some_and(|idle| !state.can_release_idle(idle)) {
                return Ok(());
            }
            state.summary.agent_id.clone()
        };
        let registry = core.registry();
        let _guard = if registry.get(&agent_id).is_ok() {
            registry.enter(&agent_id, false).await.ok()
        } else {
            None
        };
        let background = self.background_processes();
        if idle.is_some() && background > 0 {
            if automatic {
                return Ok(());
            }
            return Err(AgentError::new(
                ErrorCode::BackgroundWork,
                "Wait for the agent's background processes before editing history",
            ));
        }
        let (session, id) = {
            let mut state = self.state.lock().unwrap();
            if !force && !state.pending.is_empty() {
                return Err(AgentError::new(
                    ErrorCode::Busy,
                    "Thread has unanswered requests",
                ));
            }
            if idle.is_some_and(|idle| !state.can_release_idle(idle)) {
                return Ok(());
            }
            let Some(session) = state.session.take() else {
                return Ok(());
            };
            state.generation += 1;
            if !state.queue.is_empty() {
                state.queue.clear();
                state.emit(
                    &core,
                    ThreadEvent::Queue {
                        messages: Vec::new(),
                    },
                );
            }
            state.cancel_pending(&core, |_| true);
            state.summary.live = false;
            state.status = ThreadStatus::Idle;
            state.update_status();
            state.changed_without_bump(&core);
            (session, state.summary.id.clone())
        };
        let reason = match (force, idle) {
            (true, _) => "shutdown",
            (false, Some(idle)) if idle.is_zero() => "history edit",
            (false, Some(_)) => "idle",
            (false, None) => "user detach",
        };
        core.home.log(&format!(
            "Released {id} ({reason}); {background} background process(es) were running"
        ));
        session.detach().await;
        self.persist_log(&core, &id, &session);
        registry.request_release(&agent_id);
        Ok(())
    }

    async fn maintain(&self, idle: Option<Duration>) {
        let Ok(_operation) = self.operations.try_lock() else {
            return;
        };
        if let Some(idle) = idle {
            let _ = self.release(false, Some(idle)).await;
        }
        let mut state = self.state.lock().unwrap();
        if let Some(session) = &state.session {
            if state
                .raw_saved_at
                .is_none_or(|saved| saved < state.last_activity)
            {
                self.persist_log(&self.core(), &state.summary.id, session);
                state.raw_saved_at = Some(Instant::now());
            }
        }
    }

    pub(crate) async fn shutdown(&self) {
        let _operation = self.operations.lock().await;
        let _ = self.release(true, None).await;
    }

    pub(crate) fn raw(&self, params: ThreadRawParams) -> Result<ThreadRawResult> {
        let state = self.state.lock().unwrap();
        let core = self.core();
        let mut entries = if let Some(session) = &state.session {
            let entries = session.wire_log().snapshot();
            core.home.write_raw(&params.thread_id, &entries)?;
            entries
        } else {
            core.home.read_raw(&params.thread_id)?
        };
        let prefix = raw_prefix_len(&entries);
        let limit = page_limit(params.limit, 500, 5000).max(prefix);
        entries.drain(prefix..prefix.max(entries.len().saturating_sub(limit - prefix)));
        Ok(ThreadRawResult { entries })
    }

    fn persist_log(&self, core: &Core, id: &str, session: &Arc<dyn Session>) {
        if let Err(error) = core.home.write_raw(id, &session.wire_log().snapshot()) {
            core.home.log(&format!("Wire log write failed: {error}"));
        }
    }
}

fn raw_prefix_len(entries: &[RawEntry]) -> usize {
    if !entries
        .first()
        .is_some_and(|entry| entry.direction == RawDirection::Spawn)
    {
        return 0;
    }
    let mut prefix = 1;
    let mut initialize = None;
    for entry in entries.iter().skip(1).take(3) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&entry.text) else {
            break;
        };
        match value["method"].as_str() {
            Some("initialize") => initialize = value.get("id").cloned(),
            Some("initialized") => {}
            None if initialize.as_ref().is_some_and(|id| id == &value["id"]) => {}
            _ => break,
        }
        prefix += 1;
    }
    prefix
}

impl Timeline {
    fn new(summary: ThreadSummary) -> Self {
        Self {
            status: summary.status,
            summary,
            session: None,
            generation: 0,
            last_activity: Instant::now(),
            last_access: Instant::now(),
            raw_saved_at: None,
            pending: HashMap::new(),
            queue: VecDeque::new(),
            queue_seq: 0,
            accepted_sends: VecDeque::new(),
            options: Vec::new(),
            commands: Vec::new(),
            usage: ThreadUsage::default(),
            items: Vec::new(),
            by_id: HashMap::new(),
            seq: 0,
            events: VecDeque::new(),
            children: HashMap::new(),
            native_children: HashMap::new(),
            index_dirty: false,
            history_reload_needed: false,
        }
    }

    fn record_send(&mut self, attempt: Option<(String, String)>) {
        if let Some(key) = attempt {
            if self.accepted_sends.len() == RETAINED_SENDS {
                self.accepted_sends.pop_front();
            }
            self.accepted_sends.push_back(key);
        }
    }

    fn page(&self, end: usize, limit: Option<u32>) -> ThreadItemsResult {
        let start = end.saturating_sub(page_limit(limit, 200, 2000));
        ThreadItemsResult {
            items: self.items[start..end].to_vec(),
            has_more: start > 0,
        }
    }

    fn active(&self) -> bool {
        matches!(
            self.status,
            ThreadStatus::Running | ThreadStatus::AwaitingInput
        ) || !self.pending.is_empty()
    }

    fn editable_history(&self) -> Result<ThreadSummary> {
        if self.active() || !self.queue.is_empty() {
            return Err(AgentError::new(
                ErrorCode::Busy,
                "Wait for the current turn and queued messages to finish before editing history",
            ));
        }
        if !self.summary.resumable {
            return Err(AgentError::new(
                ErrorCode::NotResumable,
                "Thread is read-only",
            ));
        }
        Ok(self.summary.clone())
    }

    fn can_release_idle(&self, idle: Duration) -> bool {
        !self.active() && self.queue.is_empty() && self.last_activity.elapsed() >= idle
    }

    fn set_children(&mut self, children: Vec<(String, ThreadParent)>) {
        self.children.clear();
        for (child, link) in children {
            if let Some(call_id) = link.call_id {
                self.children.entry(call_id).or_default().push(child);
            }
        }
    }

    fn decorate(&self, kind: &mut ItemKind) {
        if let ItemKind::ToolCall(call) = kind {
            call.child_thread_ids = self
                .children
                .get(&call.call_id)
                .cloned()
                .unwrap_or_default();
        }
    }

    fn merge_history_usage(&mut self, usage: ThreadUsage) {
        if usage.last_turn.is_some() {
            self.usage.last_turn = usage.last_turn;
        }
        if usage.total.is_some() {
            self.usage.total = usage.total;
        }
    }

    fn merge_history(&mut self, mut drafts: Vec<ItemDraft>) -> Vec<ThreadEvent> {
        if drafts.iter().any(|draft| {
            draft
                .turn_id
                .as_deref()
                .is_some_and(|id| id.starts_with("replay:"))
        }) {
            self.align_acp_replay(&mut drafts);
        }
        let history_ids: HashSet<String> = drafts.iter().map(|draft| draft.id.clone()).collect();
        let history_turn_ids: HashSet<String> = drafts
            .iter()
            .filter_map(|draft| draft.turn_id.clone())
            .collect();
        let mut claimed: HashSet<String> = HashSet::new();
        let mut events = Vec::new();
        for mut draft in drafts {
            if !self.by_id.contains_key(&draft.id) {
                if let ItemKind::UserMessage { content } = &draft.kind {
                    let existing = self.items.iter().find(|item| {
                        !history_ids.contains(&item.id)
                            && !claimed.contains(&item.id)
                            && matches!(&item.kind, ItemKind::UserMessage { content: c } if c == content)
                            && (item.turn_id.is_none()
                                || draft.turn_id.is_none()
                                || item.turn_id == draft.turn_id
                                || item
                                    .turn_id
                                    .as_deref()
                                    .is_some_and(|tid| !history_turn_ids.contains(tid)))
                    });
                    if let Some(existing) = existing {
                        claimed.insert(existing.id.clone());
                        draft.id = existing.id.clone();
                        draft.ts = draft.ts.or(existing.ts);
                    }
                }
            }
            if let Some(existing) = self.by_id.get(&draft.id).map(|&index| &self.items[index]) {
                draft.ts = draft.ts.or(existing.ts);
                if let (
                    ItemKind::TurnEnd {
                        usage, duration_ms, ..
                    },
                    ItemKind::TurnEnd {
                        usage: old_usage,
                        duration_ms: old_duration,
                        ..
                    },
                ) = (&mut draft.kind, &existing.kind)
                {
                    *usage = usage.take().or_else(|| old_usage.clone());
                    *duration_ms = duration_ms.or(*old_duration);
                }
            }
            self.decorate(&mut draft.kind);
            let unchanged = self.by_id.get(&draft.id).is_some_and(|index| {
                let item = &self.items[*index];
                item.kind == draft.kind && item.ts == draft.ts && item.turn_id == draft.turn_id
            });
            if !unchanged {
                events.push(self.apply_item(draft, true));
            }
        }
        events
    }

    fn align_acp_replay(&self, drafts: &mut Vec<ItemDraft>) {
        let visible = |kind: &ItemKind| {
            matches!(
                kind,
                ItemKind::UserMessage { .. } | ItemKind::AgentMessage { .. }
            )
        };
        let mut messages = HashMap::new();
        let mut turns = HashMap::new();
        let native_ids: HashSet<_> = drafts.iter().map(|draft| draft.id.as_str()).collect();
        for (stored, live) in drafts
            .iter()
            .filter(|draft| visible(&draft.kind))
            .zip(self.items.iter().filter(|item| visible(&item.kind)))
        {
            if stored.id != live.id
                && (stored.kind != live.kind
                    || native_ids.contains(live.id.as_str())
                    || self.by_id.contains_key(&stored.id))
            {
                break;
            }
            messages.insert(stored.id.clone(), live);
            if let (Some(stored_turn), Some(live_turn)) = (&stored.turn_id, &live.turn_id) {
                turns.insert(stored_turn.clone(), live_turn);
            }
        }
        let endings: HashMap<_, _> = self
            .items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::TurnEnd { .. }))
            .filter_map(|item| Some((item.turn_id.as_ref()?, item)))
            .collect();
        let mut represented_endings = HashSet::new();
        drafts.retain_mut(|draft| {
            if let Some(live) = messages.get(&draft.id) {
                draft.id.clone_from(&live.id);
                draft.ts = draft.ts.or(live.ts);
            }
            if let Some(turn) = draft.turn_id.as_ref().and_then(|id| turns.get(id)) {
                draft.turn_id = Some((*turn).clone());
                if matches!(draft.kind, ItemKind::TurnEnd { .. }) {
                    if !represented_endings.insert(*turn) {
                        return false;
                    }
                    if let Some(live) = endings.get(turn) {
                        draft.id.clone_from(&live.id);
                        if let (
                            ItemKind::TurnEnd {
                                usage,
                                duration_ms,
                                stop_reason,
                            },
                            ItemKind::TurnEnd {
                                usage: old_usage,
                                duration_ms: old_duration,
                                stop_reason: old_reason,
                            },
                        ) = (&mut draft.kind, &live.kind)
                        {
                            *usage = usage.take().or_else(|| old_usage.clone());
                            *duration_ms = duration_ms.or(*old_duration);
                            *stop_reason = *old_reason;
                        }
                        draft.ts = draft.ts.or(live.ts);
                    }
                }
            }
            true
        });
    }

    fn item(&mut self, draft: ItemDraft) -> ThreadEvent {
        self.apply_item(draft, false)
    }

    fn apply_item(&mut self, mut draft: ItemDraft, from_history: bool) -> ThreadEvent {
        if let ItemKind::ToolCall(call) = &draft.kind {
            if !call.child_thread_ids.is_empty() {
                self.native_children
                    .insert(call.call_id.clone(), call.child_thread_ids.clone());
            }
        }
        self.decorate(&mut draft.kind);
        let next = self.items.len();
        let index = *self.by_id.entry(draft.id.clone()).or_insert(next);
        let item = Item {
            id: draft.id,
            ordinal: index as u64 + 1,
            ts: draft.ts,
            turn_id: draft.turn_id,
            kind: draft.kind,
        };
        let update_turn_status = !from_history
            || index + 1 >= self.items.len()
            || !self.items[index + 1..]
                .iter()
                .any(|later| matches!(later.kind, ItemKind::TurnEnd { .. }));
        if let ItemKind::TurnEnd {
            stop_reason, usage, ..
        } = &item.kind
        {
            if update_turn_status {
                self.status = if *stop_reason == StopReason::Error {
                    ThreadStatus::Error
                } else {
                    ThreadStatus::Idle
                };
                if let Some(usage) = usage {
                    self.usage.last_turn = Some(usage.clone());
                }
            }
        }
        if let ItemKind::Request(request) = &item.kind {
            if request.resolution.is_none() {
                self.pending.insert(request.request_id.clone(), index);
            } else {
                self.pending.remove(&request.request_id);
            }
        }
        if index == next {
            self.items.push(item.clone());
            ThreadEvent::ItemAdded { item }
        } else {
            self.items[index] = item.clone();
            ThreadEvent::ItemUpdated { item }
        }
    }

    fn delta(&mut self, item_id: String, delta: String) -> Option<ThreadEvent> {
        let index = *self.by_id.get(&item_id)?;
        match &mut self.items[index].kind {
            ItemKind::AgentMessage { text, .. } | ItemKind::Reasoning { text, .. } => {
                text.push_str(&delta)
            }
            _ => return None,
        }
        Some(ThreadEvent::TextDelta { item_id, delta })
    }

    fn notice(&mut self, text: String) -> ThreadEvent {
        self.notice_with_level(NoticeLevel::Error, text)
    }

    fn info_notice(&mut self, text: String) -> ThreadEvent {
        self.notice_with_level(NoticeLevel::Info, text)
    }

    fn notice_with_level(&mut self, level: NoticeLevel, text: String) -> ThreadEvent {
        self.item(ItemDraft {
            id: format!("verre:notice:{}", self.items.len()),
            ts: Some(now()),
            turn_id: None,
            kind: ItemKind::Notice {
                level,
                text,
                detail: None,
            },
        })
    }

    fn set_options(&mut self, options: &[ConfigOption]) {
        self.options = options.to_vec();
        if let Some(model) = options
            .iter()
            .find(|option| option.category == ConfigCategory::Model)
        {
            self.summary.model = Some(model.current_value.clone());
        }
    }

    fn resolve(&mut self, id: &str, resolution: RequestResolution) -> Option<ThreadEvent> {
        let item = &mut self.items[self.pending.remove(id)?];
        if let ItemKind::Request(request) = &mut item.kind {
            request.resolution = Some(resolution);
        }
        if self.pending.is_empty() && self.status == ThreadStatus::AwaitingInput {
            self.status = ThreadStatus::Running;
        }
        Some(ThreadEvent::ItemUpdated { item: item.clone() })
    }

    fn cancel_pending(&mut self, core: &Core, matches: impl Fn(&Item) -> bool) {
        let requests: Vec<_> = self
            .pending
            .iter()
            .filter(|&(_, index)| matches(&self.items[*index]))
            .map(|(id, _)| id.clone())
            .collect();
        for request in requests {
            if let Some(event) = self.resolve(&request, RequestResolution::Cancelled) {
                self.emit(core, event);
            }
        }
    }

    fn update_status(&mut self) {
        self.summary.status = if self.pending.is_empty() {
            self.status
        } else {
            ThreadStatus::AwaitingInput
        };
    }

    fn emit(&mut self, core: &Core, event: ThreadEvent) {
        self.seq += 1;
        let event = Arc::new(ServerEvent::Thread {
            thread_id: self.summary.id.clone(),
            seq: self.seq,
            event,
        });
        if self.events.len() == RETAINED_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event.clone());
        core.broadcast(event);
    }

    fn changed(&mut self, core: &Core) {
        self.summary.updated_at = self.summary.updated_at.max(now());
        self.changed_without_bump(core);
    }

    fn changed_without_bump(&mut self, core: &Core) {
        self.emit(
            core,
            ThreadEvent::Summary {
                thread: core.overlay(self.summary.clone()),
            },
        );
        core.thread_changed(self.summary.clone());
    }
}

fn split_id(id: &str) -> Result<(&str, &str)> {
    id.split_once(':')
        .filter(|(agent, native)| !agent.is_empty() && !native.is_empty())
        .ok_or_else(|| bad_request("Expected a thread id of the form agent:native-id"))
}

fn remove_incompatible_option(thread_id: &str, options: &mut BTreeMap<String, String>, id: &str) {
    if !thread_id.starts_with("codex:") {
        return;
    }
    match id {
        "permissions" => options.remove("sandbox"),
        "sandbox" => options.remove("permissions"),
        _ => None,
    };
}

fn rollback_reload_error(error: AgentError) -> AgentError {
    AgentError::new(
        error.code,
        format!(
            "Conversation was rolled back, but history reload failed: {}",
            error.message
        ),
    )
}

fn new_summary(
    agent_id: &str,
    native_id: &str,
    title: String,
    cwd: Option<String>,
) -> ThreadSummary {
    let timestamp = now();
    let mut summary = ThreadSummary {
        id: format!("{agent_id}:{native_id}"),
        agent_id: agent_id.into(),
        native_id: native_id.into(),
        title,
        preview: None,
        cwd,
        created_at: Some(timestamp),
        updated_at: timestamp,
        source: ThreadSource::Verre,
        origin: None,
        status: ThreadStatus::Idle,
        live: false,
        resumable: true,
        parent: None,
        child_count: 0,
        model: None,
        archived: false,
        pinned: false,
        internal: false,
    };
    summary.internal = crate::storage::classified(&summary);
    summary
}

#[cfg(test)]
impl Sessions {
    pub(crate) fn insert_test_status(&self, core: &Arc<Core>, id: &str, status: ThreadStatus) {
        let mut summary = new_summary("codex", id, id.into(), None);
        summary.status = status;
        let state = Timeline::new(summary);
        self.insert(Arc::new(Thread {
            operations: AsyncMutex::new(()),
            state: Mutex::new(state),
            core: Arc::downgrade(core),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, turn_id: Option<&str>, text: &str) -> ItemDraft {
        ItemDraft {
            id: id.into(),
            ts: None,
            turn_id: turn_id.map(Into::into),
            kind: ItemKind::UserMessage {
                content: vec![ContentBlock::Text { text: text.into() }],
            },
        }
    }

    fn user_count(state: &Timeline) -> usize {
        state
            .items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::UserMessage { .. }))
            .count()
    }

    #[test]
    fn history_user_message_reuses_live_local_id() {
        let mut state = Timeline::new(new_summary("codex", "t1", "t1".into(), None));
        state.item(user("local-user:1", None, "question"));
        state.item(user("local-user:2", None, "second"));
        let events = state.merge_history(vec![
            user("native-1", Some("turn-1"), "question"),
            user("native-2", Some("turn-2"), "second"),
        ]);
        assert_eq!(user_count(&state), 2);
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| matches!(event, ThreadEvent::ItemUpdated { .. })));
        assert_eq!(state.items[0].id, "local-user:1");
        assert_eq!(state.items[0].turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn identical_history_user_messages_are_each_matched_once() {
        let mut state = Timeline::new(new_summary("codex", "t1", "t1".into(), None));
        state.item(user("local-user:1", None, "again"));
        state.merge_history(vec![
            user("native-1", Some("turn-1"), "again"),
            user("native-2", Some("turn-2"), "again"),
        ]);
        assert_eq!(user_count(&state), 2);
    }

    #[test]
    fn acp_replay_aligns_repeated_turns_by_order_and_keeps_live_completion() {
        let mut state = Timeline::new(new_summary("kiro", "session", "Test".into(), None));
        let items = |prefix: &str, duration_ms| {
            (0..2)
                .flat_map(|index| {
                    let turn = format!("{prefix}:{index}");
                    [
                        user(&format!("{turn}:user"), Some(&turn), "Repeat"),
                        ItemDraft {
                            id: format!("{turn}:answer"),
                            turn_id: Some(turn.clone()),
                            ts: None,
                            kind: ItemKind::AgentMessage {
                                text: "Done".into(),
                                streaming: false,
                            },
                        },
                        ItemDraft::turn_end(&turn, StopReason::EndTurn, None, duration_ms, None),
                    ]
                })
                .collect::<Vec<_>>()
        };
        for item in items("acp:live", Some(42)) {
            state.item(item);
        }
        let mut replay = items("replay:stored", None);
        replay.push(user("external", Some("replay:external"), "New"));
        assert_eq!(state.merge_history(replay.clone()).len(), 1);
        assert!(state.merge_history(replay).is_empty());
        assert_eq!(state.items.len(), 7);
        assert!(matches!(
            state.items[2].kind,
            ItemKind::TurnEnd {
                duration_ms: Some(42),
                ..
            }
        ));
        assert!(matches!(
            state.items[5].kind,
            ItemKind::TurnEnd {
                duration_ms: Some(42),
                ..
            }
        ));
    }

    fn assistant(id: &str, turn_id: Option<&str>, text: &str) -> ItemDraft {
        ItemDraft {
            id: id.into(),
            ts: None,
            turn_id: turn_id.map(Into::into),
            kind: ItemKind::AgentMessage {
                text: text.into(),
                streaming: false,
            },
        }
    }

    #[test]
    fn consecutive_identical_user_messages_stay_deduped_across_rescans() {
        let mut state = Timeline::new(new_summary("codex", "t1", "t1".into(), None));
        state.item(user("local-user:1", None, "ping"));
        state.item(user("local-user:2", None, "ping"));

        let first_rescan = vec![
            user("native-1", Some("turn-1"), "ping"),
            user("native-2", Some("turn-2"), "ping"),
        ];
        assert_eq!(state.merge_history(first_rescan.clone()).len(), 2);
        assert_eq!(user_count(&state), 2);
        assert!(state.merge_history(first_rescan).is_empty());

        let full_rescan = vec![
            user("native-1", Some("turn-1"), "ping"),
            user("native-2", Some("turn-2"), "ping"),
            user("native-3", Some("turn-3"), "ping"),
        ];
        assert_eq!(state.merge_history(full_rescan.clone()).len(), 1);
        assert_eq!(user_count(&state), 3);
        assert!(state.merge_history(full_rescan).is_empty());
    }

    #[test]
    fn steer_consecutive_user_messages_and_synthetic_turn_ids_reconcile() {
        let mut state = Timeline::new(new_summary("codex", "t1", "t1".into(), None));
        state.item(user("local-1", Some("live-turn-1"), "start"));
        state.item(user("local-2", Some("live-turn-1"), "steer"));

        let history = vec![
            user("native-u1", Some("turn-1"), "start"),
            user("native-u2", Some("turn-1"), "steer"),
            assistant("native-a1", Some("turn-1"), "handled both"),
        ];
        let events = state.merge_history(history.clone());
        assert_eq!(events.len(), 3);
        assert_eq!(user_count(&state), 2);
        assert_eq!(state.items.len(), 3);
        assert_eq!(state.items[0].id, "local-1");
        assert_eq!(state.items[1].id, "local-2");
        assert_eq!(state.items[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(state.items[1].turn_id.as_deref(), Some("turn-1"));
        assert!(state.merge_history(history).is_empty());
    }

    #[test]
    fn detach_rescan_preserves_live_turn_end_metadata_when_history_omits_it() {
        let mut state = Timeline::new(new_summary("claude", "c1", "c1".into(), None));
        let usage = TokenUsage {
            input: Some(100),
            cached_input: None,
            output: Some(25),
            reasoning: None,
            context_used: Some(125),
            context_window: Some(200_000),
        };
        state.item(ItemDraft {
            id: "u1".into(),
            ts: Some(1000),
            turn_id: Some("t1".into()),
            kind: ItemKind::UserMessage {
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
        });
        state.item(ItemDraft::turn_end(
            "t1",
            StopReason::EndTurn,
            Some(2000),
            Some(320),
            Some(usage.clone()),
        ));

        let events = state.merge_history(vec![
            user("u1", Some("t1"), "hi"),
            ItemDraft::turn_end("t1", StopReason::EndTurn, None, None, None),
        ]);
        assert!(events.is_empty());
        assert_eq!(state.items[0].ts, Some(1000));
        assert_eq!(state.items[1].ts, Some(2000));
        assert!(matches!(
            &state.items[1].kind,
            ItemKind::TurnEnd {
                duration_ms: Some(320),
                usage: Some(u),
                ..
            } if *u == usage
        ));
    }

    #[test]
    fn updating_earlier_error_turn_end_does_not_overwrite_latest_idle_status() {
        let mut state = Timeline::new(new_summary("codex", "t1", "t1".into(), None));
        state.item(user("u1", Some("t1"), "first"));
        state.item(ItemDraft::turn_end(
            "t1",
            StopReason::Error,
            Some(1000),
            None,
            None,
        ));
        state.item(user("u2", Some("t2"), "second"));
        state.item(ItemDraft::turn_end(
            "t2",
            StopReason::EndTurn,
            Some(2000),
            Some(50),
            None,
        ));
        state.update_status();
        assert_eq!(state.summary.status, ThreadStatus::Idle);

        state.merge_history(vec![
            user("u1", Some("t1"), "first"),
            ItemDraft::turn_end("t1", StopReason::Error, Some(1000), Some(12), None),
            user("u2", Some("t2"), "second"),
            ItemDraft::turn_end("t2", StopReason::EndTurn, Some(2000), Some(50), None),
        ]);
        state.update_status();
        assert_eq!(state.summary.status, ThreadStatus::Idle);
    }

    #[test]
    fn live_reused_turn_end_id_still_marks_thread_idle() {
        let mut state = Timeline::new(new_summary("codex", "t1", "t1".into(), None));
        state.item(ItemDraft::turn_end(
            "end",
            StopReason::EndTurn,
            Some(1000),
            None,
            None,
        ));
        state.item(ItemDraft::turn_end(
            "interrupt",
            StopReason::Cancelled,
            Some(1500),
            None,
            None,
        ));
        state.status = ThreadStatus::Running;
        state.update_status();
        assert_eq!(state.summary.status, ThreadStatus::Running);

        state.item(ItemDraft::turn_end(
            "end",
            StopReason::EndTurn,
            Some(2000),
            Some(40),
            None,
        ));
        state.update_status();
        assert_eq!(state.status, ThreadStatus::Idle);
        assert_eq!(state.summary.status, ThreadStatus::Idle);
    }
}

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use tokio::sync::{mpsc, Mutex as AsyncMutex};
use vetro_agent_api::{AgentError, ItemDraft, Result, Session, SessionEvent, StartRequest};
use vetro_proto::{rpc::*, *};

use crate::{bad_request, internal, links::Projection, not_found, now, page_limit, Core};

const DORMANT_THREADS: usize = 64;
const RETAINED_EVENTS: usize = 256;

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
    queue: VecDeque<Vec<ContentBlock>>,
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
        let threads = self.snapshot();
        for thread in threads {
            thread.maintain(idle).await;
        }
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
        let thread = self.thread(id).await?;
        Ok((thread.snapshot(limit), thread))
    }

    pub(crate) async fn thread(self: &Arc<Self>, id: &str) -> Result<Arc<Thread>> {
        if let Some(thread) = self.sessions.get(id) {
            return Ok(thread);
        }
        let (agent_id, native_id) = split_id(id)?;
        let agent = self.registry.get(agent_id)?;
        let indexed = self.store.get(id)?;
        let unknown = indexed.is_none();
        let summary = indexed.unwrap_or_else(|| {
            let mut summary = new_summary(agent_id, native_id, native_id.into(), None);
            summary.source = ThreadSource::Unknown;
            summary
        });
        let transcript = {
            let _guard = self.registry.enter(agent_id, true).await;
            self.read_history_items(&agent, &summary).await
        };
        let transcript = self.auth_result(agent_id, transcript).await?;
        let mut state = Timeline::new(summary);
        state.set_children(self.store.children(id)?);
        if unknown {
            state.summary.resumable = self.agent_info(agent_id).await?.capabilities.resume;
        }
        for draft in transcript.items {
            state.item(draft);
        }
        state.update_status();
        let candidate = Arc::new(Thread::new(self, state));
        Ok(self.sessions.insert(candidate))
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        params: ThreadStartParams,
    ) -> Result<(ThreadSummary, Arc<Thread>)> {
        self.check_agent_enabled(&params.agent_id)?;
        let agent = self.registry.get(&params.agent_id)?;
        let guard = self.registry.enter(&params.agent_id, true).await;
        let cwd = crate::files::directory(Path::new(&params.cwd))?;
        let title = params
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
        let (sink, events) = mpsc::unbounded_channel();
        let session = agent
            .start(
                StartRequest {
                    cwd: cwd.clone(),
                    content: params.content,
                    options: params.options,
                },
                sink,
            )
            .await;
        let session = match session {
            Ok(session) => session,
            Err(error) => {
                drop(guard);
                return self.auth_result(&params.agent_id, Err(error)).await;
            }
        };
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
        let mut state = Timeline::new(summary.clone());
        state.session = Some(session.clone());
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

    pub(crate) fn refresh_history(&self, mut summary: ThreadSummary, items: Vec<ItemDraft>) {
        let core = self.core();
        let Ok(children) = core.store.children(&summary.id) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if state.session.is_some() || state.index_dirty {
            return;
        }
        state.set_children(children);
        if summary.created_at.is_none() {
            summary.created_at = state.summary.created_at;
        }
        let old = state.summary.clone();
        state.status = summary.status;
        state.summary = summary;
        for mut draft in items {
            state.decorate(&mut draft.kind);
            let unchanged = state.by_id.get(&draft.id).is_some_and(|index| {
                let item = &state.items[*index];
                item.kind == draft.kind && item.ts == draft.ts && item.turn_id == draft.turn_id
            });
            if !unchanged {
                let event = state.item(draft);
                state.emit(&core, event);
            }
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
            || state.summary.child_count != summary.child_count;
        state.summary.parent.clone_from(&summary.parent);
        state.summary.child_count = summary.child_count;
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
        let event = match event {
            SessionEvent::Item(draft) => {
                state.index_dirty |= matches!(
                    draft.kind,
                    ItemKind::UserMessage { .. }
                        | ItemKind::AgentMessage { .. }
                        | ItemKind::ToolCall(_)
                        | ItemKind::TurnEnd { .. }
                );
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
                state.usage = usage;
                None
            }
            SessionEvent::AuthRequired => None,
            SessionEvent::Closed { error } => {
                closed_session = state.session.take();
                state.summary.live = false;
                state.generation += 1;
                state.queue.clear();
                state.cancel_pending(&core);
                state.status = if error.is_some() {
                    ThreadStatus::Error
                } else {
                    ThreadStatus::Idle
                };
                error.map(|text| state.notice(text))
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
            state.changed(&core);
        }
        let still_live = state.session.is_some();
        if let Some(session) = closed_session {
            self.persist_log(&core, &state.summary.id, &session);
        }
        still_live
    }

    async fn attach(self: &Arc<Self>) -> Result<Arc<dyn Session>> {
        let summary = {
            let state = self.state.lock().unwrap();
            if let Some(session) = &state.session {
                return Ok(session.clone());
            }
            state.summary.clone()
        };
        if !summary.resumable {
            return Err(AgentError::new(
                ErrorCode::NotResumable,
                "Thread is read-only",
            ));
        }
        let core = self.core();
        core.check_agent_enabled(&summary.agent_id)?;
        let guard = core.registry.enter(&summary.agent_id, true).await;
        let (sink, events) = mpsc::unbounded_channel();
        let result = core
            .registry
            .get(&summary.agent_id)?
            .attach(
                &summary.native_id,
                summary.cwd.as_deref().map(Path::new),
                sink,
            )
            .await;
        if let Err(error) = &result {
            if error.code == ErrorCode::NotResumable {
                let mut state = self.state.lock().unwrap();
                state.summary.resumable = false;
                state.changed(&core);
            }
        }
        let session = match result {
            Ok(session) => session,
            Err(error) => {
                drop(guard);
                return core.auth_result(&summary.agent_id, Err(error)).await;
            }
        };
        if session.native_id() != summary.native_id {
            session.detach().await;
            return Err(internal("Agent attached to a different native session"));
        }
        let mut state = self.state.lock().unwrap();
        state.session = Some(session.clone());
        state.summary.live = true;
        state.last_activity = Instant::now();
        state.raw_saved_at = None;
        state.changed(&core);
        self.listen(state.generation, events);
        Ok(session)
    }

    pub(crate) async fn send(self: &Arc<Self>, content: Vec<ContentBlock>) -> Result<()> {
        let _operation = self.operations.lock().await;
        let core = self.core();
        let agent_id = self.state.lock().unwrap().summary.agent_id.clone();
        let steer = core
            .agent_info(&agent_id)
            .await?
            .capabilities
            .mid_turn_input;
        {
            let mut state = self.state.lock().unwrap();
            if state.session.is_some()
                && (!state.queue.is_empty()
                    || (state.active() && !steer)
                    || !state.pending.is_empty())
            {
                if state.queue.len() >= 32 {
                    return Err(AgentError::new(
                        ErrorCode::Busy,
                        "Thread input queue is full",
                    ));
                }
                state.queue.push_back(content);
                return Ok(());
            }
        }
        let session = self.attach().await?;
        self.send_now(&core, session, content).await
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
        if let Err(error) = session.send(content).await {
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
                (session, state.queue.pop_front().unwrap())
            };
            if self.send_now(&core, session, content).await.is_ok() {
                return;
            }
        }
    }

    pub(crate) async fn interrupt(&self) -> Result<()> {
        let _operation = self.operations.lock().await;
        let session = {
            let mut state = self.state.lock().unwrap();
            state.queue.clear();
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
        let session = self.attach().await?;
        let options = session.set_option(&params.option_id, &params.value).await?;
        let generation = self.state.lock().unwrap().generation;
        self.handle(generation, SessionEvent::Options(options.clone()));
        Ok(OptionsResult { options })
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

    pub(crate) async fn detach(&self) -> Result<()> {
        let _operation = self.operations.lock().await;
        self.release(false, None).await
    }

    async fn release(&self, force: bool, idle: Option<Duration>) -> Result<()> {
        let core = self.core();
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
        let _guard = core.registry.enter(&agent_id, false).await;
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
            state.queue.clear();
            state.cancel_pending(&core);
            state.summary.live = false;
            state.status = ThreadStatus::Idle;
            state.update_status();
            state.changed(&core);
            (session, state.summary.id.clone())
        };
        session.detach().await;
        self.persist_log(&core, &id, &session);
        core.registry.request_release(&agent_id);
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
        entries.drain(
            ..entries
                .len()
                .saturating_sub(page_limit(params.limit, 500, 5000)),
        );
        Ok(ThreadRawResult { entries })
    }

    fn persist_log(&self, core: &Core, id: &str, session: &Arc<dyn Session>) {
        if let Err(error) = core.home.write_raw(id, &session.wire_log().snapshot()) {
            core.home.log(&format!("Wire log write failed: {error}"));
        }
    }
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

    fn item(&mut self, mut draft: ItemDraft) -> ThreadEvent {
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
        if let ItemKind::TurnEnd {
            stop_reason, usage, ..
        } = &item.kind
        {
            self.status = if *stop_reason == StopReason::Error {
                ThreadStatus::Error
            } else {
                ThreadStatus::Idle
            };
            if let Some(usage) = usage {
                self.usage.last_turn = Some(usage.clone());
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
        self.item(ItemDraft {
            id: format!("vetro:notice:{}", self.items.len()),
            ts: Some(now()),
            turn_id: None,
            kind: ItemKind::Notice {
                level: NoticeLevel::Error,
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

    fn cancel_pending(&mut self, core: &Core) {
        let requests: HashSet<_> = self.pending.keys().cloned().collect();
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

fn new_summary(
    agent_id: &str,
    native_id: &str,
    title: String,
    cwd: Option<String>,
) -> ThreadSummary {
    let timestamp = now();
    ThreadSummary {
        id: format!("{agent_id}:{native_id}"),
        agent_id: agent_id.into(),
        native_id: native_id.into(),
        title,
        preview: None,
        cwd,
        created_at: Some(timestamp),
        updated_at: timestamp,
        source: ThreadSource::Vetro,
        origin: None,
        status: ThreadStatus::Idle,
        live: false,
        resumable: true,
        parent: None,
        child_count: 0,
        model: None,
        archived: false,
    }
}

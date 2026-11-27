use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use tokio::sync::watch;
use vetro_agent_api::{AgentError, Result};
use vetro_proto::{rpc::*, PROTOCOL_VERSION};

use super::{
    auth::{self, Client},
    HttpState,
};
use crate::{
    bad_request, files, internal,
    service::{check_project_path, in_project},
    sessions::Thread,
};

#[derive(Default)]
pub(super) struct Connection {
    pub(super) hello: bool,
    subscriptions: HashMap<String, Subscription>,
}

struct Subscription {
    thread: Arc<Thread>,
    seq: u64,
}

type MessageKey = (String, String);
type StartOutcome = Option<Result<String>>;
type StartReceipt = watch::Receiver<StartOutcome>;

#[derive(Default)]
pub(super) struct StartReceipts(Mutex<VecDeque<(MessageKey, StartReceipt)>>);

impl StartReceipts {
    fn reserve(
        &self,
        key: MessageKey,
        recover_only: bool,
    ) -> Result<(StartReceipt, Option<watch::Sender<StartOutcome>>)> {
        const MAX_RECEIPTS: usize = 256;
        let mut receipts = self.0.lock().unwrap();
        if let Some((_, receipt)) = receipts.iter().find(|(existing, _)| existing == &key) {
            return Ok((receipt.clone(), None));
        }
        if recover_only {
            return Err(delivery_unknown());
        }
        if receipts.len() == MAX_RECEIPTS {
            let finished = receipts
                .iter()
                .position(|(_, receipt)| {
                    receipt.borrow().is_some() || receipt.has_changed().is_err()
                })
                .ok_or_else(|| {
                    AgentError::new(ErrorCode::Busy, "Too many pending thread starts")
                })?;
            receipts.remove(finished);
        }
        let (sender, receipt) = watch::channel(None);
        receipts.push_back((key, receipt.clone()));
        Ok((receipt, Some(sender)))
    }
}

fn delivery_unknown() -> AgentError {
    AgentError::new(
        ErrorCode::DeliveryUnknown,
        "Vetro could not confirm delivery. Check the thread history before sending again.",
    )
}

fn client_message_key(
    client: &Client,
    id: Option<String>,
    recover_only: bool,
) -> Result<Option<MessageKey>> {
    let id = id.filter(|id| !id.is_empty());
    if id.as_ref().is_some_and(|id| id.len() > 128) {
        return Err(bad_request("Client message ID is too long"));
    }
    if recover_only && id.is_none() {
        return Err(bad_request(
            "A client message ID is required to recover delivery",
        ));
    }
    Ok(id.map(|id| (client.device_key().to_owned(), id)))
}

impl Connection {
    pub(super) fn accepts(&mut self, event: &ServerEvent) -> bool {
        if !self.hello {
            return false;
        }
        if let ServerEvent::Thread { thread_id, seq, .. } = event {
            let Some(subscription) = self.subscriptions.get_mut(thread_id) else {
                return false;
            };
            if *seq <= subscription.seq {
                return false;
            }
            subscription.seq = *seq;
        }
        true
    }

    fn subscribe(&mut self, id: String, thread: Arc<Thread>, seq: u64) {
        self.subscriptions.insert(id, Subscription { thread, seq });
    }

    pub(super) fn merge(&mut self, completed: Self) -> Option<Vec<Arc<ServerEvent>>> {
        let mut replay = Some(Vec::new());
        for (id, subscription) in completed.subscriptions {
            match subscription.thread.events_after(subscription.seq) {
                Some(events) => {
                    if let Some(replay) = &mut replay {
                        replay.extend(events);
                    }
                }
                None => replay = None,
            }
            self.subscriptions.insert(id, subscription);
        }
        replay
    }
}

pub(super) async fn dispatch(
    state: &HttpState,
    client: &Client,
    connection: &mut Connection,
    request: RpcRequest,
) -> RpcResponse {
    let id = request.id;
    match call(state, client, connection, request).await {
        Ok(result) => RpcResponse {
            id,
            result: Some(result),
            error: None,
        },
        Err(error) => RpcResponse {
            id,
            result: None,
            error: Some(error.into()),
        },
    }
}

async fn call(
    state: &HttpState,
    client: &Client,
    connection: &mut Connection,
    request: RpcRequest,
) -> Result<Value> {
    let core = &state.core;
    let params = request.params;
    let method = request.method.as_str();
    if !connection.hello && method != "app.hello" {
        return Err(bad_request("Call app.hello first"));
    }
    match method {
        "app.hello" => {
            if connection.hello {
                return Err(bad_request("app.hello already completed"));
            }
            let params: HelloParams = parse(params)?;
            if params.protocol_version != PROTOCOL_VERSION {
                return Err(bad_request(format!(
                    "Expected protocol version {PROTOCOL_VERSION}"
                )));
            }
            connection.hello = true;
            output(core.app_info(client.remote()))
        }
        "app.ping" => empty(),
        "agents.list" => output(AgentsResult {
            agents: core.agents(false).await,
        }),
        "agents.refresh" => {
            let agents = core.agents(true).await;
            core.request_history_refresh();
            output(AgentsResult { agents })
        }
        "agents.authenticate" => {
            client.desktop()?;
            output(core.authenticate(parse(params)?).await?)
        }
        "agents.options" => {
            let mut params: AgentOptionsParams = parse(params)?;
            if client.remote() {
                params.cwd = match params
                    .cwd
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(cwd) => Some(checked_cwd(state, cwd)?),
                    None => None,
                };
            }
            output(core.agent_options(params).await?)
        }
        "projects.list" => output(ProjectsResult {
            projects: core.projects()?,
        }),
        "projects.add" => {
            client.desktop()?;
            output(core.add_project(parse(params)?)?)
        }
        "projects.remove" => {
            client.desktop()?;
            core.remove_project(parse(params)?)?;
            empty()
        }
        "projects.update" => output(core.update_project(parse(params)?)?),
        "threads.list" => output(core.threads(parse(params)?)?),
        "threads.active" => output(core.active_threads()?),
        "threads.open" => {
            let params: ThreadOpenParams = parse(params)?;
            let (snapshot, thread) = core.open(&params.thread_id, params.limit).await?;
            connection.subscribe(params.thread_id, thread, snapshot.seq);
            output(snapshot)
        }
        "threads.close" => {
            let params: ThreadIdParams = parse(params)?;
            connection.subscriptions.remove(&params.thread_id);
            empty()
        }
        "threads.items" => {
            let params: ThreadItemsParams = parse(params)?;
            let thread = core.thread(&params.thread_id).await?;
            output(thread.items(params.before, params.limit))
        }
        "threads.start" => {
            let mut params: ThreadStartParams = parse(params)?;
            let recover_only = params.recover_only.unwrap_or_default();
            let key = client_message_key(client, params.client_message_id.take(), recover_only)?;
            if !recover_only && client.remote() && !params.cwd.trim().is_empty() {
                params.cwd = checked_cwd(state, &params.cwd)?;
            }
            let (summary, thread) = if let Some(key) = key {
                let (mut receipt, sender) = state.starts.reserve(key, recover_only)?;
                if let Some(sender) = sender {
                    let core = core.clone();
                    let mut stopping = state.stopping.clone();
                    tokio::spawn(async move {
                        let result = tokio::select! {
                            biased;
                            _ = stopping.wait_for(|stopping| *stopping) => {
                                Err(AgentError::new(ErrorCode::Busy, "Vetro is shutting down"))
                            }
                            result = core.start(params) => result.map(|(summary, _)| summary.id),
                        };
                        sender.send_replace(Some(result));
                    });
                }
                let outcome = receipt
                    .wait_for(Option::is_some)
                    .await
                    .map_err(|_| delivery_unknown())?
                    .clone();
                let id = outcome.ok_or_else(delivery_unknown)??;
                let thread = core.thread(&id).await?;
                (thread.snapshot(Some(1)).thread, thread)
            } else {
                core.start(params).await?
            };
            connection.subscribe(summary.id.clone(), thread, 0);
            output(ThreadResult { thread: summary })
        }
        "threads.fork" => {
            let params: ThreadForkParams = parse(params)?;
            let (summary, thread) = core.fork_thread(params).await?;
            let seq = thread.snapshot(Some(1)).seq;
            connection.subscribe(summary.id.clone(), thread, seq);
            output(ThreadResult { thread: summary })
        }
        "threads.rollback" => {
            let (snapshot, thread) = core.rollback_thread(parse(params)?).await?;
            connection.subscribe(snapshot.thread.id.clone(), thread, snapshot.seq);
            output(snapshot)
        }
        "threads.rewindFiles" => output(core.rewind_thread_files(parse(params)?).await?),
        "threads.send" => {
            let params: ThreadSendParams = parse(params)?;
            let recover_only = params.recover_only.unwrap_or_default();
            let key = client_message_key(client, params.client_message_id, recover_only)?;
            if recover_only {
                let thread = core
                    .sessions
                    .get(&params.thread_id)
                    .ok_or_else(delivery_unknown)?;
                let key = key.ok_or_else(delivery_unknown)?;
                return if thread.confirm_send(&key).await {
                    empty()
                } else {
                    Err(delivery_unknown())
                };
            }
            let thread = core.thread(&params.thread_id).await?;
            if let Some(key) = key {
                let mut stopping = state.stopping.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        biased;
                        _ = stopping.wait_for(|stopping| *stopping) => {
                            Err(AgentError::new(ErrorCode::Busy, "Vetro is shutting down"))
                        }
                        result = thread.send(params.content, Some(key)) => result,
                    }
                })
                .await
                .map_err(internal)??;
            } else {
                thread.send(params.content, None).await?;
            }
            empty()
        }
        "threads.cancelQueued" => {
            let params: ThreadQueueCancelParams = parse(params)?;
            core.thread(&params.thread_id)
                .await?
                .cancel_queued(&params.queue_id);
            empty()
        }
        "threads.interrupt" => {
            let params: ThreadIdParams = parse(params)?;
            core.thread(&params.thread_id).await?.interrupt().await?;
            empty()
        }
        "threads.setOption" => {
            let params: ThreadSetOptionParams = parse(params)?;
            output(
                core.thread(&params.thread_id)
                    .await?
                    .set_option(params)
                    .await?,
            )
        }
        "threads.respond" => {
            let params: ThreadRespondParams = parse(params)?;
            core.thread(&params.thread_id)
                .await?
                .respond(params)
                .await?;
            empty()
        }
        "threads.update" => output(ThreadResult {
            thread: core.update_thread(parse(params)?)?,
        }),
        "threads.detach" => {
            let params: ThreadDetachParams = parse(params)?;
            core.thread(&params.thread_id)
                .await?
                .detach(params.force.unwrap_or(false))
                .await?;
            empty()
        }
        "threads.raw" => {
            let params: ThreadRawParams = parse(params)?;
            output(core.thread(&params.thread_id).await?.raw(params)?)
        }
        "search.threads" => output(core.search(parse(params)?)?),
        "fs.list" => {
            let params: FsListParams = parse(params)?;
            let path = params
                .path
                .map(PathBuf::from)
                .unwrap_or_else(|| core.home.user_home().to_owned());
            if client.remote() {
                let path = remote_path(state, &path)?;
                let roots = core.projects()?;
                let mut result = tokio::task::spawn_blocking(move || files::list(&path))
                    .await
                    .map_err(internal)??;
                if result
                    .parent
                    .as_ref()
                    .is_some_and(|parent| !in_project(Path::new(parent), &roots))
                {
                    result.parent = None;
                }
                return output(result);
            }
            output(
                tokio::task::spawn_blocking(move || files::list(&path))
                    .await
                    .map_err(internal)??,
            )
        }
        "fs.find" => {
            let mut params: FsFindParams = parse(params)?;
            if client.remote() {
                params.cwd = remote_path(state, Path::new(&params.cwd))?
                    .to_string_lossy()
                    .into_owned();
            }
            output(
                tokio::task::spawn_blocking(move || files::find(params))
                    .await
                    .map_err(internal)??,
            )
        }
        "workspace.gitStatus" => {
            let params: WorkspacePathParams = parse(params)?;
            let cwd = workspace_cwd(state, client, &params.cwd)?;
            output(core.git_status(&cwd, client.remote()).await?)
        }
        "workspace.gitDiff" => {
            let params: GitDiffParams = parse(params)?;
            let cwd = workspace_cwd(state, client, &params.cwd)?;
            output(core.git_diff(&cwd, &params.path, client.remote()).await?)
        }
        "workspace.readFile" => output(
            core.read_workspace_file(parse(params)?, client.remote())
                .await?,
        ),
        "workspace.worktrees" => {
            let params: WorkspacePathParams = parse(params)?;
            let cwd = workspace_cwd(state, client, &params.cwd)?;
            output(core.worktrees(&cwd, client.remote()).await?)
        }
        "workspace.openExternal" => {
            client.desktop()?;
            core.open_external(parse(params)?).await?;
            empty()
        }
        "app.notify" => {
            client.desktop()?;
            core.notify(parse(params)?);
            empty()
        }
        "app.setBadge" => {
            client.desktop()?;
            let params: BadgeParams = parse(params)?;
            core.set_badge(params.count);
            empty()
        }
        "settings.get" => {
            let mut settings = core.home.settings();
            if client.remote() {
                for agent in settings.agents.values_mut() {
                    agent
                        .env
                        .values_mut()
                        .for_each(|value| *value = "***".into());
                }
                for agent in &mut settings.custom_acp_agents {
                    agent
                        .env
                        .values_mut()
                        .for_each(|value| *value = "***".into());
                }
            }
            output(settings)
        }
        "prefs.get" => output(Prefs {
            prefs: core.home.prefs(client.device_key()),
        }),
        "prefs.set" => {
            let params: Prefs = parse(params)?;
            core.prefs_set(client.device_key(), params.prefs)?;
            empty()
        }
        "settings.update" => {
            client.desktop()?;
            output(core.update_settings(parse(params)?).await?)
        }
        "remote.status" => {
            if !client.remote() {
                state.auth.refresh(state).await;
            }
            output(auth::status(state, Some(client)))
        }
        "remote.set" => {
            client.desktop()?;
            state.auth.set(state, parse(params)?).await?;
            let status = auth::status(state, Some(client));
            core.publish(ServerEvent::RemoteUpdated {
                status: status.clone(),
            });
            output(status)
        }
        "remote.createPairing" => {
            client.desktop()?;
            let pairing = state.auth.create_pairing()?;
            core.publish(ServerEvent::RemoteUpdated {
                status: auth::status(state, None),
            });
            output(pairing)
        }
        "remote.revokeDevice" => {
            let params: RemoteRevokeParams = parse(params)?;
            output(auth::revoke(state, client, &params.device_id)?)
        }
        "voice.status" => output(core.voice_status()?),
        "voice.install" => {
            client.desktop()?;
            output(core.voice_install(parse(params)?).await?)
        }
        "voice.uninstall" => {
            client.desktop()?;
            output(core.voice_uninstall(parse(params)?).await?)
        }
        "voice.transcribe" => output(core.voice_transcribe(parse(params)?).await?),
        _ => Err(AgentError::new(
            ErrorCode::NotFound,
            format!("Unknown RPC method: {method}"),
        )),
    }
}

fn checked_cwd(state: &HttpState, cwd: &str) -> Result<String> {
    Ok(remote_path(state, Path::new(cwd))?
        .to_string_lossy()
        .into_owned())
}

fn workspace_cwd(state: &HttpState, client: &Client, cwd: &str) -> Result<PathBuf> {
    let path = Path::new(cwd);
    if client.remote() {
        remote_path(state, path)
    } else {
        Ok(path.to_owned())
    }
}

fn remote_path(state: &HttpState, path: &Path) -> Result<PathBuf> {
    let forbidden = || {
        AgentError::new(
            ErrorCode::Unauthorized,
            "Path is outside known project roots",
        )
    };
    let path = files::directory(path).map_err(|_| forbidden())?;
    check_project_path(&path, &state.core.projects()?)?;
    Ok(path)
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T> {
    let params = if params.is_null() {
        Value::Object(Default::default())
    } else {
        params
    };
    serde_json::from_value(params)
        .map_err(|error| bad_request(format!("Invalid parameters: {error}")))
}

#[cfg(test)]
#[test]
fn voice_model_params_allow_legacy_and_targeted_requests() {
    use serde_json::json;

    for params in [Value::Null, json!({}), json!({"modelId": null})] {
        assert!(parse::<VoiceModelParams>(params)
            .unwrap()
            .model_id
            .is_none());
    }
    let params = parse::<VoiceModelParams>(json!({"modelId": "qwen3-asr"})).unwrap();
    assert_eq!(params.model_id.as_deref(), Some("qwen3-asr"));
}

fn output(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(internal)
}

fn empty() -> Result<Value> {
    output(Empty::default())
}

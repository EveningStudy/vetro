use std::{collections::HashMap, path::PathBuf, sync::Arc};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use vetro_agent_api::{AgentError, Result};
use vetro_proto::{rpc::*, PROTOCOL_VERSION};

use super::{
    auth::{self, Client},
    HttpState,
};
use crate::{bad_request, files, internal, sessions::Thread};

#[derive(Default)]
pub(super) struct Connection {
    hello: bool,
    subscriptions: HashMap<String, Subscription>,
}

struct Subscription {
    _thread: Arc<Thread>,
    seq: u64,
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
        self.subscriptions.insert(
            id,
            Subscription {
                _thread: thread,
                seq,
            },
        );
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
            let params: HelloParams = parse(params)?;
            if params.protocol_version != PROTOCOL_VERSION {
                return Err(bad_request(format!(
                    "Expected protocol version {PROTOCOL_VERSION}"
                )));
            }
            connection.hello = true;
            output(core.app_info(client.remote()))
        }
        "agents.list" => output(AgentsResult {
            agents: core.agents(false).await,
        }),
        "agents.refresh" => {
            let agents = core.agents(true).await;
            core.request_history_refresh();
            output(AgentsResult { agents })
        }
        "agents.authenticate" => {
            core.authenticate(parse(params)?).await?;
            empty()
        }
        "agents.options" => output(core.agent_options(parse(params)?).await?),
        "projects.list" => output(ProjectsResult {
            projects: core.projects()?,
        }),
        "projects.add" => output(core.add_project(parse(params)?)?),
        "projects.update" => output(core.update_project(parse(params)?)?),
        "threads.list" => output(core.threads(parse(params)?)?),
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
            let (summary, thread) = core.start(parse(params)?).await?;
            connection.subscribe(summary.id.clone(), thread, 0);
            output(ThreadResult { thread: summary })
        }
        "threads.send" => {
            let params: ThreadSendParams = parse(params)?;
            core.thread(&params.thread_id)
                .await?
                .send(params.content)
                .await?;
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
            let params: ThreadIdParams = parse(params)?;
            core.thread(&params.thread_id).await?.detach().await?;
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
            output(
                tokio::task::spawn_blocking(move || files::list(&path))
                    .await
                    .map_err(internal)??,
            )
        }
        "fs.find" => {
            let params: FsFindParams = parse(params)?;
            output(
                tokio::task::spawn_blocking(move || files::find(params))
                    .await
                    .map_err(internal)??,
            )
        }
        "settings.get" => output(core.home.settings()),
        "settings.update" => output(core.update_settings(parse(params)?)?),
        "remote.status" => output(auth::status(state, client)),
        "remote.set" => {
            client.desktop()?;
            let _: RemoteSetParams = parse(params)?;
            output(auth::status(state, client))
        }
        "remote.createPairing" => {
            client.desktop()?;
            output(state.auth.create_pairing()?)
        }
        "remote.revokeDevice" => {
            let params: RemoteRevokeParams = parse(params)?;
            output(auth::revoke(state, client, &params.device_id)?)
        }
        _ => Err(AgentError::new(
            ErrorCode::NotFound,
            format!("Unknown RPC method: {method}"),
        )),
    }
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

fn output(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(internal)
}

fn empty() -> Result<Value> {
    output(Empty::default())
}

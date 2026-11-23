use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use tokio::sync::mpsc;
use vetro_agent_api::{Agent, Session, SessionEvent, StartRequest};
use vetro_core::Core;
use vetro_proto::{
    ContentBlock, InteractionRequest, ItemKind, PermissionOptionKind, RequestBody,
    RequestResolution, StopReason,
};

use crate::{cwd, Result};

#[derive(clap::Args)]
pub(crate) struct Args {
    agent: String,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    resume: Option<String>,
    #[arg(long)]
    raw: bool,
    #[arg(long)]
    yes: bool,
    #[arg(long, value_name = "METHOD", num_args = 0..=1)]
    auth: Option<Option<String>>,
}

pub(crate) async fn authenticate(agent: &dyn Agent, auth: Option<Option<String>>) -> Result<()> {
    if let Some(method) = auth {
        let method = match method {
            Some(method) => method,
            None => {
                agent
                    .info()
                    .await
                    .auth
                    .methods
                    .into_iter()
                    .next()
                    .ok_or_else(|| format!("{} did not report any sign-in methods", agent.id()))?
                    .id
            }
        };
        show_auth(agent.authenticate(&method).await?);
    }
    Ok(())
}

pub(crate) fn show_auth(result: vetro_proto::rpc::AuthenticateResult) {
    for line in [result.message, result.url, result.code]
        .into_iter()
        .flatten()
    {
        eprintln!("{line}");
    }
}

pub(crate) async fn run(core: &Core, args: Args) -> Result<()> {
    let agent = core.registry().get(&args.agent)?;
    if core
        .home
        .settings()
        .agents
        .get(&args.agent)
        .is_some_and(|agent| !agent.enabled)
    {
        return Err(format!("Agent is disabled: {}", args.agent).into());
    }
    authenticate(agent.as_ref(), args.auth).await?;
    let cwd = cwd(args.cwd)?;
    let text = match args.prompt {
        Some(text) => text,
        None => tokio::task::spawn_blocking(|| input("Prompt: ")).await??,
    };
    let content = vec![ContentBlock::Text { text }];
    let (sink, mut events) = mpsc::unbounded_channel();
    let session = match args.resume {
        Some(id) => {
            let session = agent.attach(&id, Some(&cwd), sink).await?;
            if let Err(error) = session.send(content).await {
                session.detach().await;
                return Err(error.into());
            }
            session
        }
        None => {
            agent
                .start(
                    StartRequest {
                        cwd,
                        content,
                        options: BTreeMap::new(),
                    },
                    sink,
                )
                .await?
        }
    };
    eprintln!("session {}", session.native_id());
    let outcome = tokio::select! {
        result = stream(&session, &mut events, args.yes) => result,
        _ = tokio::signal::ctrl_c() => session.interrupt().await.map_err(Into::into),
    };
    session.detach().await;
    let wire = session.wire_log().snapshot();
    core.home
        .write_raw(&format!("{}:{}", args.agent, session.native_id()), &wire)?;
    if args.raw {
        for entry in wire {
            eprintln!("{}", serde_json::to_string(&entry)?);
        }
    }
    outcome
}

async fn stream(
    session: &Arc<dyn Session>,
    events: &mut mpsc::UnboundedReceiver<SessionEvent>,
    yes: bool,
) -> Result<()> {
    while let Some(event) = events.recv().await {
        match event {
            SessionEvent::Item(item) => {
                println!("{}", serde_json::to_string(&item.kind)?);
                match item.kind {
                    ItemKind::Request(request) if request.resolution.is_none() => {
                        let request_id = request.request_id.clone();
                        let resolution =
                            tokio::task::spawn_blocking(move || respond(request, yes)).await??;
                        session.respond(&request_id, resolution).await?;
                    }
                    ItemKind::TurnEnd {
                        stop_reason: StopReason::Error,
                        ..
                    } => return Err("Agent turn failed".into()),
                    ItemKind::TurnEnd { .. } => return Ok(()),
                    _ => {}
                }
            }
            SessionEvent::TextDelta { item_id, delta } => {
                println!(
                    "{}",
                    serde_json::json!({"type": "text_delta", "itemId": item_id, "delta": delta})
                );
            }
            SessionEvent::Closed { error } => {
                return Err(error
                    .unwrap_or_else(|| "Agent closed before the turn ended".into())
                    .into());
            }
            SessionEvent::AuthRequired => return Err("Agent sign-in is required".into()),
            _ => {}
        }
    }
    Err("Agent event stream closed before the turn ended".into())
}

fn respond(request: InteractionRequest, yes: bool) -> io::Result<RequestResolution> {
    eprintln!("{}", request.title);
    if let Some(detail) = &request.detail {
        eprintln!("{detail}");
    }
    match request.body {
        RequestBody::Permission { options } => {
            if yes {
                if let Some(option) = options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                {
                    return Ok(RequestResolution::Selected {
                        option_id: option.id.clone(),
                    });
                }
            }
            for (index, option) in options.iter().enumerate() {
                eprintln!("  {}. {}", index + 1, option.label);
            }
            loop {
                let answer = input("Choose an option number (blank to cancel): ")?;
                if answer.trim().is_empty() {
                    return Ok(RequestResolution::Cancelled);
                }
                if let Some(option) = option_index(&answer).and_then(|index| options.get(index)) {
                    return Ok(RequestResolution::Selected {
                        option_id: option.id.clone(),
                    });
                }
            }
        }
        RequestBody::Questions { questions } => {
            let mut answers = BTreeMap::new();
            for question in questions {
                eprintln!("{}", question.text);
                for (index, choice) in question.choices.iter().enumerate() {
                    eprintln!("  {}. {}", index + 1, choice.label);
                }
                let answer = loop {
                    let answer = input("Answer (blank to cancel): ")?;
                    if answer.trim().is_empty() {
                        return Ok(RequestResolution::Cancelled);
                    }
                    let selections: Vec<_> = if question.multi_select {
                        answer.split(',').collect()
                    } else {
                        vec![answer.as_str()]
                    };
                    let choices: Option<Vec<_>> = selections
                        .iter()
                        .map(|selection| {
                            option_index(selection)
                                .and_then(|index| question.choices.get(index))
                                .map(|choice| choice.label.clone())
                        })
                        .collect();
                    if let Some(choices) = choices {
                        break choices;
                    }
                    if question.allow_free_text {
                        break vec![answer];
                    }
                };
                answers.insert(question.id, answer);
            }
            Ok(RequestResolution::Answered { answers })
        }
    }
}

fn option_index(answer: &str) -> Option<usize> {
    answer.trim().parse::<usize>().ok()?.checked_sub(1)
}

fn input(prompt: &str) -> io::Result<String> {
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim_end_matches(['\r', '\n']).to_owned())
}

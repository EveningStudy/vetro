use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use vetro_core::{home::Home, registry::Registry, server::Server, Core};
use vetro_proto::rpc::{AuthenticateParams, SearchParams, ThreadsListParams};

mod probe;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        open: bool,
    },
    Agents,
    Probe(probe::Args),
    Threads {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        search: Option<String>,
        #[arg(long)]
        children_of: Option<String>,
    },
    Links,
    Show {
        thread_id: String,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long, value_name = "METHOD", num_args = 0..=1)]
        auth: Option<Option<String>>,
    },
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vetro: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let home = Home::discover()?;
    let registry = Registry::from_settings(&home.settings())?;
    let core = Core::new(home, registry);
    let result = match cli.command {
        Command::Serve { port, open } => {
            let server = Server::bind(core, port).await?;
            let url = server.local_url();
            println!("{url}");
            if open {
                if let Err(error) = open::that_detached(&url) {
                    eprintln!("Could not open browser: {error}");
                }
            }
            server.serve(shutdown()).await?;
            return Ok(());
        }
        Command::Agents => {
            println!("{:<16} {:<12} {:<12} PATH", "AGENT", "STATUS", "VERSION");
            for agent in core.agents(false).await {
                println!(
                    "{:<16} {:<12} {:<12} {}",
                    agent.id,
                    if agent.installed {
                        "installed"
                    } else {
                        "unavailable"
                    },
                    agent.version.as_deref().unwrap_or("-"),
                    agent.path.as_deref().unwrap_or("-")
                );
                if let Some(error) = agent.error {
                    println!("  {error}");
                }
            }
            Ok(())
        }
        Command::Probe(args) => probe::run(&core, args).await,
        Command::Threads {
            agent,
            search,
            children_of,
        } => list_threads(&core, agent, search, children_of).await,
        Command::Links => links(&core).await,
        Command::Show {
            thread_id,
            limit,
            auth,
        } => show_thread(&core, &thread_id, limit, auth).await,
    };
    core.shutdown().await;
    result
}

async fn show_thread(
    core: &std::sync::Arc<Core>,
    id: &str,
    limit: Option<u32>,
    auth: Option<Option<String>>,
) -> Result<()> {
    if auth.is_some() {
        let (agent, _) = id
            .split_once(':')
            .ok_or("Expected a thread id of the form agent:native-id")?;
        let method = match auth.flatten() {
            Some(method) => method,
            None => core
                .agents(false)
                .await
                .into_iter()
                .find(|info| info.id == agent)
                .and_then(|info| info.auth.methods.into_iter().next())
                .map(|method| method.id)
                .ok_or("No authentication methods available")?,
        };
        probe::show_auth(
            core.authenticate(AuthenticateParams {
                agent_id: agent.into(),
                method_id: method,
            })
            .await?,
        );
    }
    core.refresh_history().await;
    let thread = core.open_thread(id, limit).await?;
    println!("{}", serde_json::to_string_pretty(&thread)?);
    Ok(())
}

async fn list_threads(
    core: &std::sync::Arc<Core>,
    agent: Option<String>,
    search: Option<String>,
    children_of: Option<String>,
) -> Result<()> {
    if let Some(agent) = agent.as_deref() {
        core.registry().get(agent)?;
    }
    core.refresh_history().await;
    if let Some(query) = search {
        for hit in core
            .search_threads(
                SearchParams {
                    query,
                    limit: Some(500),
                    include_hidden: None,
                },
                agent.as_deref(),
                children_of.as_deref(),
            )?
            .hits
        {
            println!(
                "{}\t{}\t{}",
                hit.thread.id,
                field(&hit.thread.title),
                field(&hit.snippet)
            );
        }
        return Ok(());
    }
    let mut params = ThreadsListParams {
        agent_ids: agent.map(|id| vec![id]),
        parent_id: children_of,
        include_archived: Some(true),
        ..Default::default()
    };
    loop {
        let page = core.threads(params.clone())?;
        for thread in page.threads {
            println!(
                "{}\t{}\t{}\t{}",
                thread.id,
                field(&thread.title),
                thread.cwd.unwrap_or_default(),
                serde_json::to_string(&thread.source)?.trim_matches('"'),
            );
        }
        let Some(cursor) = page.next_cursor else {
            return Ok(());
        };
        params.cursor = Some(cursor);
    }
}

async fn links(core: &std::sync::Arc<Core>) -> Result<()> {
    core.refresh_history().await;
    for (child, parent) in core.links()? {
        let child_title = core
            .indexed_thread(&child)?
            .map(|thread| thread.title)
            .unwrap_or_default();
        let parent_title = core
            .indexed_thread(&parent.thread_id)?
            .map(|thread| thread.title)
            .unwrap_or_default();
        println!(
            "{}\t{}\t{}\t{}\t{}",
            serde_json::to_string(&parent.confidence)?.trim_matches('"'),
            parent.thread_id,
            field(&parent_title),
            child,
            field(&child_title)
        );
    }
    Ok(())
}

fn field(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

async fn shutdown() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn cwd(path: Option<PathBuf>) -> std::io::Result<PathBuf> {
    match path {
        Some(path) => std::path::absolute(path),
        None => std::env::current_dir(),
    }
}

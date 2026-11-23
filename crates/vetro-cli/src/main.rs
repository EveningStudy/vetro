use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use vetro_core::{home::Home, registry::Registry, server::Server, Core};
use vetro_proto::rpc::ThreadsListParams;

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
        Command::Threads { agent } => list_threads(&core, agent).await,
    };
    core.shutdown().await;
    result
}

async fn list_threads(core: &Core, agent: Option<String>) -> Result<()> {
    if let Some(agent) = agent.as_deref() {
        core.registry.get(agent)?;
    }
    core.refresh_history(false).await;
    let mut params = ThreadsListParams {
        agent_ids: agent.map(|id| vec![id]),
        ..Default::default()
    };
    loop {
        let page = core.threads(params.clone())?;
        for thread in page.threads {
            println!(
                "{}\t{}\t{}",
                thread.id,
                thread.title,
                thread.cwd.unwrap_or_default()
            );
        }
        let Some(cursor) = page.next_cursor else {
            return Ok(());
        };
        params.cursor = Some(cursor);
    }
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

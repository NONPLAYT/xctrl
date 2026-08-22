mod agent;
mod cli;
mod config;
mod controller;
mod detect;
mod quota;
mod remote;
mod serve;
mod state;
mod subgen;
mod xray;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "xctrl",
    version,
    about = "Xray control plane driven by a NixOS flake"
)]
struct Cli {
    #[arg(
        long,
        env = "XCTRL_CONFIG",
        default_value = "/run/secrets/xctrl.json",
        global = true
    )]
    config: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the node agent: project users onto the local xray, report traffic.
    Agent,
    /// Run the controller and subscription server.
    Serve,
    /// List users with quota usage and online state.
    Ls,
    /// Show reachability of every node.
    Status,
    /// Run a poll and push cycle now.
    Sync,
    /// Cut a user off across all nodes.
    Block { user: String },
    /// Restore a blocked user.
    Unblock { user: String },
    /// Print a user's subscription URL and links.
    Export { user: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load(&cli.config)?;
    match cli.cmd {
        Cmd::Agent => agent::run(cfg).await,
        Cmd::Serve => serve::run(cfg).await,
        Cmd::Ls => cli::ls(&cfg).await,
        Cmd::Status => cli::status(&cfg).await,
        Cmd::Sync => cli::sync(&cfg).await,
        Cmd::Block { user } => cli::set_blocked(&cfg, &user, true).await,
        Cmd::Unblock { user } => cli::set_blocked(&cfg, &user, false).await,
        Cmd::Export { user } => cli::export(&cfg, &user).await,
    }
}

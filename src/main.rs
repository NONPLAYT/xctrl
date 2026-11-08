mod agent;
mod cli;
mod config;
mod controller;
mod detect;
mod quota;
mod remote;
mod serve;
mod state;
mod stats;
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
    /// Traffic history: everyone at a glance, or one user hour by hour.
    Stats {
        user: Option<String>,
        /// Days in the per-day table.
        #[arg(long, default_value_t = 7)]
        days: usize,
        /// Hours in a user's per-hour table (at most 14 days are kept hourly).
        #[arg(long, default_value_t = 24)]
        hours: usize,
    },
    /// Who fetches subscriptions, from which addresses and clients.
    Subs { user: Option<String> },
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
        Cmd::Stats { user, days, hours } => cli::stats(&cfg, user.as_deref(), days, hours).await,
        Cmd::Subs { user } => cli::subs(&cfg, user.as_deref()).await,
    }
}

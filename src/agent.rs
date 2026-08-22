//! The node agent: keeps the local xray's user list matching the config, and
//! reports what it measured.
//!
//! The agent decides nothing. Which users exist comes from Nix; who is cut off
//! is pushed by the controller. Its only judgement call is that it caches the
//! cut-off set on disk, so a restart while the controller is unreachable does
//! not silently re-admit everyone.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;

use crate::config::{Config, Inbound};
use crate::state::{self, Usage};
use crate::xray::Xray;

const TICK: Duration = Duration::from_secs(60);
/// Collecting resets xray's counters, so back-to-back polls would shred the
/// resolution of the numbers. Debounce them.
const DEBOUNCE: Duration = Duration::from_secs(3);
/// A user who moved bytes within this window counts as online.
const ONLINE_TTL: u64 = 180;
const CUT_OFF_FILE: &str = "cutoff.json";
const USAGE_FILE: &str = "usage.json";

struct Agent {
    cfg: Config,
    node: String,
    inbounds: Vec<Inbound>,
    xray: Xray,
    cut_off: RwLock<BTreeSet<String>>,
    usage: RwLock<Usage>,
    gate: Mutex<Option<Instant>>,
}

pub async fn run(cfg: Config) -> Result<()> {
    let node = cfg.local()?;
    let agent = Arc::new(Agent {
        node: node.name.clone(),
        inbounds: node.inbounds.clone(),
        xray: Xray::new(&cfg.xray_api)?,
        cut_off: RwLock::new(state::load(&cfg.state_dir, CUT_OFF_FILE)),
        usage: RwLock::new(state::load(&cfg.state_dir, USAGE_FILE)),
        gate: Mutex::new(None),
        cfg,
    });

    let listen = agent.cfg.agent_listen.clone();
    tokio::spawn(tick(agent.clone()));

    let guarded = Router::new()
        .route("/traffic", get(traffic))
        .route("/sync", post(force_sync))
        .route("/cutoff", post(set_cut_off))
        .route_layer(middleware::from_fn_with_state(agent.clone(), auth));
    let router = Router::new()
        .route("/health", get(health))
        .merge(guarded)
        .with_state(agent);

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("agent listening on {listen}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn auth(State(agent): State<Arc<Agent>>, req: Request, next: Next) -> Response {
    let expected = format!("Bearer {}", agent.cfg.token);
    let got = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if got == Some(expected.as_str()) {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Liveness of the thing we actually care about — xray, not this process.
async fn health(State(agent): State<Arc<Agent>>) -> StatusCode {
    if agent.xray.alive().await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn traffic(State(agent): State<Arc<Agent>>) -> Json<Usage> {
    collect_debounced(&agent).await;
    Json(agent.usage.read().await.clone())
}

async fn force_sync(State(agent): State<Arc<Agent>>) -> Response {
    report(sync(&agent).await)
}

/// Replaces the cut-off set wholesale. Declarative on purpose: the controller
/// sends the full truth every time, so a lost message cannot leave the node
/// holding a stale opinion about one user.
async fn set_cut_off(
    State(agent): State<Arc<Agent>>,
    Json(users): Json<BTreeSet<String>>,
) -> Response {
    {
        let mut current = agent.cut_off.write().await;
        if *current == users {
            return StatusCode::OK.into_response();
        }
        *current = users;
        if let Err(e) = state::save(&agent.cfg.state_dir, CUT_OFF_FILE, &*current) {
            eprintln!("save cutoff: {e:#}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    report(sync(&agent).await)
}

fn report(result: Result<()>) -> Response {
    match result {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => {
            eprintln!("sync: {e:#}");
            (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn tick(agent: Arc<Agent>) {
    let mut ticker = tokio::time::interval(TICK);
    loop {
        ticker.tick().await;
        if let Err(e) = sync(&agent).await {
            eprintln!("sync: {e:#}");
        }
        collect_debounced(&agent).await;
    }
}

async fn collect_debounced(agent: &Agent) {
    let mut last = agent.gate.lock().await;
    if last.is_none_or(|t| t.elapsed() >= DEBOUNCE) {
        match collect(agent).await {
            Ok(()) => *last = Some(Instant::now()),
            Err(e) => eprintln!("collect: {e:#}"),
        }
    }
}

/// Drains xray's counters into the on-disk running total.
///
/// The read resets the counters, so anything not persisted here is lost for
/// good — never drop the result of this call.
async fn collect(agent: &Agent) -> Result<()> {
    let samples = agent.xray.traffic(true).await?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut usage = agent.usage.write().await;
    for (email, direction, value) in samples {
        if value == 0 {
            continue;
        }
        usage.seen.insert(email.clone(), now);
        let entry = usage.users.entry(email).or_default();
        match direction.as_str() {
            "uplink" => entry.up += value,
            "downlink" => entry.down += value,
            _ => {}
        }
    }
    usage.online = usage
        .seen
        .iter()
        .filter(|(_, seen)| now.saturating_sub(**seen) <= ONLINE_TTL)
        .map(|(email, _)| email.clone())
        .collect();
    usage.collected_at = now;
    state::save(&agent.cfg.state_dir, USAGE_FILE, &*usage)
}

/// Brings every inbound on this node to the user list it should have.
///
/// Each inbound serves exactly one group, so a user of another group appearing
/// in it is drift and gets removed.
async fn sync(agent: &Agent) -> Result<()> {
    let cut_off = agent.cut_off.read().await.clone();
    for inbound in &agent.inbounds {
        let desired: Vec<_> = agent
            .cfg
            .users_of(&inbound.group)
            .filter(|u| !cut_off.contains(&u.name))
            .collect();
        let actual = agent.xray.users(&inbound.tag).await?;

        let flow = inbound.link.params.get("flow").cloned().unwrap_or_default();
        for user in desired.iter().filter(|u| !actual.contains(&u.name)) {
            agent
                .xray
                .add_user(&inbound.tag, &user.name, &user.uuid, &flow)
                .await?;
            eprintln!("{}: + {} ({})", agent.node, user.name, inbound.tag);
        }
        for email in actual
            .iter()
            .filter(|e| !desired.iter().any(|u| &u.name == *e))
        {
            agent.xray.remove_user(&inbound.tag, email).await?;
            eprintln!("{}: - {} ({})", agent.node, email, inbound.tag);
        }
    }
    Ok(())
}

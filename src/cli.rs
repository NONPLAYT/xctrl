use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::Deserialize;

use crate::config::Config;
use crate::remote;
use crate::state::{Ledger, Usage};
use crate::subgen;

/// Formats a byte count the way a person reads it.
pub fn human(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("T", 1 << 40),
        ("G", 1 << 30),
        ("M", 1 << 20),
        ("K", 1 << 10),
    ];
    match UNITS.into_iter().find(|(_, size)| bytes >= *size) {
        Some((unit, size)) => format!("{:.1}{unit}", bytes as f64 / size as f64),
        None => bytes.to_string(),
    }
}

#[derive(Deserialize)]
struct Snapshot {
    ledger: Ledger,
    nodes: BTreeMap<String, Usage>,
    limits: BTreeMap<String, Option<u64>>,
    next_reset: DateTime<Utc>,
}

#[derive(Deserialize)]
struct NodeStatus {
    node: String,
    up: bool,
}

async fn call(cfg: &Config, method: Method, path: &str, body: Option<String>) -> Result<String> {
    let url = format!("{}{path}", cfg.controller_api.trim_end_matches('/'));
    let client = remote::client()?;
    let mut req = client.request(method, &url).bearer_auth(&cfg.token);
    if let Some(b) = body {
        req = req.body(b);
    }
    let response = req
        .send()
        .await
        .with_context(|| format!("reaching the controller at {url}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    ensure!(status.is_success(), "controller returned {status}: {text}");
    Ok(text)
}

async fn snapshot(cfg: &Config) -> Result<Snapshot> {
    let body = call(cfg, Method::GET, "/ledger", None).await?;
    serde_json::from_str(&body).context("parsing controller ledger")
}

fn table(rows: &[Vec<String>]) {
    let Some(first) = rows.first() else { return };
    let widths: Vec<usize> = (0..first.len())
        .map(|i| {
            rows.iter()
                .map(|r| r.get(i).map(|c| c.chars().count()).unwrap_or(0))
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in rows {
        let line = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect::<Vec<_>>()
            .join("  ");
        println!("{}", line.trim_end());
    }
}

pub async fn ls(cfg: &Config) -> Result<()> {
    let snap = snapshot(cfg).await?;
    let mut rows = vec![
        ["on", "st", "user", "group", "used", "limit", "expires"]
            .map(String::from)
            .to_vec(),
    ];
    for user in &cfg.users {
        let online = snap
            .nodes
            .values()
            .any(|usage| usage.online.iter().any(|o| o == &user.name));
        let state = if snap.ledger.blocked.contains(&user.name) {
            "block"
        } else if snap.ledger.limited.contains(&user.name) {
            "limit"
        } else if snap.ledger.expired.contains(&user.name) {
            "exp"
        } else {
            "-"
        };
        let used = snap.ledger.spent_by(&user.name).total();
        let limit = snap
            .limits
            .get(&user.name)
            .copied()
            .flatten()
            .map(human)
            .unwrap_or_else(|| "∞".into());
        rows.push(vec![
            if online { "*" } else { "-" }.into(),
            state.into(),
            user.name.clone(),
            user.group.clone(),
            human(used),
            limit,
            user.expires
                .map(|d| d.to_string())
                .unwrap_or_else(|| "never".into()),
        ]);
    }
    table(&rows);
    println!("\nnext reset: {}", cfg.quota.render_local(snap.next_reset));
    Ok(())
}

pub async fn status(cfg: &Config) -> Result<()> {
    let body = call(cfg, Method::GET, "/status", None).await?;
    let nodes: Vec<NodeStatus> = serde_json::from_str(&body).context("parsing node status")?;
    for node in nodes {
        println!("{:<12} {}", node.node, if node.up { "up" } else { "down" });
    }
    Ok(())
}

pub async fn sync(cfg: &Config) -> Result<()> {
    call(cfg, Method::POST, "/sync", None).await?;
    println!("ok");
    Ok(())
}

pub async fn set_blocked(cfg: &Config, user: &str, blocked: bool) -> Result<()> {
    let path = if blocked { "/block" } else { "/unblock" };
    call(cfg, Method::POST, path, Some(user.to_string())).await?;
    println!("{user}: {}", if blocked { "blocked" } else { "unblocked" });
    Ok(())
}

/// Prints a user's subscription URL and every link in it.
pub async fn export(cfg: &Config, user: &str) -> Result<()> {
    let u = cfg
        .user(user)
        .with_context(|| format!("no such user: {user}"))?;

    // Best effort: a live controller tells us whether they are cut off.
    let cut_off: BTreeSet<String> = match snapshot(cfg).await {
        Ok(snap) => snap.ledger.cut_off(),
        Err(e) => {
            eprintln!("warning: controller unreachable, cut-off state unknown ({e:#})");
            BTreeSet::new()
        }
    };

    let client = remote::client()?;
    let mut addrs = BTreeMap::new();
    for node in &cfg.nodes {
        let addr = match &node.addr {
            Some(addr) => addr.clone(),
            None => remote::doh_resolve(&client, &node.fqdn)
                .await
                .unwrap_or_else(|e| {
                    eprintln!(
                        "warning: resolve {} failed ({e:#}), using the name",
                        node.fqdn
                    );
                    node.fqdn.clone()
                }),
        };
        addrs.insert(node.name.clone(), addr);
    }

    let blocked = cut_off.contains(&u.name);
    let endpoints = if blocked {
        vec![subgen::blocked_endpoint(&cfg.support_url)]
    } else {
        subgen::build_endpoints(cfg, u, |node| addrs.get(node).cloned())
    };

    let mark = if blocked { "  [CUT OFF]" } else { "" };
    println!("https://{}/{}{mark}\n", cfg.sub_domain, u.sub_token);
    for ep in &endpoints {
        println!("{}", subgen::uri(ep));
    }
    Ok(())
}

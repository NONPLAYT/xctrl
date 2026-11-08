use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Duration, Utc};
use reqwest::Method;
use serde::Deserialize;

use crate::config::Config;
use crate::remote;
use crate::state::{Ledger, Traffic, Usage};
use crate::stats::{self, Fetches, History, UserFetches};
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

async fn fetch_json<T: serde::de::DeserializeOwned>(cfg: &Config, path: &str) -> Result<T> {
    let body = call(cfg, Method::GET, path, None).await?;
    serde_json::from_str(&body).with_context(|| format!("parsing controller {path}"))
}

/// A byte count for a table cell, with nothing shown as a dash so the eye
/// lands on the rows that matter.
fn cell(bytes: u64) -> String {
    if bytes == 0 { "-".into() } else { human(bytes) }
}

pub async fn stats(cfg: &Config, user: Option<&str>, days: usize, hours: usize) -> Result<()> {
    let snap = snapshot(cfg).await?;
    let history: History = fetch_json(cfg, "/history").await?;
    match user {
        None => fleet_stats(cfg, &snap, &history, days),
        Some(name) => {
            let u = cfg
                .user(name)
                .with_context(|| format!("no such user: {name}"))?;
            let fetches: Fetches = fetch_json(cfg, "/fetches").await?;
            user_stats(cfg, &snap, &history, &fetches, &u.name, days, hours);
            Ok(())
        }
    }
}

fn fleet_stats(cfg: &Config, snap: &Snapshot, history: &History, days: usize) -> Result<()> {
    let now = Utc::now();
    let last_1h = history.since(now);
    let last_24h = history.since(now - Duration::hours(23));
    let last_7d = history.since(now - Duration::days(7) + Duration::hours(1));

    // The busiest hour of the week per user: a burst says something different
    // from a steady drip of the same size.
    let mut peak: BTreeMap<&str, (u64, i64)> = BTreeMap::new();
    let week_from = (now - Duration::days(7)).timestamp();
    for (key, bucket) in history.hourly.range(week_from..) {
        for (user, t) in &bucket.users {
            let p = peak.entry(user.as_str()).or_default();
            if t.total() > p.0 {
                *p = (t.total(), *key);
            }
        }
    }

    let mut users: Vec<_> = cfg.users.iter().collect();
    users.sort_by_key(|u| std::cmp::Reverse(last_7d.user(&u.name).total()));

    let mut rows = vec![
        [
            "user",
            "group",
            "1h",
            "24h",
            "7d",
            "period",
            "limit",
            "peak hour (7d)",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for u in users {
        let limit = snap
            .limits
            .get(&u.name)
            .copied()
            .flatten()
            .map(human)
            .unwrap_or_else(|| "∞".into());
        let peak = peak
            .get(u.name.as_str())
            .map(|(bytes, key)| {
                format!(
                    "{} @ {}",
                    human(*bytes),
                    cfg.quota.render_short(stats::hour_start(*key))
                )
            })
            .unwrap_or_else(|| "-".into());
        rows.push(vec![
            u.name.clone(),
            u.group.clone(),
            cell(last_1h.user(&u.name).total()),
            cell(last_24h.user(&u.name).total()),
            cell(last_7d.user(&u.name).total()),
            cell(snap.ledger.spent_by(&u.name).total()),
            limit,
            peak,
        ]);
    }
    rows.push(vec![
        "total".into(),
        String::new(),
        cell(last_1h.total().total()),
        cell(last_24h.total().total()),
        cell(last_7d.total().total()),
        cell(snap.ledger.spent.values().map(Traffic::total).sum::<u64>()),
    ]);
    table(&rows);

    let by_day = history.days(&cfg.quota);
    if by_day.is_empty() {
        println!("\nno history yet");
        return Ok(());
    }
    println!();
    let mut header = vec!["day".to_string(), "total".into()];
    header.extend(cfg.nodes.iter().map(|n| n.name.clone()));
    header.push("top users".into());
    let mut rows = vec![header];
    for (day, bucket) in by_day.iter().rev().take(days) {
        let mut row = vec![
            day.format("%d.%m.%Y").to_string(),
            cell(bucket.total().total()),
        ];
        row.extend(
            cfg.nodes
                .iter()
                .map(|n| cell(bucket.nodes.get(&n.name).map(Traffic::total).unwrap_or(0))),
        );
        let mut top: Vec<_> = bucket.users.iter().collect();
        top.sort_by_key(|(_, t)| std::cmp::Reverse(t.total()));
        row.push(
            top.iter()
                .take(3)
                .map(|(u, t)| format!("{u} {}", human(t.total())))
                .collect::<Vec<_>>()
                .join(", "),
        );
        rows.push(row);
    }
    table(&rows);
    println!("\ntimes in UTC{:+}", cfg.quota.reset_utc_offset_hours);
    Ok(())
}

fn user_stats(
    cfg: &Config,
    snap: &Snapshot,
    history: &History,
    fetches: &Fetches,
    user: &str,
    days: usize,
    hours: usize,
) {
    let spent = snap.ledger.spent_by(user);
    let limit = snap
        .limits
        .get(user)
        .copied()
        .flatten()
        .map(human)
        .unwrap_or_else(|| "∞".into());
    println!(
        "{user}: {} of {limit} this period (up {}, down {}), next reset {}\n",
        human(spent.total()),
        human(spent.up),
        human(spent.down),
        cfg.quota.render_local(snap.next_reset)
    );

    let traffic_row =
        |label: String, t: Traffic| vec![label, cell(t.up), cell(t.down), cell(t.total())];
    let header = |first: &str| [first, "up", "down", "total"].map(String::from).to_vec();

    let mut rows = vec![header("day")];
    for (day, bucket) in history.days(&cfg.quota).iter().rev().take(days) {
        rows.push(traffic_row(
            day.format("%d.%m.%Y").to_string(),
            bucket.user(user),
        ));
    }
    table(&rows);

    // Only hours that moved anything: a quiet night is not worth a screen.
    let from = (Utc::now() - Duration::hours(hours.saturating_sub(1) as i64)).timestamp();
    let from = from.div_euclid(3600) * 3600;
    let mut rows = vec![header("hour")];
    for (key, bucket) in history.hourly.range(from..).rev() {
        let t = bucket.user(user);
        if t.total() > 0 {
            rows.push(traffic_row(
                cfg.quota.render_short(stats::hour_start(*key)),
                t,
            ));
        }
    }
    println!();
    if rows.len() > 1 {
        table(&rows);
    } else {
        println!("no traffic in the last {hours}h");
    }

    println!();
    match fetches.users.get(user) {
        Some(f) => client_table(cfg, f),
        None => println!("subscription never fetched"),
    }
}

fn client_table(cfg: &Config, f: &UserFetches) {
    println!(
        "subscription: {} fetches from {} addresses",
        f.total,
        f.distinct_ips()
    );
    let mut rows = vec![
        ["last", "first", "count", "ip", "format", "client"]
            .map(String::from)
            .to_vec(),
    ];
    for c in &f.clients {
        rows.push(vec![
            cfg.quota.render_short(c.last),
            cfg.quota.render_short(c.first),
            c.count.to_string(),
            c.ip.clone(),
            c.format.clone(),
            c.ua.clone(),
        ]);
    }
    table(&rows);
}

pub async fn subs(cfg: &Config, user: Option<&str>) -> Result<()> {
    let fetches: Fetches = fetch_json(cfg, "/fetches").await?;
    if let Some(name) = user {
        let u = cfg
            .user(name)
            .with_context(|| format!("no such user: {name}"))?;
        match fetches.users.get(&u.name) {
            Some(f) => client_table(cfg, f),
            None => println!("{name}: subscription never fetched"),
        }
        return Ok(());
    }

    let mut rows = vec![
        [
            "user",
            "fetches",
            "ips",
            "clients",
            "last fetch",
            "last ip",
            "last client",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for u in &cfg.users {
        let Some(f) = fetches.users.get(&u.name) else {
            rows.push(vec![
                u.name.clone(),
                "0".into(),
                "-".into(),
                "-".into(),
                "never".into(),
            ]);
            continue;
        };
        let last = f.last();
        rows.push(vec![
            u.name.clone(),
            f.total.to_string(),
            f.distinct_ips().to_string(),
            f.clients.len().to_string(),
            last.map(|c| cfg.quota.render_short(c.last))
                .unwrap_or_else(|| "-".into()),
            last.map(|c| c.ip.clone()).unwrap_or_default(),
            last.map(|c| c.ua.clone()).unwrap_or_default(),
        ]);
    }
    table(&rows);
    println!("\ntimes in UTC{:+}", cfg.quota.reset_utc_offset_hours);
    Ok(())
}

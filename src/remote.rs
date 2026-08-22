use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, Method, Response};

use crate::config::Node;
use crate::state::Usage;

const DOH: &[&str] = &["https://1.1.1.1/dns-query", "https://8.8.8.8/resolve"];

pub fn client() -> Result<Client> {
    Ok(Client::builder().timeout(Duration::from_secs(10)).build()?)
}

/// Candidate agent URLs for a node.
fn urls(node: &Node, path: &str) -> Vec<String> {
    match &node.api {
        Some(base) => vec![format!("{}{path}", base.trim_end_matches('/'))],
        None => vec![
            format!("https://{}:443{path}", node.fqdn),
            format!("https://{}:8443{path}", node.fqdn),
        ],
    }
}

pub async fn request(
    client: &Client,
    token: &str,
    node: &Node,
    method: Method,
    path: &str,
    body: Option<String>,
) -> Result<Response> {
    let mut last = None;
    for url in urls(node, path) {
        let mut req = client.request(method.clone(), &url).bearer_auth(token);
        if let Some(b) = &body {
            req = req
                .body(b.clone())
                .header("content-type", "application/json");
        }
        match req.send().await {
            Ok(r) => return Ok(r),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("no reachable address for {}", node.name)))
}

/// POSTs a JSON body and fails on any non-success status.
pub async fn post_json<T: serde::Serialize>(
    client: &Client,
    token: &str,
    node: &Node,
    path: &str,
    body: &T,
) -> Result<()> {
    let payload = serde_json::to_string(body)?;
    let r = request(client, token, node, Method::POST, path, Some(payload)).await?;
    let status = r.status();
    if status.is_success() {
        return Ok(());
    }
    let text = r.text().await.unwrap_or_default();
    anyhow::bail!("{} returned {status}: {text}", node.name)
}

pub async fn fetch_usage(client: &Client, token: &str, node: &Node) -> Result<Usage> {
    let r = request(client, token, node, Method::GET, "/traffic", None).await?;
    Ok(r.error_for_status()?.json().await?)
}

pub async fn is_up(client: &Client, token: &str, node: &Node) -> bool {
    matches!(
        request(client, token, node, Method::GET, "/health", None).await,
        Ok(r) if r.status().is_success()
    )
}

/// Resolves an A record over DoH, trying each resolver before giving up.
pub async fn doh_resolve(client: &Client, name: &str) -> Result<String> {
    let mut last = None;
    for base in DOH {
        let attempt = async {
            let r = client
                .get(format!("{base}?name={name}&type=A"))
                .header("accept", "application/dns-json")
                .send()
                .await?
                .error_for_status()?;
            let v: serde_json::Value = r.json().await?;
            v["Answer"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| a["type"].as_i64() == Some(1))
                .filter_map(|a| a["data"].as_str())
                .next()
                .map(str::to_string)
                .context("no A records")
        }
        .await;
        match attempt {
            Ok(ip) => return Ok(ip),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no resolvers configured")))
}

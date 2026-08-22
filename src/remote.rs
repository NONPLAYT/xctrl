use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, Method, Response};

use crate::config::{Config, Node};
use crate::state::Usage;

const DOH: &[&str] = &["https://1.1.1.1/dns-query", "https://8.8.8.8/resolve"];

pub fn client() -> Result<Client> {
    Ok(Client::builder().timeout(Duration::from_secs(10)).build()?)
}

/// Candidate agent URLs for a node.
///
/// A node with an explicit `api` is the one running on this very host and is
/// reached directly. The rest go over TLS under `api_path`, because a node's
/// vhost root belongs to its cover site: port 443 hits nginx, and 8443 is the
/// reality inbound, which forwards handshakes it does not recognise to that
/// same nginx — so the API survives 443 being blocked.
fn urls(node: &Node, api_path: &str, path: &str) -> Vec<String> {
    match &node.api {
        Some(base) => vec![format!("{}{path}", base.trim_end_matches('/'))],
        None => {
            let prefix = api_path.trim_end_matches('/');
            vec![
                format!("https://{}:443{prefix}{path}", node.fqdn),
                format!("https://{}:8443{prefix}{path}", node.fqdn),
            ]
        }
    }
}

pub async fn request(
    client: &Client,
    cfg: &Config,
    node: &Node,
    method: Method,
    path: &str,
    body: Option<String>,
) -> Result<Response> {
    let mut last = None;
    for url in urls(node, &cfg.api_path, path) {
        let mut req = client.request(method.clone(), &url).bearer_auth(&cfg.token);
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
    cfg: &Config,
    node: &Node,
    path: &str,
    body: &T,
) -> Result<()> {
    let payload = serde_json::to_string(body)?;
    let r = request(client, cfg, node, Method::POST, path, Some(payload)).await?;
    let status = r.status();
    if status.is_success() {
        return Ok(());
    }
    let text = r.text().await.unwrap_or_default();
    anyhow::bail!("{} returned {status}: {text}", node.name)
}

pub async fn fetch_usage(client: &Client, cfg: &Config, node: &Node) -> Result<Usage> {
    let r = request(client, cfg, node, Method::GET, "/traffic", None).await?;
    Ok(r.error_for_status()?.json().await?)
}

pub async fn is_up(client: &Client, cfg: &Config, node: &Node) -> bool {
    matches!(
        request(client, cfg, node, Method::GET, "/health", None).await,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(api: Option<&str>) -> Node {
        serde_json::from_value(serde_json::json!({
            "name": "asgard", "flag": "", "fqdn": "asgard.example.test",
            "api": api, "inbounds": []
        }))
        .unwrap()
    }

    #[test]
    fn a_remote_node_is_reached_under_the_api_path() {
        assert_eq!(
            urls(&node(None), "/xctrl", "/traffic"),
            [
                "https://asgard.example.test:443/xctrl/traffic",
                "https://asgard.example.test:8443/xctrl/traffic"
            ]
        );
    }

    #[test]
    fn the_local_node_ignores_the_api_path() {
        assert_eq!(
            urls(&node(Some("http://127.0.0.1:10086")), "/xctrl", "/traffic"),
            ["http://127.0.0.1:10086/traffic"]
        );
    }
}

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct Config {
    pub support_url: String,
    pub sub_domain: String,
    /// Profile name clients display, with `{user}` standing in for the
    /// username. Unset falls back to `{sub_domain} — {user}`.
    #[serde(default)]
    pub title: Option<String>,
    /// Shared bearer token guarding every agent endpoint.
    pub token: String,
    /// Name of the node this host runs, when it runs one. Unset on pure clients.
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default = "default_serve_listen")]
    pub serve_listen: String,
    #[serde(default = "default_agent_listen")]
    pub agent_listen: String,
    /// Controller admin API, bound on the controller host only.
    #[serde(default = "default_admin_listen")]
    pub admin_listen: String,
    /// Where management subcommands reach the controller.
    #[serde(default = "default_controller_api")]
    pub controller_api: String,
    #[serde(default = "default_xray_api")]
    pub xray_api: String,
    /// Path the agent API is mounted under on a node's public vhost. Nodes hide
    /// the API behind whatever site they already serve, so it cannot sit at the
    /// root; the local node is reached through `Node::api` and ignores this.
    #[serde(default = "default_api_path")]
    pub api_path: String,
    #[serde(default)]
    pub quota: Quota,
    #[serde(default)]
    pub happ_routing: Option<String>,
    #[serde(default)]
    pub clash: Option<ClashProfile>,
    pub groups: Vec<Group>,
    pub nodes: Vec<Node>,
    pub users: Vec<User>,
}

#[derive(Clone, Deserialize)]
pub struct ClashProfile {
    /// Selector group the generator emits. The profile's rules point at it.
    pub selector: String,
    /// url-test group offered first inside the selector. Omit to skip it.
    #[serde(default)]
    pub auto: Option<String>,
    /// Extra selectors, each holding only the endpoints of one scheme. Lets a
    /// profile send part of its traffic over a different protocol -- a UDP one
    /// for voice, say -- while the main selector still spans everything.
    #[serde(default)]
    pub groups: Vec<ClashGroup>,
    pub profile: String,
}

/// One scheme-filtered selector. Empty of members it is left out entirely: a
/// mihomo group with no proxies fails to load.
#[derive(Clone, Deserialize)]
pub struct ClashGroup {
    pub name: String,
    pub scheme: String,
}

/// A tenant: a set of users served by a set of node inbounds.
///
/// Two groups on one node means two independent VPNs sharing a machine — each
/// with its own inbound, reality keys and traffic accounting.
#[derive(Clone, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub limit: Option<Limit>,
}

#[derive(Clone, Deserialize)]
pub struct Node {
    pub name: String,
    pub flag: String,
    pub fqdn: String,
    /// Pinned IP. When absent it is resolved over DoH so clients do not depend on DNS.
    #[serde(default)]
    pub addr: Option<String>,
    /// Direct agent URL, set only for the node running on this very host.
    #[serde(default)]
    pub api: Option<String>,
    pub inbounds: Vec<Inbound>,
}

/// One xray inbound, dedicated to exactly one group.
#[derive(Clone, Deserialize)]
pub struct Inbound {
    pub group: String,
    /// xray inbound tag, the handle used over the gRPC handler API.
    pub tag: String,
    /// Appended to the node label so two inbounds of one node serving one group
    /// stay distinguishable in a client. Absent leaves the label untouched,
    /// which is what a single-inbound node wants -- changing it would reshuffle
    /// every subscriber's stored selection.
    #[serde(default)]
    pub suffix: Option<String>,
    pub link: Link,
}

/// Everything needed to render a connection URI, kept protocol agnostic so a new
/// protocol is a config change rather than a code change.
#[derive(Clone, Deserialize)]
pub struct Link {
    pub scheme: String,
    pub port: u16,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Clone, Deserialize)]
pub struct User {
    pub name: String,
    pub group: String,
    pub uuid: String,
    /// Subscription path segment; rotatable without touching the connection key.
    pub sub_token: String,
    /// Absent inherits the group default; `"unlimited"` opts out of it.
    #[serde(default)]
    pub limit: Option<Limit>,
    /// Last day the account works, read in the quota offset. Absent never expires.
    #[serde(default)]
    pub expires: Option<NaiveDate>,
    #[serde(default)]
    pub admin: bool,
}

/// Monthly quota reset, pinned to a fixed UTC offset rather than host local time
/// so nodes in different timezones agree on the boundary.
#[derive(Clone, Deserialize)]
pub struct Quota {
    /// Day of month 1..=31, clamped to the last day of shorter months.
    #[serde(default = "default_reset_day")]
    pub reset_day: u32,
    #[serde(default = "default_reset_hour")]
    pub reset_hour: u32,
    #[serde(default)]
    pub reset_minute: u32,
    #[serde(default = "default_reset_offset")]
    pub reset_utc_offset_hours: i32,
}

#[derive(Clone, Copy)]
pub enum Limit {
    Unlimited,
    Bytes(u64),
}

impl Config {
    pub fn user(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|u| u.name == name)
    }

    pub fn user_by_token(&self, token: &str) -> Option<&User> {
        self.users.iter().find(|u| u.sub_token == token)
    }

    pub fn group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }

    /// The node this host serves, for agent mode.
    pub fn local(&self) -> Result<&Node> {
        let node = self.node.as_deref().context("config has no node")?;
        self.nodes
            .iter()
            .find(|n| n.name == node)
            .with_context(|| format!("no node entry for {node}"))
    }

    /// Effective byte quota for a user: own limit, else the group default, else unlimited.
    pub fn limit_of(&self, user: &User) -> Option<u64> {
        match user.limit {
            Some(Limit::Unlimited) => None,
            Some(Limit::Bytes(n)) => Some(n),
            None => match self.group(&user.group).and_then(|g| g.limit) {
                Some(Limit::Bytes(n)) => Some(n),
                _ => None,
            },
        }
    }

    /// Users belonging to `group`.
    pub fn users_of(&self, group: &str) -> impl Iterator<Item = &User> {
        self.users.iter().filter(move |u| u.group == group)
    }

    /// Inbounds across all nodes that serve `group`, paired with their node.
    pub fn inbounds_for(&self, group: &str) -> impl Iterator<Item = (&Node, &Inbound)> {
        self.nodes.iter().flat_map(move |n| {
            n.inbounds
                .iter()
                .filter(move |i| i.group == group)
                .map(move |i| (n, i))
        })
    }
}

pub fn load(path: &str) -> Result<Config> {
    let data = std::fs::read(path).with_context(|| format!("read config {path}"))?;
    serde_json::from_slice(&data).with_context(|| format!("parse config {path}"))
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            reset_day: default_reset_day(),
            reset_hour: default_reset_hour(),
            reset_minute: 0,
            reset_utc_offset_hours: default_reset_offset(),
        }
    }
}

fn default_state_dir() -> PathBuf {
    "/var/lib/xctrl".into()
}

fn default_serve_listen() -> String {
    "127.0.0.1:3001".into()
}

fn default_agent_listen() -> String {
    "127.0.0.1:10086".into()
}

fn default_admin_listen() -> String {
    "127.0.0.1:10087".into()
}

fn default_controller_api() -> String {
    "http://127.0.0.1:10087".into()
}

fn default_xray_api() -> String {
    "127.0.0.1:10085".into()
}

fn default_api_path() -> String {
    "/xctrl".into()
}

fn default_reset_day() -> u32 {
    1
}

fn default_reset_hour() -> u32 {
    1
}

fn default_reset_offset() -> i32 {
    3
}

impl<'de> Deserialize<'de> for Limit {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Num(u64),
            Text(String),
        }
        match Repr::deserialize(d)? {
            Repr::Num(n) => Ok(Limit::Bytes(n)),
            Repr::Text(s) if is_unlimited(&s) => Ok(Limit::Unlimited),
            Repr::Text(s) => parse_size(&s)
                .map(Limit::Bytes)
                .map_err(serde::de::Error::custom),
        }
    }
}

fn is_unlimited(s: &str) -> bool {
    let s = s.trim();
    s == "\u{221e}" || s.eq_ignore_ascii_case("unlimited") || s.eq_ignore_ascii_case("none")
}

pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some(c) => match c.to_ascii_uppercase() {
            'K' => (&s[..s.len() - 1], 10),
            'M' => (&s[..s.len() - 1], 20),
            'G' => (&s[..s.len() - 1], 30),
            'T' => (&s[..s.len() - 1], 40),
            'B' | '0'..='9' => (s.trim_end_matches(['B', 'b']), 0),
            _ => return Err(format!("bad size unit in {s:?}")),
        },
        None => return Err("empty size".into()),
    };
    let value: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("bad size number in {s:?}"))?;
    if value < 0.0 {
        return Err(format!("negative size {s:?}"));
    }
    Ok((value * (1u64 << shift) as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::{Limit, parse_size};

    #[test]
    fn parses_binary_units() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("200G").unwrap(), 200 * (1 << 30));
        assert_eq!(parse_size("1.5T").unwrap(), 1649267441664);
        assert_eq!(parse_size("512M").unwrap(), 512 * (1 << 20));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_size("").is_err());
        assert!(parse_size("12X").is_err());
        assert!(parse_size("abc").is_err());
    }

    #[test]
    fn a_user_limit_can_opt_out_of_the_group_default() {
        let parse = |s: &str| serde_json::from_str::<Limit>(s).unwrap();
        assert!(matches!(parse(r#""unlimited""#), Limit::Unlimited));
        assert!(matches!(parse("\"\u{221e}\""), Limit::Unlimited));
        assert!(matches!(parse(r#""150G""#), Limit::Bytes(n) if n == 150 * (1 << 30)));
        assert!(serde_json::from_str::<Limit>(r#""12X""#).is_err());
    }
}

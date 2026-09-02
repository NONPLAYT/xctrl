//! Rendering a user's endpoints into whatever their client speaks.
//!
//! An [`Endpoint`] is one connectable proxy: a user's key resolved against one
//! node inbound. Everything protocol-specific arrives as `params` straight from
//! the Nix config, so adding a protocol is a config change, not a match arm.

mod base64;
mod clash;
mod happ;
mod links;
mod singbox;
mod xray;

use std::collections::BTreeMap;

pub use links::uri;

use crate::config::{Config, User};
use crate::detect::Format;

#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Node name, used as the proxy label together with the flag.
    pub node: String,
    pub flag: String,
    /// Tail of the label, set when one node offers this group more than one
    /// inbound and the two would otherwise collide.
    pub suffix: Option<String>,
    pub scheme: String,
    /// Resolved IP when available, otherwise the FQDN.
    pub address: String,
    pub port: u16,
    pub uuid: String,
    pub params: BTreeMap<String, String>,
}

impl Endpoint {
    /// Display label, e.g. `🇸🇪 stockholm`. Doubles as the proxy name in every
    /// dialect, so two endpoints must never produce the same one.
    pub fn label(&self) -> String {
        let mut label = if self.flag.is_empty() {
            self.node.clone()
        } else {
            format!("{} {}", self.flag, self.node)
        };
        if let Some(suffix) = &self.suffix {
            label.push(' ');
            label.push_str(suffix);
        }
        label
    }

    pub fn param(&self, key: &str) -> &str {
        self.params.get(key).map(String::as_str).unwrap_or_default()
    }

    /// uTLS fingerprint, defaulting to chrome like every sane client does.
    pub fn fingerprint(&self) -> &str {
        match self.param("fp") {
            "" => "chrome",
            fp => fp,
        }
    }

    pub fn is_reality(&self) -> bool {
        self.param("security") == "reality"
    }

    pub fn is_tls(&self) -> bool {
        matches!(self.param("security"), "tls" | "reality")
    }

    /// Transport network; xray defaults to tcp when the URI omits `type`.
    pub fn network(&self) -> &str {
        match self.param("type") {
            "" => "tcp",
            net => net,
        }
    }

    /// TLS server name, falling back to the dial address.
    pub fn sni(&self) -> &str {
        match self.param("sni") {
            "" => &self.address,
            sni => sni,
        }
    }
}

/// Resolves a user against the config, emitting one endpoint per inbound that
/// serves the user's group. `address_of` supplies the dial address per node so
/// the caller can hand over a DoH-resolved IP instead of the FQDN.
pub fn build_endpoints(
    cfg: &Config,
    user: &User,
    address_of: impl Fn(&str) -> Option<String>,
) -> Vec<Endpoint> {
    cfg.inbounds_for(&user.group)
        .filter_map(|(node, inbound)| {
            let address = address_of(&node.name)?;
            Some(Endpoint {
                node: node.name.clone(),
                flag: node.flag.clone(),
                suffix: inbound.suffix.clone(),
                scheme: inbound.link.scheme.clone(),
                address,
                port: inbound.link.port,
                uuid: user.uuid.clone(),
                params: inbound.link.params.clone(),
            })
        })
        .collect()
}

/// A rendered subscription body plus the content type to serve it with.
pub struct Rendered {
    pub body: Vec<u8>,
    pub content_type: &'static str,
}

/// Renders endpoints in the requested format.
pub fn render(cfg: &Config, format: Format, user: &User, eps: &[Endpoint]) -> Rendered {
    let clash_profile = cfg.clash.as_ref();
    match format {
        Format::Links => links::render(eps),
        Format::Base64 => base64::render(eps),
        Format::Happ => happ::render(cfg.happ_routing.as_deref(), eps),
        // A Clash dialect that can express none of the endpoints would hand
        // back a valid-but-empty document, which reads to the user as a broken
        // server. Fall back to the universal format instead.
        Format::Clash => {
            clash::render(eps, false, clash_profile).unwrap_or_else(|| base64::render(eps))
        }
        Format::ClashMeta => {
            clash::render(eps, true, clash_profile).unwrap_or_else(|| base64::render(eps))
        }
        Format::Singbox => singbox::render(eps),
        Format::Xray => xray::render(user, eps),
    }
}

/// The stand-in handed to a user who has been cut off, so their client shows
/// something explanatory instead of silently failing to connect.
pub fn blocked_endpoint(support_url: &str) -> Endpoint {
    Endpoint {
        node: format!("blocked — {support_url}"),
        flag: "⚠️".into(),
        suffix: None,
        scheme: "vless".into(),
        address: "127.0.0.1".into(),
        port: 1,
        uuid: "00000000-0000-0000-0000-000000000000".into(),
        params: BTreeMap::from([("security".to_string(), "none".to_string())]),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Documentation-reserved values only: TEST-NET-3 address, `.test` TLD,
    /// all-zero-ish uuid. Nothing here may resemble a real deployment.
    pub fn sample() -> Endpoint {
        Endpoint {
            node: "stockholm".into(),
            flag: "🇸🇪".into(),
            suffix: None,
            scheme: "vless".into(),
            address: "203.0.113.7".into(),
            port: 8443,
            uuid: "00000000-0000-0000-0000-000000000001".into(),
            params: [
                ("security", "reality"),
                ("sni", "example.test"),
                ("pbk", "testpbk"),
                ("sid", "testsid"),
                ("flow", "xtls-rprx-vision"),
                ("type", "tcp"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        }
    }

    pub fn sample_xhttp() -> Endpoint {
        let mut ep = sample();
        ep.params.remove("flow");
        for (k, v) in [
            ("type", "xhttp"),
            ("mode", "packet-up"),
            ("path", "/static/media"),
        ] {
            ep.params.insert(k.to_string(), v.to_string());
        }
        ep
    }

    /// The second inbound of the same node: hysteria2 beside the reality one,
    /// carrying the suffix that keeps the two labels apart.
    pub fn sample_hysteria() -> Endpoint {
        Endpoint {
            node: "stockholm".into(),
            flag: "🇸🇪".into(),
            suffix: Some("udp".into()),
            scheme: "hysteria2".into(),
            address: "203.0.113.7".into(),
            port: 443,
            uuid: "00000000-0000-0000-0000-000000000001".into(),
            params: [("sni", "example.test"), ("up", "50"), ("down", "100")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    pub fn sample_user() -> User {
        User {
            name: "tester".into(),
            group: "main".into(),
            uuid: "00000000-0000-0000-0000-000000000001".into(),
            sub_token: "testtoken".into(),
            limit: None,
            expires: None,
            admin: false,
        }
    }

    /// A config in exactly the shape the Nix side renders, kept here so a
    /// breaking change to the contract fails a test rather than a deployment.
    pub(crate) fn sample_config() -> Config {
        serde_json::from_str(
            r#"{
              "support_url": "https://example.test/support",
              "sub_domain": "sub.example.test",
              "title": "relay for {user}",
              "token": "testadmintoken",
              "happ_routing": "{\"Name\":\"Test\"}",
              "clash": {
                "selector": "TEST",
                "auto": "auto",
                "groups": [ { "name": "TEST-UDP", "scheme": "hysteria2" } ],
                "profile": "mode: rule\nrules:\n  - MATCH,TEST"
              },
              "groups": [
                { "name": "main",    "limit": "1T"   },
                { "name": "friends", "limit": "200G" }
              ],
              "nodes": [
                {
                  "name": "stockholm", "flag": "🇸🇪", "fqdn": "stockholm.example.test",
                  "api": "http://127.0.0.1:10086",
                  "inbounds": [
                    { "group": "main", "tag": "vless-main",
                      "link": { "scheme": "vless", "port": 8443,
                                "params": { "security": "reality", "type": "xhttp",
                                            "mode": "packet-up", "path": "/static/media" } } },
                    { "group": "main", "tag": "hysteria-main", "suffix": "udp",
                      "link": { "scheme": "hysteria2", "port": 443,
                                "params": { "sni": "stockholm.example.test",
                                            "up": "50", "down": "100" } } }
                  ]
                },
                {
                  "name": "asgard", "flag": "🇳🇴", "fqdn": "asgard.example.test",
                  "inbounds": [
                    { "group": "friends", "tag": "vless-friends",
                      "link": { "scheme": "vless", "port": 8443, "params": {} } }
                  ]
                }
              ],
              "users": [
                { "name": "me",    "group": "main",    "uuid": "00000000-0000-0000-0000-000000000001",
                  "sub_token": "tok-me", "admin": true },
                { "name": "buddy", "group": "friends", "uuid": "00000000-0000-0000-0000-000000000002",
                  "sub_token": "tok-buddy", "limit": "500G", "expires": "2027-03-24" },
                { "name": "guest", "group": "friends", "uuid": "00000000-0000-0000-0000-000000000003",
                  "sub_token": "tok-guest", "limit": "unlimited" }
              ]
            }"#,
        )
        .expect("sample config must match the Config contract")
    }

    #[test]
    fn config_defaults_fill_in() {
        let cfg = sample_config();
        assert_eq!(cfg.state_dir.to_str().unwrap(), "/var/lib/xctrl");
        assert_eq!(cfg.xray_api, "127.0.0.1:10085");
        assert_eq!(cfg.api_path, "/xctrl");
        // Quota defaults to the 1st at 01:00 +03:00.
        assert_eq!(cfg.quota.reset_day, 1);
        assert_eq!(cfg.quota.reset_hour, 1);
        assert_eq!(cfg.quota.reset_utc_offset_hours, 3);
    }

    #[test]
    fn a_user_only_sees_nodes_of_their_group() {
        let cfg = sample_config();
        let address_of = |node: &str| Some(format!("{node}.example.test"));

        // Two inbounds on one node means two endpoints, told apart by the suffix.
        let mine = build_endpoints(&cfg, cfg.user("me").unwrap(), address_of);
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().all(|ep| ep.node == "stockholm"));
        assert_eq!(mine[0].network(), "xhttp");
        assert_eq!(mine[0].param("mode"), "packet-up");
        assert_eq!(mine[0].label(), "🇸🇪 stockholm");
        assert_eq!(mine[1].scheme, "hysteria2");
        assert_eq!(mine[1].param("down"), "100");
        assert_eq!(mine[1].label(), "🇸🇪 stockholm udp");

        let theirs = build_endpoints(&cfg, cfg.user("buddy").unwrap(), address_of);
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].node, "asgard");
    }

    #[test]
    fn limits_resolve_user_then_group_then_unlimited() {
        let cfg = sample_config();
        // Own limit wins over the group default.
        assert_eq!(
            cfg.limit_of(cfg.user("buddy").unwrap()),
            Some(500 * (1 << 30))
        );
        // No own limit: the group default applies.
        assert_eq!(cfg.limit_of(cfg.user("me").unwrap()), Some(1 << 40));
        // An explicit opt-out beats the group default.
        assert_eq!(cfg.limit_of(cfg.user("guest").unwrap()), None);
    }

    #[test]
    fn expiry_is_optional_and_parsed_as_a_date() {
        let cfg = sample_config();
        assert_eq!(
            cfg.user("buddy").unwrap().expires,
            chrono::NaiveDate::from_ymd_opt(2027, 3, 24)
        );
        assert!(cfg.user("me").unwrap().expires.is_none());
    }

    #[test]
    fn client_policy_arrives_from_nix() {
        let cfg = sample_config();
        assert_eq!(cfg.happ_routing.as_deref(), Some(r#"{"Name":"Test"}"#));
        let clash = cfg.clash.as_ref().expect("profile parsed");
        assert_eq!(clash.selector, "TEST");
        assert_eq!(clash.auto.as_deref(), Some("auto"));
        assert_eq!(clash.groups.len(), 1);
        assert_eq!(clash.groups[0].name, "TEST-UDP");
        assert_eq!(clash.groups[0].scheme, "hysteria2");
        // The profile must own `rules:` and leave the proxies to the generator.
        assert!(clash.profile.contains("rules:"));
        assert!(!clash.profile.contains("proxies:"));
    }

    #[test]
    fn unresolved_nodes_are_skipped_not_faked() {
        let cfg = sample_config();
        let eps = build_endpoints(&cfg, cfg.user("me").unwrap(), |_| None);
        assert!(eps.is_empty());
    }
}

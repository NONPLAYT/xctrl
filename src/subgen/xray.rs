//! Xray client JSON: an outbound per endpoint, for clients that take a raw
//! core config rather than a subscription dialect.

use serde_json::{Value, json};

use super::{Endpoint, Rendered};
use crate::config::User;

pub fn render(_user: &User, eps: &[Endpoint]) -> Rendered {
    let outbounds: Vec<Value> = eps.iter().filter_map(outbound).collect();
    let doc = json!({ "outbounds": outbounds });
    Rendered {
        body: serde_json::to_vec_pretty(&doc).unwrap_or_default(),
        content_type: "application/json; charset=utf-8",
    }
}

fn outbound(ep: &Endpoint) -> Option<Value> {
    let settings = match ep.scheme.as_str() {
        "vless" => json!({
            "vnext": [{
                "address": ep.address,
                "port": ep.port,
                "users": [{
                    "id": ep.uuid,
                    "encryption": "none",
                    "flow": ep.param("flow"),
                }],
            }],
        }),
        "trojan" => json!({
            "servers": [{
                "address": ep.address,
                "port": ep.port,
                "password": ep.uuid,
            }],
        }),
        // Xray spells the protocol "hysteria" and splits it in two: the server
        // endpoint in settings, the credential in the transport. Both halves
        // insist on version 2, and the core refuses the config otherwise.
        "hysteria2" | "hy2" => {
            return Some(json!({
                "tag": ep.label(),
                "protocol": "hysteria",
                "settings": {
                    "version": 2,
                    "address": ep.address,
                    "port": ep.port,
                },
                "streamSettings": {
                    "network": "hysteria",
                    "security": "tls",
                    "tlsSettings": {
                        "serverName": ep.sni(),
                        "alpn": ["h3"],
                        "allowInsecure": ep.param("insecure") == "1",
                    },
                    "hysteriaSettings": { "version": 2, "auth": ep.uuid },
                },
            }));
        }
        _ => return None,
    };

    let mut stream = json!({ "network": ep.network() });
    let map = stream.as_object_mut()?;
    if ep.is_reality() {
        map.insert("security".into(), json!("reality"));
        map.insert(
            "realitySettings".into(),
            json!({
                "serverName": ep.sni(),
                "publicKey": ep.param("pbk"),
                "shortId": ep.param("sid"),
                "fingerprint": ep.fingerprint(),
            }),
        );
    } else if ep.is_tls() {
        map.insert("security".into(), json!("tls"));
        map.insert(
            "tlsSettings".into(),
            json!({ "serverName": ep.sni(), "fingerprint": ep.fingerprint() }),
        );
    }

    Some(json!({
        "tag": ep.label(),
        "protocol": ep.scheme,
        "settings": settings,
        "streamSettings": stream,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgen::tests::{sample, sample_hysteria, sample_user};

    #[test]
    fn renders_a_reality_outbound() {
        let out = render(&sample_user(), &[sample()]);
        let doc: Value = serde_json::from_slice(&out.body).unwrap();
        let ob = &doc["outbounds"][0];
        assert_eq!(ob["protocol"], "vless");
        assert_eq!(ob["settings"]["vnext"][0]["port"], 8443);
        assert_eq!(
            ob["settings"]["vnext"][0]["users"][0]["flow"],
            "xtls-rprx-vision"
        );
        assert_eq!(ob["streamSettings"]["security"], "reality");
        assert_eq!(
            ob["streamSettings"]["realitySettings"]["publicKey"],
            "testpbk"
        );
        assert_eq!(ob["streamSettings"]["network"], "tcp");
    }

    #[test]
    fn renders_a_hysteria_outbound_in_both_halves() {
        let out = render(&sample_user(), &[sample_hysteria()]);
        let doc: Value = serde_json::from_slice(&out.body).unwrap();
        let ob = &doc["outbounds"][0];
        // Xray names the protocol "hysteria" even though the link says hysteria2.
        assert_eq!(ob["protocol"], "hysteria");
        assert_eq!(ob["settings"]["version"], 2);
        assert_eq!(ob["settings"]["port"], 443);
        assert_eq!(ob["streamSettings"]["network"], "hysteria");
        assert_eq!(ob["streamSettings"]["security"], "tls");
        assert_eq!(ob["streamSettings"]["tlsSettings"]["alpn"][0], "h3");
        assert_eq!(ob["streamSettings"]["hysteriaSettings"]["version"], 2);
        assert_eq!(
            ob["streamSettings"]["hysteriaSettings"]["auth"],
            "00000000-0000-0000-0000-000000000001"
        );
    }
}

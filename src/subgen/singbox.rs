//! sing-box JSON: an outbound per endpoint plus a selector over them.

use serde_json::{Value, json};

use super::{Endpoint, Rendered};

const SELECTOR: &str = "xctrl";

pub fn render(eps: &[Endpoint]) -> Rendered {
    let mut outbounds: Vec<Value> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    for ep in eps {
        let Some(ob) = outbound(ep) else { continue };
        tags.push(ep.label());
        outbounds.push(ob);
    }
    outbounds.push(json!({
        "type": "selector",
        "tag": SELECTOR,
        "outbounds": tags,
    }));
    let doc = json!({ "outbounds": outbounds });
    Rendered {
        body: serde_json::to_vec_pretty(&doc).unwrap_or_default(),
        content_type: "application/json; charset=utf-8",
    }
}

fn outbound(ep: &Endpoint) -> Option<Value> {
    let mut ob = match ep.scheme.as_str() {
        "vless" if ep.network() != "tcp" => return None,
        "vless" => json!({
            "type": "vless",
            "tag": ep.label(),
            "server": ep.address,
            "server_port": ep.port,
            "uuid": ep.uuid,
        }),
        "trojan" => json!({
            "type": "trojan",
            "tag": ep.label(),
            "server": ep.address,
            "server_port": ep.port,
            "password": ep.uuid,
        }),
        // Returned whole: hysteria2 carries its own QUIC TLS, so none of the
        // uTLS/reality tail below applies to it.
        "hysteria2" | "hy2" => {
            let mut ob = json!({
                "type": "hysteria2",
                "tag": ep.label(),
                "server": ep.address,
                "server_port": ep.port,
                "password": ep.uuid,
                "tls": {
                    "enabled": true,
                    "server_name": ep.sni(),
                    "insecure": ep.param("insecure") == "1",
                    "alpn": ["h3"],
                },
            });
            if let (Ok(up), Ok(down)) = (
                ep.param("up").parse::<u32>(),
                ep.param("down").parse::<u32>(),
            ) {
                let map = ob.as_object_mut()?;
                map.insert("up_mbps".into(), json!(up));
                map.insert("down_mbps".into(), json!(down));
            }
            return Some(ob);
        }
        _ => return None,
    };
    let map = ob.as_object_mut()?;
    if ep.scheme == "vless" && !ep.param("flow").is_empty() {
        map.insert("flow".into(), json!(ep.param("flow")));
    }
    if ep.is_tls() {
        let mut tls = json!({
            "enabled": true,
            "server_name": ep.sni(),
            "utls": { "enabled": true, "fingerprint": ep.fingerprint() },
        });
        if ep.is_reality() {
            tls.as_object_mut()?.insert(
                "reality".into(),
                json!({
                    "enabled": true,
                    "public_key": ep.param("pbk"),
                    "short_id": ep.param("sid"),
                }),
            );
        }
        map.insert("tls".into(), tls);
    }
    Some(ob)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgen::tests::{sample, sample_hysteria, sample_xhttp};

    #[test]
    fn renders_a_reality_outbound_and_selector() {
        let out = render(&[sample()]);
        let doc: Value = serde_json::from_slice(&out.body).unwrap();
        let obs = doc["outbounds"].as_array().unwrap();
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0]["type"], "vless");
        assert_eq!(obs[0]["server_port"], 8443);
        assert_eq!(obs[0]["flow"], "xtls-rprx-vision");
        assert_eq!(obs[0]["tls"]["reality"]["public_key"], "testpbk");
        assert_eq!(obs[0]["tls"]["utls"]["fingerprint"], "chrome");
        assert_eq!(obs[1]["type"], "selector");
        assert_eq!(obs[1]["outbounds"][0], "🇸🇪 stockholm");
    }

    #[test]
    fn an_xhttp_endpoint_is_left_out_rather_than_mangled() {
        let out = render(&[sample_xhttp(), sample_hysteria()]);
        let doc: Value = serde_json::from_slice(&out.body).unwrap();
        let obs = doc["outbounds"].as_array().unwrap();
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0]["type"], "hysteria2");
        assert_eq!(obs[1]["outbounds"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn renders_a_hysteria2_outbound() {
        let out = render(&[sample_hysteria()]);
        let doc: Value = serde_json::from_slice(&out.body).unwrap();
        let ob = &doc["outbounds"][0];
        assert_eq!(ob["type"], "hysteria2");
        assert_eq!(ob["tag"], "\u{1f1f8}\u{1f1ea} stockholm udp");
        assert_eq!(ob["server_port"], 443);
        assert_eq!(ob["password"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(ob["tls"]["server_name"], "example.test");
        assert_eq!(ob["tls"]["insecure"], false);
        // No uTLS or reality on a QUIC protocol.
        assert!(ob["tls"].get("utls").is_none());
        assert!(ob["tls"].get("reality").is_none());
        assert_eq!(ob["up_mbps"], 50);
        assert_eq!(ob["down_mbps"], 100);
    }
}

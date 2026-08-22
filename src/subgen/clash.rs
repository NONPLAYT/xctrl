use super::{Endpoint, Rendered};

/// Proxy group name; also what the generated rule points at.
const GROUP: &str = "XCTRL";

/// Returns `None` when the dialect can express none of the endpoints, so the
/// caller can fall back rather than serve an empty proxy list.
pub fn render(eps: &[Endpoint], meta: bool) -> Option<Rendered> {
    let mut out = String::from("proxies:\n");
    let mut names = Vec::new();
    for ep in eps {
        let Some(block) = proxy(ep, meta) else {
            continue; // protocol this dialect cannot express
        };
        out.push_str(&block);
        out.push('\n');
        names.push(ep.label());
    }
    out.push_str("proxy-groups:\n");
    out.push_str(&format!(
        "  - name: {GROUP}\n    type: select\n    proxies:\n"
    ));
    for name in &names {
        out.push_str(&format!("      - {}\n", quote(name)));
    }
    if names.is_empty() && !eps.is_empty() {
        return None;
    }
    out.push_str(&format!("rules:\n  - MATCH,{GROUP}\n"));
    Some(Rendered {
        body: out.into_bytes(),
        content_type: "text/yaml; charset=utf-8",
    })
}

/// Renders one proxy as a YAML list item, or `None` when this Clash dialect
/// cannot express the endpoint.
fn proxy(ep: &Endpoint, meta: bool) -> Option<String> {
    let mut kv: Vec<(&str, String)> = vec![
        ("name", quote(&ep.label())),
        ("server", ep.address.clone()),
        ("port", ep.port.to_string()),
    ];
    match ep.scheme.as_str() {
        "vless" => {
            // Plain Clash has no vless support at all.
            if !meta {
                return None;
            }
            kv.insert(1, ("type", "vless".into()));
            kv.push(("uuid", quote(&ep.uuid)));
            kv.push(("udp", "true".into()));
            if !ep.param("flow").is_empty() {
                kv.push(("flow", ep.param("flow").into()));
            }
        }
        "trojan" => {
            kv.insert(1, ("type", "trojan".into()));
            kv.push(("password", quote(&ep.uuid)));
            kv.push(("udp", "true".into()));
        }
        _ => return None,
    }
    if ep.is_tls() {
        kv.push(("tls", "true".into()));
        kv.push(("servername", quote(ep.sni())));
        kv.push(("client-fingerprint", ep.fingerprint().into()));
    }
    kv.push(("network", ep.network().into()));
    let mut block = block(&kv);
    if ep.is_reality() {
        if !meta {
            return None; // reality is a Meta-only extension
        }
        block.push_str(&format!(
            "\n    reality-opts:\n      public-key: {}\n      short-id: {}",
            quote(ep.param("pbk")),
            quote(ep.param("sid"))
        ));
    }
    Some(block)
}

/// Ordered key/values as a YAML list item: the first key carries the dash.
fn block(kv: &[(&str, String)]) -> String {
    kv.iter()
        .enumerate()
        .map(|(i, (k, v))| {
            let indent = if i == 0 { "  - " } else { "    " };
            format!("{indent}{k}: {v}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Double-quoted YAML scalar. Labels carry emoji and spaces, so quoting is not optional.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgen::tests::sample;

    fn rendered(eps: &[Endpoint], meta: bool) -> String {
        String::from_utf8(render(eps, meta).expect("dialect renders these").body).unwrap()
    }

    #[test]
    fn meta_renders_vless_reality() {
        let out = rendered(&[sample()], true);
        assert!(out.contains("type: vless"));
        assert!(out.contains("uuid: \"00000000-0000-0000-0000-000000000001\""));
        assert!(out.contains("flow: xtls-rprx-vision"));
        assert!(out.contains("reality-opts:"));
        assert!(out.contains("public-key: \"testpbk\""));
        assert!(out.contains("short-id: \"testsid\""));
        assert!(out.contains("client-fingerprint: chrome"));
        assert!(out.contains("MATCH,XCTRL"));
    }

    #[test]
    fn plain_clash_cannot_express_vless() {
        // Rather than an empty document, the caller is told to fall back.
        assert!(render(&[sample()], false).is_none());
    }

    #[test]
    fn an_empty_subscription_is_still_valid_clash() {
        // No endpoints at all is a real state, not a dialect failure.
        let out = String::from_utf8(render(&[], true).unwrap().body).unwrap();
        assert!(out.starts_with("proxies:\n"));
        assert!(out.contains("MATCH,XCTRL"));
    }

    #[test]
    fn labels_with_emoji_are_quoted() {
        let out = rendered(&[sample()], true);
        assert!(out.contains("name: \"🇸🇪 stockholm\""));
        assert!(out.contains("      - \"🇸🇪 stockholm\""));
    }
}

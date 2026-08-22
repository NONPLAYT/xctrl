use super::{Endpoint, Rendered};
use crate::config::ClashProfile;

/// Selector name used when the operator supplies no profile of their own.
const GROUP: &str = "XCTRL";

/// Returns `None` when the dialect can express none of the endpoints, so the
/// caller can fall back rather than serve an empty proxy list.
///
/// The generator owns `proxies:` and `proxy-groups:`; a profile supplies the
/// rest of the document and is appended verbatim.
pub fn render(eps: &[Endpoint], meta: bool, profile: Option<&ClashProfile>) -> Option<Rendered> {
    let selector = profile.map_or(GROUP, |p| p.selector.as_str());
    let auto = profile.and_then(|p| p.auto.as_deref());

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
    if names.is_empty() && !eps.is_empty() {
        return None;
    }

    // A group with no members is a load error, so an empty subscription still
    // has to point the selector at something real.
    let members: Vec<String> = if names.is_empty() {
        vec!["DIRECT".into()]
    } else {
        names.iter().map(|n| quote(n)).collect()
    };

    out.push_str("proxy-groups:\n");
    out.push_str(&format!(
        "  - name: {}\n    type: select\n    proxies:\n",
        quote(selector)
    ));
    for member in auto.iter().map(|a| quote(a)).chain(members.iter().cloned()) {
        out.push_str(&format!("      - {member}\n"));
    }
    if let Some(auto) = auto {
        for line in [
            format!("  - name: {}", quote(auto)),
            "    type: url-test".into(),
            "    tolerance: 150".into(),
            "    interval: 300".into(),
            "    url: https://www.gstatic.com/generate_204".into(),
            "    hidden: true".into(),
            "    proxies:".into(),
        ] {
            out.push_str(&line);
            out.push('\n');
        }
        for member in &members {
            out.push_str(&format!("      - {member}\n"));
        }
    }

    match profile {
        Some(p) => {
            out.push('\n');
            out.push_str(p.profile.trim_end());
            out.push('\n');
        }
        None => out.push_str(&format!("rules:\n  - MATCH,{selector}\n")),
    }
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
        String::from_utf8(render(eps, meta, None).expect("dialect renders these").body).unwrap()
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
        assert!(render(&[sample()], false, None).is_none());
    }

    #[test]
    fn an_empty_subscription_is_still_valid_clash() {
        // No endpoints at all is a real state, not a dialect failure.
        let out = String::from_utf8(render(&[], true, None).unwrap().body).unwrap();
        assert!(out.starts_with("proxies:\n"));
        assert!(out.contains("MATCH,XCTRL"));
    }

    #[test]
    fn a_profile_replaces_the_generated_rules() {
        let profile = ClashProfile {
            selector: "NEXON".into(),
            auto: Some("\u{26a1}\u{fe0f} Auto".into()),
            profile: "mode: rule\nrules:\n  - MATCH,NEXON".into(),
        };
        let out =
            String::from_utf8(render(&[sample()], true, Some(&profile)).unwrap().body).unwrap();
        // Exactly one of each top-level key, or mihomo refuses the document.
        for key in ["proxies:", "proxy-groups:", "rules:", "mode:"] {
            assert_eq!(
                out.matches(&format!("\n{key}")).count() + usize::from(out.starts_with(key)),
                1,
                "{key}"
            );
        }
        assert!(out.contains("  - name: \"NEXON\"\n    type: select"));
        assert!(out.contains("type: url-test"));
        // The auto group leads the selector and lists the nodes itself.
        let auto = "      - \"\u{26a1}\u{fe0f} Auto\"\n";
        assert!(out.contains(&format!("{auto}      - \"\u{1f1f8}\u{1f1ea} stockholm\"")));
        assert!(out.trim_end().ends_with("- MATCH,NEXON"));
    }

    #[test]
    fn an_empty_group_still_points_somewhere_real() {
        let profile = ClashProfile {
            selector: "NEXON".into(),
            auto: None,
            profile: "rules:\n  - MATCH,NEXON".into(),
        };
        let out = String::from_utf8(render(&[], true, Some(&profile)).unwrap().body).unwrap();
        assert!(out.contains("      - DIRECT"));
    }

    #[test]
    fn labels_with_emoji_are_quoted() {
        let out = rendered(&[sample()], true);
        assert!(out.contains("name: \"🇸🇪 stockholm\""));
        assert!(out.contains("      - \"🇸🇪 stockholm\""));
    }
}

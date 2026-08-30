use super::{Endpoint, Rendered};
use crate::config::{ClashGroup, ClashProfile};

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

    for group in profile.map(|p| p.groups.as_slice()).unwrap_or_default() {
        out.push_str(&scheme_group(group, eps, meta, selector));
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

/// A selector holding only the endpoints of one scheme.
///
/// It is emitted even with nothing to hold, pointing at the main selector
/// instead: the profile's rules name it unconditionally, and a rule naming a
/// group that does not exist fails the whole document. Falling back to the main
/// selector also means a user whose group has no such inbound keeps working,
/// just over the ordinary protocol.
fn scheme_group(group: &ClashGroup, eps: &[Endpoint], meta: bool, selector: &str) -> String {
    let members: Vec<String> = eps
        .iter()
        .filter(|ep| ep.scheme == group.scheme && proxy(ep, meta).is_some())
        .map(|ep| quote(&ep.label()))
        .collect();
    let members = if members.is_empty() {
        vec![quote(selector)]
    } else {
        members
    };
    let mut out = format!(
        "  - name: {}\n    type: select\n    proxies:\n",
        quote(&group.name)
    );
    for member in members {
        out.push_str(&format!("      - {member}\n"));
    }
    out
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
        // Hysteria2 is QUIC end to end: it carries its own TLS and has no
        // transport to name, so it returns here rather than falling through to
        // the tls/network tail below.
        "hysteria2" | "hy2" => {
            if !meta {
                return None; // Meta-only, like vless
            }
            kv.insert(1, ("type", "hysteria2".into()));
            kv.push(("password", quote(&ep.uuid)));
            kv.push(("sni", quote(ep.sni())));
            if ep.param("insecure") == "1" {
                kv.push(("skip-cert-verify", "true".into()));
            }
            if !ep.param("obfs").is_empty() {
                kv.push(("obfs", ep.param("obfs").into()));
                kv.push(("obfs-password", quote(ep.param("obfs-password"))));
            }
            return Some(block(&kv));
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
    use crate::subgen::tests::{sample, sample_hysteria};

    fn profile(groups: Vec<ClashGroup>) -> ClashProfile {
        ClashProfile {
            selector: "NEXON".into(),
            auto: None,
            groups,
            profile: "rules:\n  - MATCH,NEXON".into(),
        }
    }

    fn udp_group() -> Vec<ClashGroup> {
        vec![ClashGroup {
            name: "NEXON-UDP".into(),
            scheme: "hysteria2".into(),
        }]
    }

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
            groups: Vec::new(),
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
            groups: Vec::new(),
            profile: "rules:\n  - MATCH,NEXON".into(),
        };
        let out = String::from_utf8(render(&[], true, Some(&profile)).unwrap().body).unwrap();
        assert!(out.contains("      - DIRECT"));
    }

    #[test]
    fn meta_renders_hysteria2_without_a_transport() {
        let out = rendered(&[sample_hysteria()], true);
        assert!(out.contains("type: hysteria2"));
        assert!(out.contains("port: 443"));
        assert!(out.contains("password: \"00000000-0000-0000-0000-000000000001\""));
        assert!(out.contains("sni: \"example.test\""));
        // QUIC carries its own TLS: none of the stream-transport keys apply.
        for absent in [
            "network:",
            "tls: true",
            "servername:",
            "client-fingerprint:",
        ] {
            assert!(!out.contains(absent), "{absent}");
        }
    }

    #[test]
    fn plain_clash_cannot_express_hysteria2_either() {
        assert!(render(&[sample_hysteria()], false, None).is_none());
    }

    #[test]
    fn a_scheme_group_holds_only_that_scheme() {
        let eps = [sample(), sample_hysteria()];
        let out = String::from_utf8(
            render(&eps, true, Some(&profile(udp_group())))
                .unwrap()
                .body,
        )
        .unwrap();
        let group = out
            .split("  - name: \"NEXON-UDP\"")
            .nth(1)
            .expect("the group is emitted");
        assert!(group.contains("      - \"🇸🇪 stockholm udp\""));
        // The reality endpoint belongs to the main selector, not this one.
        assert!(!group.contains("      - \"🇸🇪 stockholm\"\n"));
    }

    #[test]
    fn a_scheme_group_with_no_members_falls_back_to_the_selector() {
        // A user whose group has no hysteria inbound still gets a loadable
        // document: the rules name NEXON-UDP either way.
        let out = String::from_utf8(
            render(&[sample()], true, Some(&profile(udp_group())))
                .unwrap()
                .body,
        )
        .unwrap();
        let group = out.split("  - name: \"NEXON-UDP\"").nth(1).unwrap();
        assert!(group.contains("      - \"NEXON\""));
    }

    #[test]
    fn two_inbounds_of_one_node_do_not_collide() {
        let out = rendered(&[sample(), sample_hysteria()], true);
        assert!(out.contains("name: \"🇸🇪 stockholm\"\n"));
        assert!(out.contains("name: \"🇸🇪 stockholm udp\"\n"));
    }

    #[test]
    fn labels_with_emoji_are_quoted() {
        let out = rendered(&[sample()], true);
        assert!(out.contains("name: \"🇸🇪 stockholm\""));
        assert!(out.contains("      - \"🇸🇪 stockholm\""));
    }
}

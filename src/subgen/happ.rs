use base64::prelude::*;

use super::{Endpoint, Rendered, links};

pub fn routing_link(routing: &str) -> String {
    format!(
        "happ://routing/onadd/{}",
        BASE64_STANDARD_NO_PAD.encode(routing.trim())
    )
}

pub fn body(routing: Option<&str>, eps: &[Endpoint]) -> String {
    let links = links::body(eps);
    let plain = match routing {
        Some(routing) if !routing.trim().is_empty() => {
            format!("{}\n{links}", routing_link(routing))
        }
        _ => links,
    };
    BASE64_STANDARD.encode(plain)
}

pub fn render(routing: Option<&str>, eps: &[Endpoint]) -> Rendered {
    Rendered {
        body: body(routing, eps).into_bytes(),
        content_type: "text/plain; charset=utf-8",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgen::tests::sample;

    fn decode(body: &str) -> String {
        String::from_utf8(BASE64_STANDARD.decode(body).unwrap()).unwrap()
    }

    #[test]
    fn the_routing_profile_leads_the_link_list() {
        let out = decode(&body(Some(r#"{"Name":"Test"}"#), &[sample()]));
        let mut lines = out.lines();
        let first = lines.next().unwrap();
        assert!(first.starts_with("happ://routing/onadd/"));
        // Unpadded, so the client's decoder is never handed a stray '='.
        assert!(!first.ends_with('='));
        assert!(lines.next().unwrap().starts_with("vless://"));
    }

    #[test]
    fn the_payload_round_trips_to_the_original_json() {
        let json = r#"{"Name":"Test","DirectSites":["geosite:private"]}"#;
        let encoded = routing_link(json)
            .strip_prefix("happ://routing/onadd/")
            .unwrap()
            .to_string();
        let decoded = BASE64_STANDARD_NO_PAD.decode(encoded).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), json);
    }

    #[test]
    fn without_a_profile_it_is_a_plain_base64_list() {
        let eps = [sample()];
        assert_eq!(decode(&body(None, &eps)), links::body(&eps));
    }
}

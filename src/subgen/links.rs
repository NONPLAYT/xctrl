//! Plain URI list, one per line — what most clients import by paste.

use super::{Endpoint, Rendered};

/// Builds the connection URI for one endpoint, with the label as the fragment.
pub fn uri(ep: &Endpoint) -> String {
    let query: Vec<String> = ep
        .params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect();
    let mut uri = format!("{}://{}@{}:{}", ep.scheme, ep.uuid, ep.address, ep.port);
    if !query.is_empty() {
        uri.push('?');
        uri.push_str(&query.join("&"));
    }
    uri.push('#');
    uri.push_str(&urlencoding::encode(&ep.label()));
    uri
}

pub fn body(eps: &[Endpoint]) -> String {
    eps.iter().map(uri).collect::<Vec<_>>().join("\n")
}

pub fn render(eps: &[Endpoint]) -> Rendered {
    Rendered {
        body: body(eps).into_bytes(),
        content_type: "text/plain; charset=utf-8",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgen::tests::{sample, sample_xhttp};

    #[test]
    fn builds_a_reality_uri() {
        let uri = uri(&sample());
        assert!(uri.starts_with("vless://00000000-0000-0000-0000-000000000001@203.0.113.7:8443?"));
        assert!(uri.contains("security=reality"));
        assert!(uri.contains("pbk=testpbk"));
        assert!(uri.contains("flow=xtls-rprx-vision"));
        // The label is percent-encoded in the fragment.
        assert!(uri.contains('#'));
        assert!(uri.ends_with("stockholm"));
    }

    #[test]
    fn an_xhttp_uri_carries_its_transport() {
        let uri = uri(&sample_xhttp());
        assert!(uri.contains("type=xhttp"));
        assert!(uri.contains("mode=packet-up"));
        assert!(uri.contains("path=%2Fstatic%2Fmedia"));
        assert!(!uri.contains("flow="));
    }

    #[test]
    fn joins_one_per_line() {
        let eps = vec![sample(), sample()];
        assert_eq!(body(&eps).lines().count(), 2);
    }
}

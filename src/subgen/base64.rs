use base64::prelude::*;

use super::{Endpoint, Rendered, links};

pub fn body(eps: &[Endpoint]) -> String {
    BASE64_STANDARD.encode(links::body(eps))
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
    use crate::subgen::tests::sample;

    #[test]
    fn round_trips_to_the_link_list() {
        let eps = vec![sample()];
        let decoded = BASE64_STANDARD.decode(body(&eps)).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), links::body(&eps));
    }
}

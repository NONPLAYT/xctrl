use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Links,
    Base64,
    Clash,
    ClashMeta,
    Happ,
    Singbox,
    Xray,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Links => "links",
            Format::Base64 => "base64",
            Format::Clash => "clash",
            Format::ClashMeta => "clash-meta",
            Format::Happ => "happ",
            Format::Singbox => "singbox",
            Format::Xray => "xray",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "links" => Format::Links,
            "base64" => Format::Base64,
            "clash" => Format::Clash,
            "clash-meta" => Format::ClashMeta,
            "happ" => Format::Happ,
            "singbox" => Format::Singbox,
            "xray" => Format::Xray,
            _ => return None,
        })
    }
}

const RULES: &[(&str, Format)] = &[
    (
        r"^([Cc]lash[\-\.]?[Vv]erge|[Cc]lash[\-\.]?[Mm]eta|[Ff][Ll][Cc]lash|[Mm]ihomo|[Ss]tash)",
        Format::ClashMeta,
    ),
    (r"^([Cc]lash|[Ss]tash)", Format::Clash),
    (r"^[Hh]app", Format::Happ),
    (r"INCY/[\d.]+", Format::Xray),
    (
        r"^([Vv]2rayNG|[Vv]2rayN|[Ss]treisand|[Kk]tor\-client)",
        Format::Xray,
    ),
    (
        r"^(SFA|SFI|SFM|SFT|[Kk]aring|[Hh]iddify[Nn]ext)|.*[Ss]ing[-_]?box.*",
        Format::Singbox,
    ),
];

static COMPILED: LazyLock<Vec<(Regex, Format)>> = LazyLock::new(|| {
    RULES
        .iter()
        .map(|(pattern, format)| {
            let re = Regex::new(pattern).expect("built-in UA pattern must compile");
            (re, *format)
        })
        .collect()
});

pub fn detect(ua: &str) -> Format {
    COMPILED
        .iter()
        .find(|(re, _)| re.is_match(ua))
        .map(|(_, format)| *format)
        .unwrap_or(Format::Base64)
}

pub fn is_browser(ua: &str) -> bool {
    ua.starts_with("Mozilla/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_clients() {
        assert_eq!(detect("mihomo/1.19.0"), Format::ClashMeta);
        assert_eq!(detect("clash-verge/2.0.0"), Format::ClashMeta);
        assert_eq!(detect("Clash/1.0"), Format::Clash);
        assert_eq!(detect("v2rayNG/1.8.0"), Format::Xray);
        assert_eq!(detect("Streisand/1.0"), Format::Xray);
        assert_eq!(detect("INCY/2.1"), Format::Xray);
        assert_eq!(detect("SFI/1.9"), Format::Singbox);
        assert_eq!(detect("sing-box 1.9"), Format::Singbox);
        assert_eq!(detect("Happ/1.0"), Format::Happ);
    }

    #[test]
    fn clash_meta_wins_over_plain_clash() {
        assert_eq!(detect("clash-meta/1.0"), Format::ClashMeta);
        assert_eq!(detect("Stash/2.0"), Format::ClashMeta);
    }

    #[test]
    fn unknown_falls_back_to_base64() {
        assert_eq!(detect("curl/8.5.0"), Format::Base64);
        assert_eq!(detect(""), Format::Base64);
    }

    #[test]
    fn browsers_are_separate() {
        assert!(is_browser("Mozilla/5.0 (X11; Linux x86_64)"));
        assert!(!is_browser("mihomo/1.19.0"));
    }
}

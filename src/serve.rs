use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::prelude::*;
use fast_qr::convert::Builder;
use fast_qr::convert::svg::SvgBuilder;
use fast_qr::{ECL, QRBuilder};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

use crate::config::{Config, User};
use crate::controller::{self, Controller};
use crate::detect;
use crate::remote;
use crate::subgen;

const RESOLVE_RETRY: Duration = Duration::from_secs(5);
const RESOLVE_REFRESH: Duration = Duration::from_secs(300);
const UPDATE_INTERVAL_H: u32 = 1;

struct App {
    ctl: Arc<Controller>,
    env: minijinja::Environment<'static>,
    /// node name to dial address, kept fresh by the resolver task.
    addrs: RwLock<BTreeMap<String, String>>,
}

pub async fn run(cfg: Config) -> Result<()> {
    let mut env = minijinja::Environment::new();
    env.add_template("index.html", include_str!("../templates/index.html"))?;

    // Seed with whatever the config states; the resolver refines it.
    let addrs = cfg
        .nodes
        .iter()
        .map(|n| {
            let addr = n.addr.clone().unwrap_or_else(|| n.fqdn.clone());
            (n.name.clone(), addr)
        })
        .collect();

    let ctl = Controller::new(cfg)?;
    let app = Arc::new(App {
        env,
        addrs: RwLock::new(addrs),
        ctl: ctl.clone(),
    });

    tokio::spawn(resolver(app.clone()));
    tokio::spawn(controller::run_poller(ctl.clone()));
    tokio::spawn(admin(app.clone()));

    let listen = app.ctl.cfg.serve_listen.clone();
    let router = Router::new()
        .route("/", get(teapot))
        .route("/{token}", get(subscription))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("subscriptions listening on {listen}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn teapot() -> StatusCode {
    StatusCode::IM_A_TEAPOT
}

// ---------------------------------------------------------------- management

async fn admin(app: Arc<App>) {
    let listen = app.ctl.cfg.admin_listen.clone();
    let router = Router::new()
        .route("/ledger", get(ledger))
        .route("/status", get(status))
        .route("/block", post(block))
        .route("/unblock", post(unblock))
        .route("/sync", post(sync_now))
        .route_layer(middleware::from_fn_with_state(app.clone(), auth))
        .with_state(app);
    match tokio::net::TcpListener::bind(&listen).await {
        Ok(listener) => {
            eprintln!("admin api listening on {listen}");
            if let Err(e) = axum::serve(listener, router).await {
                eprintln!("admin api: {e:#}");
            }
        }
        Err(e) => eprintln!("bind admin api on {listen}: {e:#}"),
    }
}

async fn auth(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let expected = format!("Bearer {}", app.ctl.cfg.token);
    let got = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if got == Some(expected.as_str()) {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

async fn ledger(State(app): State<Arc<App>>) -> Response {
    let ledger = app.ctl.ledger.read().await.clone();
    let nodes = app.ctl.nodes.read().await.clone();
    let limits: BTreeMap<&str, Option<u64>> = app
        .ctl
        .cfg
        .users
        .iter()
        .map(|u| (u.name.as_str(), app.ctl.cfg.limit_of(u)))
        .collect();
    Json(json!({
        "ledger": ledger,
        "nodes": nodes,
        "limits": limits,
        "next_reset": app.ctl.cfg.quota.next_reset(chrono::Utc::now()),
    }))
    .into_response()
}

async fn status(State(app): State<Arc<App>>) -> Response {
    let mut out = Vec::new();
    for node in &app.ctl.cfg.nodes {
        let up = remote::is_up(&app.ctl.client, &app.ctl.cfg, node).await;
        out.push(json!({ "node": node.name, "up": up }));
    }
    Json(out).into_response()
}

async fn block(State(app): State<Arc<App>>, body: String) -> Response {
    set_blocked(app, body, true).await
}

async fn unblock(State(app): State<Arc<App>>, body: String) -> Response {
    set_blocked(app, body, false).await
}

async fn set_blocked(app: Arc<App>, user: String, blocked: bool) -> Response {
    match app.ctl.set_blocked(user.trim(), blocked).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

async fn sync_now(State(app): State<Arc<App>>) -> Response {
    match app.ctl.cycle().await {
        Ok(()) => {
            app.ctl.push().await;
            StatusCode::OK.into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// -------------------------------------------------------------- subscription

/// `?format=` pins the output when User-Agent detection guesses wrong, which
/// happens with clients that do not identify themselves.
#[derive(Deserialize)]
struct SubQuery {
    #[serde(default)]
    format: Option<String>,
}

async fn subscription(
    State(app): State<Arc<App>>,
    Path(token): Path<String>,
    Query(query): Query<SubQuery>,
    headers: HeaderMap,
) -> Response {
    let cfg = &app.ctl.cfg;
    // An unknown token must not distinguish itself from the site having nothing
    // at that path, so it gets the same reply as the root.
    let Some(user) = cfg.user_by_token(&token) else {
        return StatusCode::IM_A_TEAPOT.into_response();
    };

    let cut_off = app.ctl.ledger.read().await.is_cut_off(&user.name);
    let endpoints = if cut_off {
        vec![subgen::blocked_endpoint(&cfg.support_url)]
    } else {
        let addrs = app.addrs.read().await;
        subgen::build_endpoints(cfg, user, |node| addrs.get(node).cloned())
    };

    let sub_url = format!("https://{}/{token}", cfg.sub_domain);
    let header_of = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let ua = header_of(header::USER_AGENT);

    let pinned = query.format.as_deref().and_then(detect::Format::parse);
    if pinned.is_none()
        && (detect::is_browser(&ua) || header_of(header::ACCEPT).contains("text/html"))
    {
        return dashboard(&app, user, &sub_url, &endpoints, cut_off).await;
    }

    let format = pinned.unwrap_or_else(|| detect::detect(&ua));
    eprintln!("sub {} -> {} (ua: {ua})", user.name, format.as_str());
    let rendered = subgen::render(cfg, format, user, &endpoints);
    let (up, down) = split_traffic(&app, &user.name).await;
    let limit = cfg.limit_of(user).unwrap_or(0);
    let expire = user
        .expires
        .map(|d| cfg.quota.expiry(d).timestamp())
        .unwrap_or(0);
    let title = profile_title(cfg, user, cut_off);

    (
        [
            (header::CONTENT_TYPE, rendered.content_type.to_string()),
            (
                header::HeaderName::from_static("profile-title"),
                format!("base64:{}", BASE64_STANDARD.encode(title)),
            ),
            (
                header::HeaderName::from_static("subscription-userinfo"),
                format!("upload={up}; download={down}; total={limit}; expire={expire}"),
            ),
            (
                header::HeaderName::from_static("profile-update-interval"),
                UPDATE_INTERVAL_H.to_string(),
            ),
            (
                header::HeaderName::from_static("support-url"),
                cfg.support_url.clone(),
            ),
        ],
        rendered.body,
    )
        .into_response()
}

fn profile_title(cfg: &Config, user: &User, cut_off: bool) -> String {
    let warn = if cut_off { "\u{26a0}\u{fe0f} " } else { "" };
    match &cfg.title {
        Some(t) => format!("{warn}{}", t.replace("{user}", &user.name)),
        None => format!("{warn}{} \u{2014} {}", cfg.sub_domain, user.name),
    }
}

async fn split_traffic(app: &App, user: &str) -> (u64, u64) {
    app.ctl
        .nodes
        .read()
        .await
        .values()
        .filter_map(|usage| usage.users.get(user))
        .fold((0, 0), |(up, down), t| (up + t.up, down + t.down))
}

async fn dashboard(
    app: &App,
    user: &crate::config::User,
    sub_url: &str,
    endpoints: &[subgen::Endpoint],
    cut_off: bool,
) -> Response {
    let cfg = &app.ctl.cfg;
    let qr = QRBuilder::new(sub_url)
        .ecl(ECL::M)
        .build()
        .map(|code| {
            SvgBuilder::default()
                .module_color("#d8d8d8")
                .background_color("#00000000")
                .to_str(&code)
        })
        .unwrap_or_default();

    let links: Vec<_> = endpoints
        .iter()
        .map(|ep| {
            json!({
                "uri": subgen::uri(ep),
                "label": ep.label(),
                "flag": ep.flag,
                "node": ep.node,
            })
        })
        .collect();

    let used = app.ctl.used_by(&user.name).await;
    let limit = cfg.limit_of(user);
    let page = app
        .env
        .get_template("index.html")
        .unwrap()
        .render(minijinja::context! {
            username => user.name,
            group => user.group,
            expires => user.expires.map(|d| d.format("%d.%m.%Y").to_string()),
            sub_url => sub_url,
            links => links,
            blocked => cut_off,
            qr => qr,
            support_url => cfg.support_url,
            used => crate::cli::human(used),
            limit => limit.map(crate::cli::human),
            percent => limit.map(|l| ((used as f64 / l as f64) * 100.0).min(100.0).round() as u64),
            next_reset => cfg.quota.render_local(cfg.quota.next_reset(chrono::Utc::now())),
        });
    match page {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            eprintln!("render dashboard: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn resolver(app: Arc<App>) {
    let dynamic: Vec<(String, String)> = app
        .ctl
        .cfg
        .nodes
        .iter()
        .filter(|n| n.addr.is_none())
        .map(|n| (n.name.clone(), n.fqdn.clone()))
        .collect();
    if dynamic.is_empty() {
        return;
    }
    let mut resolved_all = false;
    loop {
        let mut ok = true;
        for (name, fqdn) in &dynamic {
            match remote::doh_resolve(&app.ctl.client, fqdn).await {
                Ok(ip) => {
                    let mut addrs = app.addrs.write().await;
                    if addrs.get(name).map(String::as_str) != Some(ip.as_str()) {
                        eprintln!("resolved {fqdn} -> {ip}");
                        addrs.insert(name.clone(), ip);
                    }
                }
                Err(e) => {
                    ok = false;
                    eprintln!("resolve {fqdn}: {e:#}");
                }
            }
        }
        resolved_all = resolved_all || ok;
        tokio::time::sleep(if ok { RESOLVE_REFRESH } else { RESOLVE_RETRY }).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(title: Option<&str>) -> Config {
        let mut c = crate::subgen::tests::sample_config();
        c.title = title.map(str::to_string);
        c
    }

    #[test]
    fn title_substitutes_the_username() {
        let c = cfg(Some("relay for {user}"));
        assert_eq!(profile_title(&c, &c.users[0], false), "relay for me");
    }

    #[test]
    fn title_falls_back_to_the_subscription_domain() {
        let c = cfg(None);
        assert_eq!(
            profile_title(&c, &c.users[0], false),
            "sub.example.test \u{2014} me"
        );
    }

    #[test]
    fn a_cut_off_user_is_marked_either_way() {
        for t in [Some("relay for {user}"), None] {
            let c = cfg(t);
            assert!(profile_title(&c, &c.users[0], true).starts_with("\u{26a0}\u{fe0f} "));
        }
    }
}

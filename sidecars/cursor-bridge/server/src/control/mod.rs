//! Exposes the local control API.
mod calls;
mod harness;
mod models;
mod service;
mod settings;

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, header::CONTENT_TYPE, HeaderValue, Method, Response, StatusCode},
    middleware::{self, Next},
    routing::{any, get, post, put},
    Router,
};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    services::ServeDir,
};
use url::{Host, Url};

pub use service::{
    CallDetail, CallSummary, ControlService, DiscoveredModels, ModelConnectivityResult,
    ModelDiscoveryInput, ObservabilitySettings,
};

pub fn web_router(
    service: ControlService,
    assets: impl AsRef<std::path::Path>,
    control_token: Option<String>,
) -> Router {
    Router::new()
        .nest_service(
            "/__byok-api__",
            ServeDir::new(assets).append_index_html_on_directories(true),
        )
        .merge(api_router(service, control_token))
}

pub fn proxy_web_router(
    service: ControlService,
    target: Url,
    control_token: Option<String>,
) -> Router {
    frontend_proxy_router(target).merge(api_router(service, control_token))
}

fn frontend_proxy_router(target: Url) -> Router {
    let state = FrontendProxy {
        client: reqwest::Client::new(),
        target: target.as_str().trim_end_matches('/').to_string(),
    };
    Router::new()
        .route("/__byok-api__/", any(proxy_frontend))
        .route("/__byok-api__/{*path}", any(proxy_frontend))
        .with_state(state)
}

#[derive(Clone)]
struct FrontendProxy {
    client: reqwest::Client,
    target: String,
}

async fn proxy_frontend(
    State(proxy): State<FrontendProxy>,
    request: Request<Body>,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/__byok-api__/");
    let mut upstream = proxy
        .client
        .request(parts.method, format!("{}{path}", proxy.target));
    for (name, value) in &parts.headers {
        if name != header::HOST && name != header::CONNECTION {
            upstream = upstream.header(name, value);
        }
    }
    let body = match to_bytes(body, 64 * 1024 * 1024).await {
        Ok(body) => body,
        Err(error) => return proxy_error(error),
    };
    let upstream = match upstream.body(body).send().await {
        Ok(response) => response,
        Err(error) => return proxy_error(error),
    };
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let body = match upstream.bytes().await {
        Ok(body) => body,
        Err(error) => return proxy_error(error),
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    for (name, value) in &headers {
        if name != header::CONNECTION
            && name != header::TRANSFER_ENCODING
            && name != header::CONTENT_LENGTH
        {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

fn proxy_error(error: impl std::fmt::Display) -> Response<Body> {
    tracing::warn!(%error, "frontend development proxy failed");
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .body(Body::from("frontend development server is unavailable"))
        .expect("static proxy error response")
}

pub fn api_router(service: ControlService, control_token: Option<String>) -> Router {
    Router::new()
        .route(
            "/__byok-api__/api/models",
            get(models::list).post(models::create),
        )
        .route("/__byok-api__/api/models/discover", post(models::discover))
        .route("/__byok-api__/api/models/reconcile", put(models::reconcile))
        .route("/__byok-api__/api/models/order", put(models::reorder))
        .route(
            "/__byok-api__/api/models/{model_hash}",
            put(models::update).delete(models::remove),
        )
        .route(
            "/__byok-api__/api/models/{model_hash}/test/{test_id}",
            post(models::test).delete(models::cancel),
        )
        .route("/__byok-api__/api/llm-calls", get(calls::list))
        .route("/__byok-api__/api/llm-calls/{call_id}", get(calls::detail))
        .route(
            "/__byok-api__/api/settings/observability",
            get(settings::get).put(settings::update),
        )
        .route(
            "/__byok-api__/api/settings/ports",
            get(settings::get_ports).put(settings::update_ports),
        )
        .route(
            "/__byok-api__/api/settings/storage/statistics",
            get(settings::get_storage).delete(settings::clear_storage),
        )
        .route(
            "/__byok-api__/api/settings/proxy",
            get(settings::get_proxy).put(settings::update_proxy),
        )
        .route(
            "/__byok-api__/api/settings/desktop",
            get(settings::get_desktop).put(settings::update_desktop),
        )
        .route(
            "/__byok-api__/api/settings/commit",
            get(settings::get_commit).put(settings::update_commit),
        )
        .route(
            "/__byok-api__/api/harness/cursor/status",
            get(harness::status),
        )
        .route(
            "/__byok-api__/api/harness/cursor/ca/initialize",
            post(harness::initialize_ca),
        )
        .route(
            "/__byok-api__/api/harness/cursor/enabled",
            put(harness::set_enabled),
        )
        .route(
            "/__byok-api__/api/harness/cursor/injection",
            axum::routing::delete(harness::clear_injection),
        )
        .with_state(service)
        .layer(middleware::from_fn_with_state(
            ControlAuth {
                token: control_token,
            },
            require_control_token,
        ))
        .layer(desktop_cors())
}

/// The shared secret the control API demands, or `None` when unconfigured.
#[derive(Clone)]
struct ControlAuth {
    token: Option<String>,
}

/// Rejects any control-API request that does not carry the configured token.
///
/// This endpoint set shares a port with the Cursor protocol and can read model
/// credentials, rewrite the model bindings, and toggle the system-wide MITM
/// injection. Loopback binding keeps it off the network, and CORS keeps
/// browsers out, but neither stops a local process — which is what the token is
/// for.
///
/// **An unconfigured token refuses everything.** Failing open would mean any
/// machine where CodeRelay forgot to pass the variable (or a user launched the
/// bridge by hand) silently reverts to an unauthenticated control plane, which
/// is the vulnerability this closes. Failing closed is visible and safe.
async fn require_control_token(
    State(auth): State<ControlAuth>,
    request: Request,
    next: Next,
) -> Response<Body> {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if authorized(auth.token.as_deref(), presented) {
        return next.run(request).await;
    }
    if auth.token.is_none() {
        tracing::warn!("rejecting control-API request: no {CONTROL_TOKEN_ENV_NAME} configured");
        unauthorized("control API is disabled: no control token was configured for this process")
    } else {
        tracing::warn!("rejecting control-API request: control token mismatch");
        unauthorized("invalid control token")
    }
}

/// The whole authorization decision, in one place.
///
/// Separated from the middleware so it can be exercised directly: axum's `Next`
/// has no public constructor, so a test that went through `require_control_token`
/// could not observe the "allowed" branch at all.
fn authorized(token: Option<&str>, authorization: Option<&str>) -> bool {
    // `None` means the token was never configured. Refusing is the point: an
    // unconfigured bridge must not expose an unauthenticated control plane.
    let Some(expected) = token else {
        return false;
    };
    // An empty expected token is refused here rather than only in the config
    // parser. If the parser ever regressed, `""` would otherwise match a request
    // that simply omitted the header (`presented` defaults to `""`), i.e. the
    // control plane would accept any caller at all. The decision function has to
    // be safe on its own, not on the strength of its caller.
    if expected.is_empty() {
        return false;
    }
    let presented = authorization
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    constant_time_eq(presented.as_bytes(), expected.as_bytes())
}

/// Compares two secrets without leaking their common prefix length through
/// timing. Comparing with `==` would let a local attacker recover the token one
/// byte at a time.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn unauthorized(message: &str) -> Response<Body> {
    let status = StatusCode::UNAUTHORIZED;
    body_response(
        status,
        serde_json::json!({ "code": "unauthenticated", "message": message }),
    )
}

fn body_response(status: StatusCode, body: serde_json::Value) -> Response<Body> {
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// Name of the environment variable holding the control token, for diagnostics.
const CONTROL_TOKEN_ENV_NAME: &str = "CODERELAY_CURSOR_CONTROL_TOKEN";

fn desktop_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| local_origin(origin)))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([
            CONTENT_TYPE,
            header::ACCEPT_LANGUAGE,
            // Required for the control-token header; without it the browser's
            // preflight would reject every authenticated cross-origin call.
            header::AUTHORIZATION,
            header::HeaderName::from_static("disable-ad-ids"),
        ])
}

/// Whether a browser `Origin` is allowed to read a control-API response.
///
/// Only the Tauri webview and loopback development servers qualify. Anything
/// wider is a real hole rather than a convenience: write methods do not need to
/// read the response to take effect, so an allowed origin can toggle the MITM
/// injection even though it cannot see the reply. Private and link-local ranges
/// were previously allowed here and are deliberately gone — isolation belongs to
/// the loopback bind and the control token, not to a CORS allowlist.
fn local_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    if origin.eq_ignore_ascii_case("tauri://localhost") {
        return true;
    }
    let Ok(origin) = Url::parse(origin) else {
        return false;
    };
    if !matches!(origin.scheme(), "http" | "https")
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return false;
    }
    match origin.host() {
        Some(Host::Domain(host)) => {
            host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("tauri.localhost")
        }
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(value: &str) -> HeaderValue {
        HeaderValue::from_str(value).expect("valid header value")
    }

    #[test]
    fn control_token_comparison_rejects_every_mismatch() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        // A prefix must not compare equal, and a length difference must not panic.
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn only_tauri_and_loopback_origins_are_allowed() {
        assert!(local_origin(&origin("tauri://localhost")));
        assert!(local_origin(&origin("http://localhost:5173")));
        assert!(local_origin(&origin("http://127.0.0.1:5173")));
        assert!(local_origin(&origin("http://[::1]:5173")));

        // The private and link-local ranges used to be allowed. They are the
        // DNS-rebinding surface: a page on the user's LAN could reach the
        // control API, and write methods do not need to read the response.
        assert!(!local_origin(&origin("http://192.168.1.10")));
        assert!(!local_origin(&origin("http://10.0.0.5")));
        assert!(!local_origin(&origin("http://172.16.0.9")));
        assert!(!local_origin(&origin("http://169.254.1.1")));
        assert!(!local_origin(&origin("http://[fd00::1]")));
        // Not a plausible webview origin.
        assert!(!local_origin(&origin("https://evil.example")));
        assert!(!local_origin(&origin("http://localhost.evil.example")));
        assert!(!local_origin(&origin("http://tauri.localhost.evil.example")));
        // Credentials and non-root paths were never legitimate origins.
        assert!(!local_origin(&origin("http://user:pass@localhost/")));
        assert!(!local_origin(&origin("http://localhost/path")));
        assert!(!local_origin(&origin("http://localhost/?q=1")));
        assert!(!local_origin(&origin("http://localhost/#f")));
    }

    /// The decision the middleware makes. Kept out of the copy-paste trap: this
    /// calls the module-level `authorized` rather than a test-local reimplementation,
    /// so the tests fail if the real decision logic regresses.
    #[test]
    fn an_unconfigured_token_refuses_instead_of_failing_open() {
        // Failing open here would mean any machine where the variable was not
        // passed silently reverts to an unauthenticated control plane.
        assert!(!authorized(None, Some("Bearer anything")));
        assert!(!authorized(None, None));
        // An empty configured token must not degenerate into "no token needed".
        assert!(!authorized(Some(""), None));
        assert!(!authorized(Some(""), Some("Bearer ")));
    }

    #[test]
    fn the_token_must_be_presented_as_a_bearer_credential() {
        let token = Some("s3cret");
        assert!(authorized(token, Some("Bearer s3cret")));
        assert!(!authorized(token, Some("s3cret")));
        assert!(!authorized(token, Some("Bearer s3cre")));
        assert!(!authorized(token, Some("Bearer s3cret ")));
        assert!(!authorized(token, Some("Basic s3cret")));
        assert!(!authorized(token, None));
    }
}

mod config;
mod map;
mod valetudo;
mod web;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{
    sse::{Event, KeepAlive, Sse},
    Html, IntoResponse, Response,
};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::config::Config;
use crate::valetudo::Valetudo;
use askama::Template as _;

#[derive(Clone)]
struct AppState {
    valetudo: Valetudo,
    /// Broadcasts state updates to connected browsers.
    updates: broadcast::Sender<serde_json::Value>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Used by the container HEALTHCHECK, which has no curl to hand. Runs before
    // the subscriber is initialised so it stays silent unless it fails.
    if std::env::args().any(|a| a == "--healthcheck") {
        std::process::exit(healthcheck().await);
    }

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,robovac=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = Config::from_env().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let valetudo = Valetudo::new(&cfg)?;
    let (updates, _) = broadcast::channel(32);

    tracing::info!(valetudo = %cfg.valetudo_url, "starting robovac");

    // Background poller. Valetudo exposes an SSE endpoint, but polling keeps
    // this working across firmware versions and doubles as the reconnect path.
    {
        let state = AppState {
            valetudo: valetudo.clone(),
            updates: updates.clone(),
        };
        tokio::spawn(poll_loop(state, cfg.poll_interval_ms));
    }

    // Resolve the bind address before building the router so a malformed
    // value fails at startup rather than on the first connection.
    let addr: SocketAddr = cfg
        .bind
        .parse()
        .map_err(|e| format!("BIND {:?} is not a valid socket address: {e}", cfg.bind))?;

    let app = Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/api/state", get(api_state))
        .route("/api/capabilities", get(api_capabilities))
        .route("/api/control/{action}", post(control))
        .route("/api/fan-speed", post(set_fan_speed))
        .route("/api/clean-segments", post(clean_segments))
        .route("/map.svg", get(map_svg))
        .route("/api/camera/stream", get(camera_stream))
        .route("/api/camera/properties", get(camera_properties))
        .route("/events", get(events))
        .route("/static/style.css", get(static_style))
        .route("/static/app.js", get(static_app))
        .layer(axum::middleware::from_fn_with_state(
            cfg.clone(),
            auth_layer,
        ))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(AppState { valetudo, updates });

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct IndexQuery {
    segments: Option<String>,
}

async fn index(State(state): State<AppState>, Query(q): Query<IndexQuery>) -> Response {
    let selected = web::parse_selected(q.segments.as_ref());

    match web::gather(&state.valetudo, selected).await {
        Ok((dashboard,)) => match dashboard.render() {
            Ok(html) => Html(html).into_response(),
            Err(err) => template_error(err),
        },
        Err(err) => {
            tracing::warn!(%err, "valetudo unreachable while rendering dashboard");
            let mut fallback = web::Dashboard {
                title: "Robovac",
                info: None,
                summary: Default::default(),
                consumables: vec![],
                segments: vec![],
                fan_presets: vec![],
                camera: None,
                connected: false,
                error: Some(err),
            };
            fallback.title = "Robovac \u{2014} offline";
            match fallback.render() {
                Ok(html) => Html(html).into_response(),
                Err(err) => template_error(err),
            }
        }
    }
}

/// A template failure is our bug, not the robot's, so surface it as a 500 with
/// the detail in the body rather than a blank page.
fn template_error(err: askama::Error) -> Response {
    tracing::error!(%err, "template render failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("template error: {err}"),
    )
        .into_response()
}

async fn healthz(State(state): State<AppState>) -> Response {
    match state.valetudo.info().await {
        Ok(info) => Json(serde_json::json!({
            "ok": true,
            "robot": info.model_name,
            "implementation": info.implementation,
        }))
        .into_response(),
        Err(err) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ok": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn api_state(State(state): State<AppState>) -> Response {
    match state.valetudo.state().await {
        Ok(s) => Json(s).into_response(),
        Err(err) => error_response(err),
    }
}

/// Raw capability list. The frontend uses it to hide controls the robot does
/// not actually implement, and it is the first place to look when a model
/// behaves differently than expected.
async fn api_capabilities(State(state): State<AppState>) -> Response {
    match state.valetudo.capabilities().await {
        Ok(caps) => Json(caps).into_response(),
        Err(err) => error_response(err),
    }
}

async fn map_svg(State(state): State<AppState>) -> Response {
    let svg = match state.valetudo.state().await {
        Ok(s) => match s.map {
            Some(raw) => match serde_json::from_value::<map::MapData>(raw) {
                Ok(m) => map::render_svg(&m),
                Err(err) => {
                    tracing::warn!(%err, "could not parse map payload");
                    String::from("<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
                }
            },
            None => String::from("<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
        },
        Err(err) => return error_response(err),
    };

    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        svg,
    )
        .into_response()
}

async fn control(
    State(state): State<AppState>,
    axum::extract::Path(action): axum::extract::Path<String>,
) -> Response {
    match state.valetudo.basic_control(&action).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct FanSpeedBody {
    name: String,
}

async fn set_fan_speed(State(state): State<AppState>, Json(body): Json<FanSpeedBody>) -> Response {
    match state.valetudo.set_fan_speed(&body.name).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct CleanSegmentsBody {
    #[serde(default)]
    segment_ids: Vec<String>,
    #[serde(default = "one")]
    iterations: u32,
}

fn one() -> u32 {
    1
}

async fn clean_segments(
    State(state): State<AppState>,
    Json(body): Json<CleanSegmentsBody>,
) -> Response {
    match state
        .valetudo
        .clean_segments(&body.segment_ids, body.iterations)
        .await
    {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(err) => error_response(err),
    }
}

async fn camera_properties(State(state): State<AppState>) -> Response {
    match state.valetudo.duststreaming_properties().await {
        Ok(props) => Json(props).into_response(),
        Err(err) => error_response(err),
    }
}

/// Proxy the robot's MPEG-TS stream so the browser only needs to reach this
/// service, not the vacuum directly.
async fn camera_stream(State(state): State<AppState>) -> Response {
    let resp = match state
        .valetudo
        .proxy_get("/capabilities/DuststreamingCapability/stream")
        .await
    {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return (status, Json(serde_json::json!({"error": detail}))).into_response();
    }

    let body = resp
        .bytes_stream()
        .map(|chunk| chunk.map_err(|e| std::io::Error::other(e.to_string())));

    let mut out = Response::new(axum::body::Body::from_stream(body));
    out.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/MP2T"));
    out
}

/// Probe our own `/healthz` and exit non-zero when it does not answer 200.
/// Kept dependency-free so the runtime image needs no HTTP client.
async fn healthcheck() -> i32 {
    let addr = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    // 0.0.0.0 is not connectable; probe loopback instead.
    let host = if addr.starts_with("0.0.0.0") || addr.starts_with("[::]") {
        addr.replace("0.0.0.0", "127.0.0.1")
    } else {
        addr
    };

    let probe = format!("http://{host}/healthz");
    match reqwest::get(&probe).await {
        Ok(res) if res.status().is_success() => 0,
        Ok(res) => {
            eprintln!("healthcheck: HTTP {}", res.status());
            1
        }
        Err(err) => {
            eprintln!("healthcheck: {err}");
            1
        }
    }
}

const STYLE_CSS: &str = include_str!("static/style.css");
const APP_JS: &str = include_str!("static/app.js");

async fn static_style() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
        .into_response()
}

async fn static_app() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
        .into_response()
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.updates.subscribe();

    let stream = async_stream::stream! {
        // Ask the browser to retry quickly rather than waiting on its default.
        yield Ok(Event::default().event("retry").data("2000"));
        loop {
            match rx.recv().await {
                Ok(value) => {
                    let data = value.to_string();
                    yield Ok(Event::default().event("state").data(data));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn error_response(err: valetudo::ApiError) -> Response {
    let status = match &err {
        valetudo::ApiError::Status { status, .. } => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        valetudo::ApiError::Transport(_) => StatusCode::BAD_GATEWAY,
        valetudo::ApiError::Decode(_) => StatusCode::BAD_GATEWAY,
    };
    (status, Json(serde_json::json!({"error": err.to_string()}))).into_response()
}

async fn poll_loop(state: AppState, interval_ms: u64) {
    let interval = Duration::from_millis(interval_ms.max(250));
    loop {
        match state.valetudo.state().await {
            Ok(s) => {
                let summary = valetudo::Summary::from_state(&s);
                let payload = serde_json::json!({
                    "summary": summary,
                    "has_map": s.map.is_some(),
                });
                // No receivers is not an error; the dashboard polls on load.
                let _ = state.updates.send(payload);
            }
            Err(err) => {
                tracing::debug!(%err, "poll failed");
                let _ = state
                    .updates
                    .send(serde_json::json!({"summary": null, "error": err.to_string()}));
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// Optional basic auth in front of this service, in addition to whatever
/// Valetudo requires. Always challenges, so browsers cache the credential.
async fn auth_layer(
    axum::extract::State(cfg): axum::extract::State<Config>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !cfg.web_auth_enabled() {
        return next.run(req).await;
    }

    let header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    if cfg.check_web_auth(header) {
        return next.run(req).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            "Basic realm=\"robovac\", charset=\"UTF-8\"",
        )],
    )
        .into_response()
}

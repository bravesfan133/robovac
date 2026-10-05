mod bytecache;
mod cache;
mod config;
mod map;
mod sse;
mod upstream;
mod valetudo;
mod web;
mod zone;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
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

use crate::bytecache::ByteCache;
use crate::cache::{broadcast_payload, RobotCache};
use crate::config::Config;
use crate::valetudo::Valetudo;
use askama::Template as _;

#[derive(Clone)]
struct AppState {
    valetudo: Valetudo,
    /// Single source of truth for everything the UI renders. Only the poller
    /// writes to it; handlers only read.
    cache: Arc<RobotCache>,
    /// Broadcasts state updates to connected browsers.
    updates: broadcast::Sender<serde_json::Value>,
    /// Obstacle photos, bounded and short-lived so re-viewing one does not
    /// hammer Valetudo's rate limiter.
    obstacle_cache: Arc<ByteCache>,
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
    let cache = Arc::new(RobotCache::new());
    let (updates, _) = broadcast::channel(64);

    tracing::info!(valetudo = %cfg.valetudo_url, "starting robovac");

    // Background poller. Valetudo exposes an SSE endpoint, but polling keeps
    // this working across firmware versions and doubles as the reconnect path.
    {
        let state = AppState {
            valetudo: valetudo.clone(),
            cache: cache.clone(),
            updates: updates.clone(),
            obstacle_cache: Arc::new(ByteCache::new(1024, Duration::from_secs(60))),
        };
        tokio::spawn(poll_loop(state.clone(), cfg.poll_interval_ms));

        // One upstream subscription for the whole process, fanned out to
        // browsers. Valetudo allows only a handful of these.
        tokio::spawn(upstream::run(
            state.valetudo.clone(),
            state.cache.clone(),
            state.updates.clone(),
        ));
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
        .route("/readyz", get(readyz))
        .route("/api/state", get(api_state))
        .route("/api/capabilities", get(api_capabilities))
        .route("/api/control/{action}", post(control))
        .route("/api/fan-speed", post(set_fan_speed))
        .route("/api/clean-segments", post(clean_segments))
        .route("/api/clean-zones", post(clean_zones))
        .route("/api/obstacles", get(api_obstacles))
        .route("/api/obstacles/enabled", post(set_obstacle_images))
        .route("/api/obstacles/image", get(obstacle_image))
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
        .with_state(AppState {
            valetudo,
            cache,
            updates,
            // 16 MB is plenty for a few dozen camera frames and keeps the
            // resident footprint trivial next to the robot itself.
            obstacle_cache: Arc::new(ByteCache::new(16 * 1024 * 1024, Duration::from_secs(300))),
        });

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
    let snap = state.cache.snapshot();

    if !snap.warm && snap.last_error.is_none() {
        // Nothing has polled yet. Say so rather than claiming the robot is
        // offline, which is a different and more alarming statement.
        return Html(web::Dashboard::warming().render().unwrap_or_default()).into_response();
    }

    let html = web::Dashboard::from_snapshot(&snap, selected).render();
    Html(rendering(html)).into_response()
}

/// Render a template result, turning a failure into a visible 500 instead of a
/// blank page.
fn rendering(html: Result<String, askama::Error>) -> String {
    match html {
        Ok(html) => html,
        Err(err) => {
            tracing::error!(%err, "template render failed");
            format!(
                "<!doctype html><meta charset=utf-8><title>Robovac</title>\
<body style=\"font:16px sans-serif;background:#14161a;color:#e6e8ec;padding:2rem\">\
<h1>Template error</h1><pre style=\"white-space:pre-wrap\">{err}</pre></body>"
            )
        }
    }
}

/// Liveness. Answers 200 as long as this process is serving, *regardless* of
/// whether the vacuum is reachable.
///
/// The container HEALTHCHECK points here, and a healthcheck that depends on an
/// external service is wrong: the robot is off the network or still unrooted for
/// long stretches, and reporting "unhealthy" then invites the orchestrator to
/// restart something that is working perfectly well. Restarting would not fix
/// it, because the cause is on the other end of the network.
async fn healthz() -> Response {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

async fn readyz(State(state): State<AppState>) -> Response {
    let snap = state.cache.snapshot();

    if snap.warm {
        let robot = snap
            .info
            .as_ref()
            .map(|i| {
                serde_json::json!({
                    "model": i.model_name,
                    "implementation": i.implementation,
                    "manufacturer": i.manufacturer,
                })
            })
            .unwrap_or(serde_json::Value::Null);
        return Json(serde_json::json!({
            "ok": true,
            "robot": robot,
            "seconds_since_contact": snap.last_ok.map(|i| i.elapsed().as_secs()),
        }))
        .into_response();
    }

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "ok": false,
            "error": snap.last_error.unwrap_or_else(|| "no successful poll yet".into()),
            "ever_connected": snap.ever_ok,
        })),
    )
        .into_response()
}

/// State for the frontend, served from cache. This route must never trigger a
/// robot poll, or every browser refresh becomes a ~1s miio round trip.
async fn api_state(State(state): State<AppState>) -> Response {
    let snap = state.cache.snapshot();

    if !snap.warm {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "summary": null,
                "warming_up": true,
                "error": snap.last_error,
            })),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "summary": snap.summary,
        "map_version": snap.map_version,
        "segments": snap.segments,
        "consumables": snap.consumables,
        "fan_presets": snap.fan_presets,
        "camera": snap.camera,
        "seconds_since_contact": snap.last_ok.map(|i| i.elapsed().as_secs()),
        "last_error": snap.last_error,
    }))
    .into_response()
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

async fn map_svg(State(state): State<AppState>, Query(q): Query<IndexQuery>) -> Response {
    // Selection is per-request, so the render is not cached across users.
    let selected = web::parse_selected(q.segments.as_ref());

    let Some(svg) = state.cache.render_map(&selected) else {
        // No map yet: the robot has not completed its first mapping run.
        return (
            [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\" \
             class=\"vacuum-map\"><rect width=\"100\" height=\"100\" fill=\"#101216\"/>\
             <text x=\"50\" y=\"52\" fill=\"#7a808c\" font-size=\"7\" text-anchor=\"middle\">\
             map unavailable</text></svg>",
        )
            .into_response();
    };

    (
        [
            (header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            // The map changes only when the geometry does, and the cache already
            // knows when that is. Without this a browser revalidates a document
            // it has just fetched.
            (
                header::CACHE_CONTROL,
                if selected.is_empty() {
                    "public, max-age=2"
                } else {
                    "no-store"
                },
            ),
        ],
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
    Json(state.cache.snapshot().camera).into_response()
}

/// Proxy the robot's MPEG-TS stream so the browser only needs to reach this
/// service, not the vacuum directly.
async fn clean_zones(
    State(state): State<AppState>,
    Json(body): Json<crate::zone::ZoneRequest>,
) -> Response {
    let Some((_min_x, _min_y, pixel_size, max_x)) = state.cache.map_extent() else {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "no map yet, so zones cannot be placed"
            })),
        )
            .into_response();
    };

    // Drawn pixels are relative to the map's own origin, so only the mapped
    // width is needed here; `pixel_size` converts them to map units.
    let extent = (0.0, 0.0, pixel_size, max_x);
    let zones = match body.to_zones(pixel_size, Some(extent)) {
        Ok(zones) => zones,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err})),
            )
                .into_response()
        }
    };

    let payload = crate::zone::ZoneCleanBody::new(&zones);
    match state.valetudo.clean_zones(&payload).await {
        Ok(()) => Json(serde_json::json!({"ok": true, "zones": zones.len()})).into_response(),
        Err(err) => error_response(err),
    }
}

async fn api_obstacles(State(state): State<AppState>) -> Response {
    let snap = state.cache.snapshot();
    Json(serde_json::json!({
        "obstacles": snap.obstacles,
        "count": snap.obstacles.len(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct ObstacleToggle {
    enabled: bool,
}

/// Turn the robot's obstacle capture on or off. This is a firmware setting that
/// persists across reboots, so it is not something to flip silently.
async fn set_obstacle_images(
    State(state): State<AppState>,
    Json(body): Json<ObstacleToggle>,
) -> Response {
    match state
        .valetudo
        .set_obstacle_images_enabled(body.enabled)
        .await
    {
        Ok(()) => Json(serde_json::json!({"ok": true, "enabled": body.enabled})).into_response(),
        Err(err) => error_response(err),
    }
}

/// Proxy one obstacle photo, caching the bytes.
///
/// Loaded on demand rather than eagerly: Valetudo rate-limits this endpoint to
/// three requests a second, and a page that fetched every obstacle at once would
/// throttle itself.
async fn obstacle_image(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ObstacleImageQuery>,
) -> Response {
    let id = q.id.unwrap_or_default();
    if let Err(err) = validate_obstacle_id(&id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err})),
        )
            .into_response();
    }

    if let Some((bytes, content_type)) = state.obstacle_cache.get(&id) {
        return (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(&content_type)
                        .unwrap_or(HeaderValue::from_static("image/jpeg")),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, max-age=120"),
                ),
            ],
            bytes,
        )
            .into_response();
    }

    let resp = match state.valetudo.obstacle_image(&id).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();

    let bytes = match resp.bytes().await {
        Ok(b) => b.to_vec(),
        Err(err) => return error_response(valetudo::ApiError::Transport(err.to_string())),
    };

    state.obstacle_cache.put(&id, bytes.clone(), &content_type);

    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_str(&content_type)
                    .unwrap_or(HeaderValue::from_static("image/jpeg")),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=120"),
            ),
        ],
        bytes,
    )
        .into_response()
}

#[derive(Deserialize)]
struct ObstacleImageQuery {
    id: Option<String>,
}

/// Obstacle ids come from the robot, so they are treated as untrusted.
///
/// Path traversal is handled by percent-encoding before the request leaves, but
/// the id is also used as a cache key and is worth bounding regardless. Real ids
/// are hex or decimal digits, sometimes with separators, so those are allowed.
fn validate_obstacle_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("no obstacle id given".into());
    }
    if id.len() > 128 {
        return Err("obstacle id is too long".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("obstacle id contains unexpected characters".into());
    }
    Ok(())
}


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

/// Refresh the slowly-changing capabilities every N state polls.
const META_EVERY_N_TICKS: u32 = 15;

async fn poll_loop(state: AppState, interval_ms: u64) {
    let interval = Duration::from_millis(interval_ms.max(250));
    let mut consecutive_failures: u32 = 0;
    // Consumables, segments and presets change rarely; polling them on every
    // state tick would triple the robot traffic for no benefit. The first tick
    // does fetch them, otherwise a freshly started dashboard would sit empty
    // for a full interval, which reads as "no rooms found".
    let mut ticks: u32 = 0;

    loop {
        match state.valetudo.state().await {
            Ok(robot_state) => {
                if consecutive_failures > 0 {
                    tracing::info!(failures = consecutive_failures, "reconnected to the vacuum");
                    consecutive_failures = 0;
                }

                // The cache keeps the map as geometry and re-renders per request,
                // since the rendered form depends on the browser's selection.
                state.cache.update_state(&robot_state);
                state.cache.record_success();

                ticks = ticks.wrapping_add(1);
                if ticks % META_EVERY_N_TICKS == 1 {
                    refresh_meta(&state).await;
                }
            }
            Err(err) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                if consecutive_failures == 1 {
                    tracing::debug!(error = %err, "poll failed");
                } else if consecutive_failures == 30 {
                    tracing::warn!(error = %err, "still cannot reach the vacuum after 30 attempts");
                } else if consecutive_failures % 300 == 0 {
                    tracing::warn!(failures = consecutive_failures, error = %err, "still cannot reach the vacuum");
                }
                state.cache.record_failure(err.to_string());
            }
        }

        let _ = state.updates.send(broadcast_payload(&state.cache));
        tokio::time::sleep(interval).await;
    }
}

/// Refresh the slowly-changing parts. Failures here are tolerated: the robot
/// being able to answer these at all is not guaranteed on every model, and a
/// missing section should not blank the dashboard.
async fn refresh_meta(state: &AppState) {
    let (consumables, segments, presets, camera) = futures_util::join!(
        state.valetudo.consumables(),
        state.valetudo.segments(),
        state.valetudo.fan_speed_presets(),
        state.valetudo.duststreaming_properties(),
    );

    // `as_ref().err()` avoids consuming the Result before it is used below.
    for (name, result) in [
        ("consumables", consumables.as_ref().err()),
        ("segments", segments.as_ref().err()),
        ("fan presets", presets.as_ref().err()),
        ("camera", camera.as_ref().err()),
    ] {
        if let Some(err) = result {
            tracing::debug!(capability = name, error = %err, "capability unavailable");
        }
    }

    if let Ok(info) = state.valetudo.info().await {
        state.cache.update_info(info);
    }

    state.cache.update_meta(
        consumables.unwrap_or_default(),
        segments.unwrap_or_default(),
        presets.unwrap_or_default(),
        camera.ok().flatten(),
    );
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

#[cfg(test)]
mod obstacle_id_tests {
    use super::validate_obstacle_id;

    #[test]
    fn accepts_realistic_ids() {
        for id in ["obj-a1", "1647867893", "AABBCCDD", "img_1", "a.b"] {
            assert!(validate_obstacle_id(id).is_ok(), "{id} should be valid");
        }
    }

    #[test]
    fn rejects_traversal_and_junk() {
        for id in [
            "",
            "../etc/passwd",
            "a/b",
            "a b",
            "a%2Fb",
            "a?b=1",
            "a#b",
            "a\\b",
        ] {
            assert!(
                validate_obstacle_id(id).is_err(),
                "{id:?} should be rejected"
            );
        }
        assert!(validate_obstacle_id(&"x".repeat(129)).is_err());
    }
}

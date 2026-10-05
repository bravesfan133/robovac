//! HTTP-level tests, with an in-process fake Valetudo.
//!
//! These exercise the real router: routing, extraction, template rendering, the
//! auth layer and status codes. Previously every handler was only covered by
//! manual probing, and `cargo test` needed no network because the Node fake
//! could not be reached from Rust.
//!
//! The fake is intentionally minimal — it returns exactly the shapes the
//! handlers parse, and nothing else.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;

use crate::cache::RobotCache;
use crate::config::Config;

/// Map and summary as Valetudo would send them.
const STATE: &str = r#"{
  "metaData": {"version": 2},
  "attributes": [
    {"__class":"StatusStateAttribute","metaData":{},"value":"docked","flag":"none"},
    {"__class":"BatteryStateAttribute","metaData":{},"level":91.0,"flag":"charging"},
    {"__class":"DockStatusStateAttribute","metaData":{},"value":"idle"},
    {"__class":"PresetSelectionStateAttribute","metaData":{},"type":"fan_speed","value":"turbo"}
  ],
  "map": {
    "metaData": {"version": 2},
    "size": {"x": 300, "y": 200},
    "pixelSize": 5,
    "layers": [
      {"type":"floor","pixels":[0,0, 1,0, 2,0],"metaData":{}},
      {"type":"segment","compressedPixels":[0,0,2],"metaData":{"segmentId":"1","name":"Kitchen"}},
      {"type":"wall","pixels":[3,0],"metaData":{}}
    ],
    "entities": [
      {"type":"robot_position","points":[1,0],"metaData":{"angle":90}},
      {"type":"obstacle","points":[2,0],"metaData":{"id":"ob1","angle":45}}
    ]
  }
}"#;

fn robot_json() -> String {
    r#"{"manufacturer":"Dreame","modelName":"L40 Ultra","implementation":"DreameL40UltraValetudoRobot"}"#.to_string()
}

fn segments_json() -> String {
    r#"[{"id":"1","name":"Kitchen"},{"id":"2","name":null}]"#.to_string()
}

/// Build the app the same way `main` does, but wired to a cache that is already
/// warm. The poller is not started, so tests are deterministic.
fn warm_app() -> Router {
    let cache = Arc::new(RobotCache::new());
    cache.record_success();
    cache.update_info(serde_json::from_str(&robot_json()).expect("robot json"));
    cache.update_meta(
        vec![serde_json::from_str(
            r#"{"type":"filter","subType":"none","remaining":{"value":80,"unit":"percent"}}"#,
        )
        .expect("consumable")],
        serde_json::from_str(&segments_json()).expect("segments"),
        vec!["quiet".into(), "turbo".into()],
        None,
    );
    cache.update_state(&serde_json::from_str(STATE).expect("state"));

    let cfg = test_config();
    let valetudo = crate::valetudo::Valetudo::new(&cfg).expect("client");
    let (updates, _) = tokio::sync::broadcast::channel(8);

    crate::build_router(
        valetudo,
        cache,
        updates,
        Arc::new(crate::bytecache::ByteCache::new(
            1024,
            std::time::Duration::from_secs(60),
        )),
        cfg,
    )
}

fn test_config() -> Config {
    // Unreachable on purpose: no test performs a real network call, since every
    // handler answers from the cache. If one ever does, it will fail loudly
    // rather than quietly reaching a real robot.
    Config {
        valetudo_url: "http://127.0.0.1:1".to_string(),
        valetudo_username: None,
        valetudo_password: None,
        bind: "127.0.0.1:0".to_string(),
        web_username: None,
        web_password: None,
        poll_interval_ms: 60_000,
        request_timeout_secs: 1,
    }
}

async fn get(app: &Router, uri: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn post(app: &Router, uri: &str, body: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::post(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn healthz_is_always_ok_regardless_of_the_robot() {
    let app = warm_app();
    let (status, body) = get(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"ok\":true"));
}

#[tokio::test]
async fn readyz_reports_the_robot() {
    let app = warm_app();
    let (status, body) = get(&app, "/readyz").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("L40 Ultra"), "got {body}");
}

#[tokio::test]
async fn readyz_is_503_before_the_first_poll() {
    // A cold cache must not claim to be ready: the distinction between "starting
    // up" and "the robot is gone" is what the diagnostics rely on.
    let cfg = test_config();
    let valetudo = crate::valetudo::Valetudo::new(&cfg).expect("client");
    let (updates, _) = tokio::sync::broadcast::channel(8);
    let app = crate::build_router(
        valetudo,
        Arc::new(RobotCache::new()),
        updates,
        Arc::new(crate::bytecache::ByteCache::new(
            1024,
            std::time::Duration::from_secs(60),
        )),
        cfg,
    );

    let (status, body) = get(&app, "/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("\"ever_connected\":false"), "got {body}");
}

#[tokio::test]
async fn dashboard_renders_from_cache_without_touching_the_network() {
    let app = warm_app();
    let (status, body) = get(&app, "/").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("L40 Ultra"));
    assert!(body.contains("Kitchen"), "segment name should render");
    assert!(body.contains("Filter"), "consumable should render");
    assert!(body.contains("91%"), "battery should render");
    // The unreachable VALETUDO_URL is proof nothing was fetched.
    assert!(body.contains("online"));
}

#[tokio::test]
async fn dashboard_preserves_selection_from_the_query_string() {
    let app = warm_app();
    let (_, body) = get(&app, "/?segments=1").await;
    assert!(
        body.contains("value=\"1\" checked"),
        "room 1 should be pre-selected"
    );

    let (_, body) = get(&app, "/?segments=bogus%20id").await;
    assert!(
        !body.contains("value=\"bogus id\" checked"),
        "an invalid id must not be honoured"
    );
}

#[tokio::test]
async fn map_is_served_as_svg_with_segment_metadata() {
    let app = warm_app();
    let response = app
        .clone()
        .oneshot(
            Request::get("/map.svg")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("image/svg+xml; charset=utf-8")
    );

    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let svg = String::from_utf8_lossy(&bytes);

    assert!(svg.contains("<svg"));
    assert!(
        svg.contains("data-pixel-size=\"5.0000\""),
        "scale must be real"
    );
    assert!(
        svg.contains("data-segment-id=\"1\""),
        "segments must be addressable"
    );
    assert!(svg.contains("data-clean=\"1\""), "labels must be clickable");
}

#[tokio::test]
async fn map_selection_is_rendered_into_the_document() {
    let app = warm_app();
    let (_, body) = get(&app, "/map.svg?segments=1").await;
    assert!(body.contains("data-segment-id=\"1\" data-selected=\"true\""));
}

#[tokio::test]
async fn map_without_a_map_says_so_rather_than_failing() {
    let cfg = test_config();
    let valetudo = crate::valetudo::Valetudo::new(&cfg).expect("client");
    let (updates, _) = tokio::sync::broadcast::channel(8);
    let app = crate::build_router(
        valetudo,
        Arc::new(RobotCache::new()),
        updates,
        Arc::new(crate::bytecache::ByteCache::new(
            1024,
            std::time::Duration::from_secs(60),
        )),
        cfg,
    );

    let (status, body) = get(&app, "/map.svg").await;
    assert_eq!(status, StatusCode::OK, "a missing map is not an error");
    assert!(body.contains("map unavailable"));
}

#[tokio::test]
async fn api_state_exposes_everything_the_frontend_needs() {
    let app = warm_app();
    let (status, body) = get(&app, "/api/state").await;
    assert_eq!(status, StatusCode::OK);

    let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(json["summary"]["status"], "docked");
    assert_eq!(json["segments"].as_array().expect("segments").len(), 2);
    assert!(json["map_version"].as_u64().expect("version") >= 1);
}

#[tokio::test]
async fn obstacles_endpoint_lists_what_the_robot_reported() {
    let app = warm_app();
    let (status, body) = get(&app, "/api/obstacles").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(json["count"], 1);
    assert_eq!(json["obstacles"][0]["id"], "ob1");
    assert_eq!(json["obstacles"][0]["angle"], 45.0);
}

#[tokio::test]
async fn obstacle_image_rejects_bad_ids_without_calling_the_robot() {
    let app = warm_app();
    for uri in [
        "/api/obstacles/image",
        "/api/obstacles/image?id=",
        "/api/obstacles/image?id=..%2Fetc",
        "/api/obstacles/image?id=a%20b",
    ] {
        let (status, _) = get(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} should be rejected");
    }
}

#[tokio::test]
async fn zones_are_rejected_when_there_is_no_map() {
    // The map is what supplies the pixel scale, so without it a zone cannot be
    // converted and the request must not be forwarded.
    let cfg = test_config();
    let valetudo = crate::valetudo::Valetudo::new(&cfg).expect("client");
    let (updates, _) = tokio::sync::broadcast::channel(8);
    let app = crate::build_router(
        valetudo,
        Arc::new(RobotCache::new()),
        updates,
        Arc::new(crate::bytecache::ByteCache::new(
            1024,
            std::time::Duration::from_secs(60),
        )),
        cfg,
    );

    let (status, body) = post(&app, "/api/clean-zones", r#"{"zones":[[1,1,5,5]]}"#).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("no map"), "got {body}");
}

#[tokio::test]
async fn too_many_zones_is_a_client_error() {
    let app = warm_app();
    let five = r#"{"zones":[[1,1,5,5],[1,1,5,5],[1,1,5,5],[1,1,5,5],[1,1,5,5]]}"#;
    let (status, body) = post(&app, "/api/clean-zones", five).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("at most 4"), "got {body}");
}

#[tokio::test]
async fn pwa_assets_are_served_with_the_right_types() {
    let app = warm_app();
    for (uri, expected) in [
        ("/static/manifest.webmanifest", "application/manifest+json"),
        (
            "/static/service-worker.js",
            "text/javascript; charset=utf-8",
        ),
        ("/static/app.css", "text/css; charset=utf-8"),
        ("/static/icons/icon-192.png", "image/png"),
        ("/static/icons/icon.svg", "image/svg+xml"),
    ] {
        let response = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some(expected),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn unknown_icons_are_not_served() {
    let app = warm_app();
    // A fixed allow-list, so this cannot be walked out of the icons directory.
    for uri in [
        "/static/icons/nope.png",
        "/static/icons/../main.rs",
        "/static/icons/..%2f..%2fCargo.toml",
    ] {
        let (status, _) = get(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn static_assets_are_cacheable_but_live_data_is_not() {
    let app = warm_app();

    let shell = app
        .clone()
        .oneshot(
            Request::get("/static/service-worker.js")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        shell
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-cache"),
        "the worker must be re-checked or updates never ship"
    );

    // A cached map would misreport where the robot is, so it is only briefly
    // cacheable, never stored, and never in a shared cache that sits behind the
    // service's own basic auth.
    let map = app
        .clone()
        .oneshot(
            Request::get("/map.svg")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let map_cache = map
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(map_cache, "private, max-age=2");

    // With a room selection the document is specific to one request, so it must
    // not be cached at all.
    let selected = app
        .oneshot(
            Request::get("/map.svg?segments=1")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        selected
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
}

/// The auth layer, exercised through the router rather than the helper.
fn authed_app(user: &str, pass: &str) -> Router {
    let cache = Arc::new(RobotCache::new());
    cache.record_success();
    let cfg = Config {
        web_username: Some(user.to_string()),
        web_password: Some(pass.to_string()),
        ..test_config()
    };
    let valetudo = crate::valetudo::Valetudo::new(&cfg).expect("client");
    let (updates, _) = tokio::sync::broadcast::channel(8);
    crate::build_router(
        valetudo,
        cache,
        updates,
        Arc::new(crate::bytecache::ByteCache::new(
            1024,
            std::time::Duration::from_secs(60),
        )),
        cfg,
    )
}

#[tokio::test]
async fn web_auth_challenges_then_accepts() {
    let app = authed_app("admin", "s3cret");

    let response = app
        .clone()
        .oneshot(
            Request::get("/healthz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // The challenge is a header. Without it a browser cannot prompt for
    // credentials and would just show an empty page.
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok()),
        Some("Basic realm=\"robovac\", charset=\"UTF-8\"")
    );

    let wrong = app
        .clone()
        .oneshot(
            Request::get("/healthz")
                .header("authorization", "Basic YWRtaW46bm90aXQ=")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let right = app
        .oneshot(
            Request::get("/healthz")
                .header("authorization", "Basic YWRtaW46czNjcmV0")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(right.status(), StatusCode::OK);
}

#[tokio::test]
async fn bind_address_is_a_valid_socket_address_in_the_test_config() {
    // Guards the default config, since a bad value is only caught at startup.
    let cfg = test_config();
    assert!(cfg.bind.parse::<SocketAddr>().is_ok());
}

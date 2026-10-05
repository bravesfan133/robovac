use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;

const BASE: &str = "/api/v2/robot";

/// Thin typed client over Valetudo's REST API v2.
///
/// Only the surface this UI needs is modelled; anything unknown is preserved as
/// raw JSON so the frontend can render capabilities we have not taught the
/// server about yet.
#[derive(Clone)]
pub struct Valetudo {
    http: reqwest::Client,
    /// A second client with no request timeout, used only for the long-lived
    /// event stream. Sharing the JSON client would sever a healthy stream every
    /// few seconds, because its timeout is sized for single request/response
    /// calls.
    stream: reqwest::Client,
    base_url: String,
    auth: Option<(String, String)>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RobotInfo {
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default, rename = "modelName")]
    pub model_name: String,
    #[serde(default)]
    pub implementation: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RobotState {
    #[serde(default)]
    pub attributes: Vec<Attribute>,
    #[serde(default)]
    pub map: Option<serde_json::Value>,
}

/// A single state attribute. Valetudo tags these with `__class` rather than
/// putting them in fixed fields, so they are matched on that tag.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "__class")]
pub enum Attribute {
    #[serde(rename = "StatusStateAttribute")]
    Status {
        value: String,
        #[serde(default)]
        flag: Option<String>,
    },
    #[serde(rename = "BatteryStateAttribute")]
    Battery {
        level: f64,
        #[serde(default)]
        flag: Option<String>,
    },
    #[serde(rename = "PresetSelectionStateAttribute")]
    Preset {
        #[serde(rename = "type")]
        preset_type: String,
        value: String,
    },
    #[serde(rename = "DockStatusStateAttribute")]
    DockStatus { value: String },
    #[serde(rename = "ErrorStateAttribute")]
    Error { error: String },
    #[serde(other)]
    Other,
}

/// Flattened view of the robot, assembled from the attribute list.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub status: Option<String>,
    pub status_flag: Option<String>,
    pub battery: Option<f64>,
    pub battery_flag: Option<String>,
    pub dock_status: Option<String>,
    pub fan_speed: Option<String>,
    pub water_grade: Option<String>,
    pub operation_mode: Option<String>,
    pub error: Option<String>,
}

impl Summary {
    pub fn from_state(state: &RobotState) -> Self {
        let mut out = Self::default();
        for attr in &state.attributes {
            match attr {
                Attribute::Status { value, flag } => {
                    out.status = Some(value.clone());
                    out.status_flag = flag.clone();
                }
                Attribute::Battery { level, flag } => {
                    out.battery = Some(*level);
                    out.battery_flag = flag.clone();
                }
                Attribute::Preset { preset_type, value } => match preset_type.as_str() {
                    "fan_speed" => out.fan_speed = Some(value.clone()),
                    "water_grade" => out.water_grade = Some(value.clone()),
                    "operation_mode" => out.operation_mode = Some(value.clone()),
                    _ => {}
                },
                Attribute::DockStatus { value } => out.dock_status = Some(value.clone()),
                Attribute::Error { error } => out.error = Some(error.clone()),
                Attribute::Other => {}
            }
        }
        out
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Consumable {
    #[serde(rename = "type")]
    pub consumable_type: String,
    #[serde(rename = "subType", default)]
    pub sub_type: String,
    pub remaining: Remaining,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Remaining {
    pub value: f64,
    pub unit: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MapSegment {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
struct BasicControlBody<'a> {
    action: &'a str,
}

#[derive(Debug, Serialize)]
struct FanSpeedBody<'a> {
    name: &'a str,
}

#[derive(Debug, Serialize)]
struct SegmentCleanBody {
    action: &'static str,
    segment_ids: Vec<String>,
    iterations: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The hostname could not be resolved. Usually a wrong name rather than a
    /// problem with the robot.
    UnresolvedHost,
    /// Nothing is listening on that address: robot powered off, not yet
    /// rooted, or on a different address.
    ConnectionRefused,
    /// The address is unroutable or the host did not answer.
    Unreachable,
    /// The request took too long, which on a busy LAN usually means the robot is
    /// mid-task rather than gone.
    Timeout,
    /// Credentials rejected. Almost always Valetudo's basic auth being enabled
    /// without `VALETUDO_USERNAME`/`VALETUDO_PASSWORD` being set here.
    Unauthorized,
    /// Authenticated but not allowed.
    Forbidden,
    /// The robot answered, with something other than success.
    Status { status: u16, detail: Option<String> },
    /// The robot answered, but not with anything we understand.
    Decode,
}

impl Failure {
    /// A short label for the status pill.
    pub fn summary(&self) -> &'static str {
        match self {
            Failure::UnresolvedHost => "name not resolving",
            Failure::ConnectionRefused => "nothing listening",
            Failure::Unreachable => "host unreachable",
            Failure::Timeout => "timed out",
            Failure::Unauthorized => "needs credentials",
            Failure::Forbidden => "refused access",
            Failure::Status { .. } => "unexpected reply",
            Failure::Decode => "unreadable reply",
        }
    }

    /// What the user can actually do about it.
    pub fn advice(&self) -> &'static str {
        match self {
            Failure::UnresolvedHost => "Check the name. Valetudo advertises itself as valetudo-<robot-id>.local; an IP also works.",
            Failure::ConnectionRefused => "Nothing is answering on that port. Is the robot powered on and rooted, and is this its current address?",
            Failure::Unreachable => "The address did not respond at all. Check the vacuum is on the same network.",
            Failure::Timeout => "No answer in time. The robot may be busy mid-cleanup; this usually resolves itself.",
            Failure::Unauthorized => "Set VALETUDO_USERNAME and VALETUDO_PASSWORD to match Valetudo's basic auth.",
            Failure::Forbidden => "Those credentials were rejected. Check the username and password.",
            Failure::Status { status, .. } => match status {
                404 => "That endpoint does not exist. The Valetudo version may be older than this UI expects.",
                429 => "Rate limited by Valetudo. Wait a moment and retry.",
                503 => "Valetudo reports it is not ready. Its camera streamer may be missing.",
                _ => "Valetudo answered with an error. Check its own logs on the robot.",
            },
            Failure::Decode => "The reply was not the expected format. A newer or older Valetudo may disagree with this UI.",
        }
    }

    pub fn from_status(status: u16, detail: Option<String>) -> Self {
        match status {
            401 => Failure::Unauthorized,
            403 => Failure::Forbidden,
            other => Failure::Status {
                status: other,
                detail,
            },
        }
    }
}

#[derive(Debug)]
pub enum ApiError {
    Transport(String),
    Status { status: u16, detail: Option<String> },
    Decode(String),
}

impl ApiError {
    /// The classified form, used for the diagnostics panel.
    pub fn failure(&self) -> Failure {
        match self {
            ApiError::Transport(msg) => {
                // The transport string is built from the same reqwest error, so
                // re-classify from the text when the cause is unavailable.
                if msg.contains("timed out") || msg.contains("timeout") {
                    Failure::Timeout
                } else if msg.contains("Connection refused") {
                    Failure::ConnectionRefused
                } else if msg.contains("Name or service not known")
                    || msg.contains("nodename nor servname")
                    || msg.contains("Temporary failure in name resolution")
                {
                    Failure::UnresolvedHost
                } else {
                    Failure::Unreachable
                }
            }
            ApiError::Status { status, detail } => Failure::from_status(*status, detail.clone()),
            ApiError::Decode(_) => Failure::Decode,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Transport(msg) => write!(f, "request to Valetudo failed: {msg}"),
            ApiError::Status { status, detail } => match detail {
                Some(d) if !d.is_empty() => write!(f, "Valetudo returned HTTP {status}: {d}"),
                _ => write!(f, "Valetudo returned HTTP {status}"),
            },
            ApiError::Decode(msg) => write!(f, "could not parse Valetudo response: {msg}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Transport(e.to_string())
    }
}

impl Valetudo {
    pub fn new(cfg: &Config) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.request_timeout_secs))
            .user_agent(concat!("robovac/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("could not build HTTP client: {e}"))?;

        let auth = match (&cfg.valetudo_username, &cfg.valetudo_password) {
            (Some(u), Some(p)) => Some((u.clone(), p.clone())),
            _ => None,
        };

        let stream = reqwest::Client::builder()
            // No overall timeout: an event stream is meant to stay open. A
            // connect timeout still guards against an unreachable host, and the
            // consumer detects a dead connection by the absence of keep-alives.
            .connect_timeout(Duration::from_secs(cfg.request_timeout_secs))
            .user_agent(concat!("robovac/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("could not build streaming HTTP client: {e}"))?;

        Ok(Self {
            http,
            stream,
            base_url: format!("{}{}", cfg.valetudo_url, BASE),
            auth,
        })
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self.http.get(format!("{}{}", self.base_url, path));
        match &self.auth {
            Some((u, p)) => req.basic_auth(u, Some(p)),
            None => req,
        }
    }

    fn request_put(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self.http.put(format!("{}{}", self.base_url, path));
        match &self.auth {
            Some((u, p)) => req.basic_auth(u, Some(p)),
            None => req,
        }
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, ApiError> {
        let status = resp.status();
        if !status.is_success() {
            // Valetudo reports errors as plain text via `err.toString()`, so a
            // best-effort body read is more useful than discarding it.
            let detail = resp.text().await.ok().map(|t| t.trim().to_string());
            let detail = detail.filter(|d| !d.is_empty());
            return Err(ApiError::Status {
                status: status.as_u16(),
                detail,
            });
        }
        resp.json::<T>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    pub async fn info(&self) -> Result<RobotInfo, ApiError> {
        let resp = self.request("").send().await.map_err(transport)?;
        Self::send_json(resp).await
    }

    /// The cached map, which Valetudo serves without contacting the robot.
    ///
    /// This is the cheap counterpart to `state()`. Prefer it anywhere a map is
    /// needed: it costs nothing on the miio link, so using `state()` here would
    /// spend a full poll to read data Valetudo already has.
    pub async fn map(&self) -> Result<crate::map::MapData, ApiError> {
        let resp = self.request("/state/map").send().await.map_err(transport)?;
        Self::send_json(resp).await
    }

    /// Upstream event stream for map changes.
    ///
    /// Valetudo caps this endpoint at five concurrent clients, so exactly one
    /// consumer is opened for the whole process and events are fanned out to
    /// browsers over our own stream.
    pub async fn map_events(&self) -> Result<reqwest::Response, ApiError> {
        let url = format!("{}/state/map/sse", self.base_url);
        let req = self.stream.get(url).header("accept", "text/event-stream");
        let req = match &self.auth {
            Some((u, p)) => req.basic_auth(u, Some(p)),
            None => req,
        };
        req.send().await.map_err(transport)
    }

    pub async fn state(&self) -> Result<RobotState, ApiError> {
        let resp = self.request("/state").send().await.map_err(transport)?;
        Self::send_json(resp).await
    }

    pub async fn capabilities(&self) -> Result<Vec<serde_json::Value>, ApiError> {
        let resp = self
            .request("/capabilities")
            .send()
            .await
            .map_err(transport)?;
        Self::send_json(resp).await
    }

    pub async fn consumables(&self) -> Result<Vec<Consumable>, ApiError> {
        let resp = self
            .request("/capabilities/ConsumableMonitoringCapability")
            .send()
            .await
            .map_err(transport)?;
        Self::send_json(resp).await
    }

    pub async fn segments(&self) -> Result<Vec<MapSegment>, ApiError> {
        let resp = self
            .request("/capabilities/MapSegmentationCapability")
            .send()
            .await
            .map_err(transport)?;
        Self::send_json(resp).await
    }

    pub async fn fan_speed_presets(&self) -> Result<Vec<String>, ApiError> {
        let resp = self
            .request("/capabilities/FanSpeedControlCapability/presets")
            .send()
            .await
            .map_err(transport)?;
        Self::send_json(resp).await
    }

    pub async fn basic_control(&self, action: &str) -> Result<(), ApiError> {
        // Whitelist rather than forward the path segment: this value ends up in
        // a JSON body, and Valetudo only accepts these four actions.
        let action = match action {
            "start" | "stop" | "pause" | "home" => action,
            other => {
                return Err(ApiError::Status {
                    status: 400,
                    detail: Some(format!("unsupported action: {other}")),
                })
            }
        };
        let resp = self
            .request_put("/capabilities/BasicControlCapability")
            .json(&BasicControlBody { action })
            .send()
            .await
            .map_err(transport)?;
        check_empty(resp).await
    }

    pub async fn set_fan_speed(&self, name: &str) -> Result<(), ApiError> {
        // Preset names are constrained to a safe character set; they end up in
        // a JSON body and we do not want to be usable as a smuggling vector if
        // this is ever called with user input from the web layer.
        if name.is_empty()
            || name.len() > 32
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(ApiError::Status {
                status: 400,
                detail: Some(format!("invalid fan speed preset: {name}")),
            });
        }
        let resp = self
            .request_put("/capabilities/FanSpeedControlCapability/preset")
            .json(&FanSpeedBody { name })
            .send()
            .await
            .map_err(transport)?;
        check_empty(resp).await
    }

    pub async fn clean_segments(
        &self,
        segment_ids: &[String],
        iterations: u32,
    ) -> Result<(), ApiError> {
        if segment_ids.is_empty() || segment_ids.len() > 64 {
            return Err(ApiError::Status {
                status: 400,
                detail: Some("between 1 and 64 segment ids required".into()),
            });
        }
        if segment_ids.iter().any(|id| id.is_empty() || id.len() > 64) {
            return Err(ApiError::Status {
                status: 400,
                detail: Some("invalid segment id".into()),
            });
        }
        let resp = self
            .request_put("/capabilities/MapSegmentationCapability")
            .json(&SegmentCleanBody {
                action: "start_segment_action",
                segment_ids: segment_ids.to_vec(),
                iterations: iterations.clamp(1, 10),
            })
            .send()
            .await
            .map_err(transport)?;
        check_empty(resp).await
    }

    /// Properties of the local camera stream, or `None` when the robot has no
    /// camera or the capability is absent.
    pub async fn duststreaming_properties(&self) -> Result<Option<DuststreamProperties>, ApiError> {
        let resp = self
            .request("/capabilities/DuststreamingCapability/properties")
            .send()
            .await
            .map_err(transport)?;

        if resp.status() == 404 {
            return Ok(None);
        }
        Self::send_json::<DuststreamProperties>(resp)
            .await
            .map(Some)
    }

    /// Clean a set of zones.
    ///
    /// The body is pre-rendered by the caller because the corner order and the
    /// `points` wrapper are Valetudo's, not ours.
    pub async fn clean_zones(&self, body: &crate::zone::ZoneCleanBody<'_>) -> Result<(), ApiError> {
        let resp = self
            .request_put("/capabilities/ZoneCleaningCapability")
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        check_empty(resp).await
    }

    /// Whether the robot is capturing obstacle images at all. Off by default,
    /// since it is a firmware setting rather than something Valetudo decides.
    #[allow(dead_code, reason = "surfaced in the UI on request")]
    pub async fn obstacle_images_enabled(&self) -> Result<bool, ApiError> {
        #[derive(Deserialize)]
        struct Enabled {
            enabled: bool,
        }
        let resp = self
            .request("/capabilities/ObstacleImagesCapability")
            .send()
            .await
            .map_err(transport)?;
        Self::send_json::<Enabled>(resp).await.map(|e| e.enabled)
    }

    pub async fn set_obstacle_images_enabled(&self, enable: bool) -> Result<(), ApiError> {
        #[derive(Serialize)]
        struct Body<'a> {
            action: &'a str,
        }
        let action = if enable { "enable" } else { "disable" };
        let resp = self
            .request_put("/capabilities/ObstacleImagesCapability")
            .json(&Body { action })
            .send()
            .await
            .map_err(transport)?;
        check_empty(resp).await
    }

    /// Stream one obstacle photo.
    ///
    /// Valetudo rate-limits this hard (3/s, 10 per 5s, 30 per 20s) because each
    /// fetch asks the firmware whether the feature is even enabled. Callers must
    /// therefore load on demand and cache, never eagerly.
    pub async fn obstacle_image(&self, id: &str) -> Result<reqwest::Response, ApiError> {
        let encoded = percent_encode(id);
        let resp = self
            .request(&format!(
                "/capabilities/ObstacleImagesCapability/img/{encoded}"
            ))
            .send()
            .await
            .map_err(transport)?;
        if !resp.status().is_success() {
            return Err(ApiError::Status {
                status: resp.status().as_u16(),
                detail: None,
            });
        }
        Ok(resp)
    }

    /// Proxy a GET to an arbitrary Valetudo path, preserving auth. Used for the
    /// camera stream, which is binary and therefore cannot go through the typed
    /// helpers above.
    pub async fn proxy_get(&self, path: &str) -> Result<reqwest::Response, ApiError> {
        let resp = self.request(path).send().await.map_err(transport)?;
        Ok(resp)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DuststreamProperties {
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(rename = "duststreamerInstalled", default)]
    pub duststreamer_installed: bool,
}

/// Percent-encode a path segment.
///
/// The previous hand-rolled dependency was removed when the read paths became
/// cache-backed, and the only value that still needs encoding is an obstacle id,
/// which comes from the robot and may contain characters that would otherwise
/// change the path.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn transport(e: reqwest::Error) -> ApiError {
    ApiError::Transport(describe_transport(&e))
}

/// Flatten an error and its causes into one string.
///
/// `reqwest::Error::to_string()` is only "error sending request for url"; the
/// part that identifies the problem is the OS-level cause further down the chain
/// ("Connection refused", "Name or service not known"). Without the chain the
/// diagnostics can only say "something went wrong", which is useless.
fn describe_transport(err: &reqwest::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = std::error::Error::source(err as &dyn std::error::Error);
    while let Some(cause) = source {
        let text = cause.to_string();
        // Stop before the URL, which adds nothing and can be long.
        if text.starts_with("for url") {
            break;
        }
        parts.push(text);
        source = cause.source();
    }
    parts.join(": ")
}

async fn check_empty(resp: reqwest::Response) -> Result<(), ApiError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let detail = resp.text().await.ok().map(|t| t.trim().to_string());
    Err(ApiError::Status {
        status: status.as_u16(),
        detail: detail.filter(|d| !d.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_flattens_attributes() {
        let state: RobotState = serde_json::from_str(
            r#"{
              "attributes": [
                {"__class": "StatusStateAttribute", "metaData": {}, "value": "cleaning", "flag": "zone"},
                {"__class": "BatteryStateAttribute", "metaData": {}, "level": 87.5, "flag": "discharging"},
                {"__class": "PresetSelectionStateAttribute", "metaData": {}, "type": "fan_speed", "value": "turbo"},
                {"__class": "PresetSelectionStateAttribute", "metaData": {}, "type": "water_grade", "value": "high"},
                {"__class": "SomethingNewAttribute", "metaData": {}}
              ],
              "map": null
            }"#,
        )
        .expect("state should parse");

        let summary = Summary::from_state(&state);
        assert_eq!(summary.status.as_deref(), Some("cleaning"));
        assert_eq!(summary.status_flag.as_deref(), Some("zone"));
        assert_eq!(summary.battery, Some(87.5));
        assert_eq!(summary.fan_speed.as_deref(), Some("turbo"));
        assert_eq!(summary.water_grade.as_deref(), Some("high"));
    }

    #[test]
    fn state_without_attributes_is_tolerated() {
        let state: RobotState = serde_json::from_str("{}").expect("empty state should parse");
        assert!(state.attributes.is_empty());
        assert!(Summary::from_state(&state).status.is_none());
    }

    #[test]
    fn transport_failures_are_classified() {
        let classify = |msg: &str| ApiError::Transport(msg.to_string()).failure();
        assert_eq!(
            classify("error sending request for url"),
            Failure::Unreachable
        );
        assert_eq!(
            classify("Connection refused (os error 61)"),
            Failure::ConnectionRefused
        );
        assert_eq!(
            classify("dns error: failed to lookup address information: Name or service not known"),
            Failure::UnresolvedHost
        );
        assert_eq!(classify("operation timed out"), Failure::Timeout);
    }

    #[test]
    fn status_failures_are_classified() {
        assert_eq!(
            ApiError::Status {
                status: 401,
                detail: None
            }
            .failure(),
            Failure::Unauthorized
        );
        assert_eq!(
            ApiError::Status {
                status: 403,
                detail: None
            }
            .failure(),
            Failure::Forbidden
        );
        assert_eq!(
            ApiError::Status {
                status: 503,
                detail: None
            }
            .failure(),
            Failure::Status {
                status: 503,
                detail: None
            }
        );
        assert_eq!(ApiError::Decode("x".into()).failure(), Failure::Decode);
    }

    #[test]
    fn every_failure_has_advice() {
        for failure in [
            Failure::UnresolvedHost,
            Failure::ConnectionRefused,
            Failure::Unreachable,
            Failure::Timeout,
            Failure::Unauthorized,
            Failure::Forbidden,
            Failure::Status {
                status: 404,
                detail: None,
            },
            Failure::Decode,
        ] {
            assert!(!failure.summary().is_empty());
            assert!(!failure.advice().is_empty());
        }
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(percent_encode("abc123"), "abc123");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
        // A slash in an id would otherwise traverse the path.
        assert_eq!(percent_encode("a/b"), "a%2Fb");
        assert_eq!(percent_encode(".."), "..");
        assert_eq!(percent_encode("a b"), "a%20b");
    }

    #[test]
    fn consumables_parse_valetudo_shape() {
        let parsed: Vec<Consumable> = serde_json::from_str(
            r#"[
              {"__class":"ValetudoConsumable","metaData":{},"type":"brush","subType":"main",
               "remaining":{"value":123456,"unit":"minutes"}},
              {"__class":"ValetudoConsumable","metaData":{},"type":"filter","subType":"none",
               "remaining":{"value":40,"unit":"percent"}}
            ]"#,
        )
        .expect("consumables should parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].remaining.unit, "minutes");
        assert_eq!(parsed[1].sub_type, "none");
    }
}

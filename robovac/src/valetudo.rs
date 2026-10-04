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

#[derive(Debug, Clone, Deserialize)]
pub struct Consumable {
    #[serde(rename = "type")]
    pub consumable_type: String,
    #[serde(rename = "subType", default)]
    pub sub_type: String,
    pub remaining: Remaining,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Remaining {
    pub value: f64,
    pub unit: String,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug)]
pub enum ApiError {
    Transport(String),
    Status { status: u16, detail: Option<String> },
    Decode(String),
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

        Ok(Self {
            http,
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

fn transport(e: reqwest::Error) -> ApiError {
    ApiError::Transport(e.to_string())
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

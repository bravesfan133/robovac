use std::sync::RwLock;
use std::time::Instant;

use serde_json::Value;

use crate::map::MapData;
use crate::valetudo::{
    Consumable, DuststreamProperties, MapSegment, RobotInfo, RobotState, Summary,
};

/// An obstacle the robot reported, with enough detail to place it on the map.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Obstacle {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub angle: f64,
}

/// Everything the dashboard renders, cached.
///
/// Valetudo distinguishes two kinds of read, and the difference matters a lot on
/// miio:
///
///   * `GET /state` calls `pollState()`, which *contacts the robot* and costs
///     about a second.
///   * `GET /state/map` and `GET /state/attributes` serve Valetudo's own cached
///     copy and cost nothing.
///
/// So this process keeps exactly one poller (`main::poll_loop`) doing the
/// expensive read, and every HTTP handler answers from here. A page load costs
/// zero robot round trips instead of two, and renders instantly.
///
/// Cold-cache reads block on the poller rather than triggering their own poll,
/// so a burst of requests cannot stampede the robot.
pub struct RobotCache {
    inner: RwLock<Inner>,
}

struct Inner {
    info: Option<RobotInfo>,
    summary: Summary,
    consumables: Vec<Consumable>,
    segments: Vec<MapSegment>,
    fan_presets: Vec<String>,
    camera: Option<DuststreamProperties>,
    /// Memoised render, keyed by the selection it was rendered for. One entry
    /// is enough: nearly every request shares the same selection, and holding
    /// more would just cache memory nobody reads.
    rendered: Option<(Vec<String>, String)>,
    /// Bumped whenever the map changes, so clients can skip redundant work.
    map_version: u64,
    /// Raw map, kept for callers that need geometry rather than a rendered SVG.
    map: Option<MapData>,
    /// Obstacle markers from the current map: `(id, x, y, angle)`.
    obstacles: Vec<Obstacle>,
    last_ok: Option<Instant>,
    last_error: Option<String>,
    /// True once a poll has ever succeeded, so "never worked" and "broke" are
    /// distinguishable in the diagnostics panel.
    ever_ok: bool,
}

impl Default for RobotCache {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Inner {
                info: None,
                summary: Summary::default(),
                consumables: Vec::new(),
                segments: Vec::new(),
                fan_presets: Vec::new(),
                camera: None,
                rendered: None,
                map_version: 0,
                map: None,
                obstacles: Vec::new(),
                last_ok: None,
                last_error: None,
                ever_ok: false,
            }),
        }
    }
}

/// A consistent snapshot, so a handler never sees a half-updated view.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub info: Option<RobotInfo>,
    pub summary: Summary,
    pub consumables: Vec<Consumable>,
    pub segments: Vec<MapSegment>,
    pub fan_presets: Vec<String>,
    pub camera: Option<DuststreamProperties>,
    pub map_version: u64,
    pub last_ok: Option<Instant>,
    pub last_error: Option<String>,
    pub ever_ok: bool,
    pub obstacles: Vec<Obstacle>,
    /// False before the first successful poll, so handlers can distinguish
    /// "still warming up" from "the robot is gone".
    pub warm: bool,
}

/// Compare-and-swap for the stored map. Lives on `Inner` so both entry points
/// share exactly one definition of "did this change".
impl Inner {
    fn absorb(&mut self, map: MapData) {
        // Obstacle markers are cheap to collect while the map is in hand, and
        // the panel needs position and heading to draw a compass needle.
        self.obstacles = map
            .entities
            .iter()
            .filter(|e| e.entity_type == "obstacle")
            .filter_map(|e| {
                let id = e.image_id()?.to_string();
                let (x, y) = (e.points.first().copied()?, *e.points.get(1)?);
                Some(Obstacle {
                    id,
                    x,
                    y,
                    angle: e.angle(),
                })
            })
            .collect();

        // Conservative proxy: the robot rewrites the map wholesale as it
        // explores, so these totals move whenever anything meaningful did.
        let changed = match self.map.as_ref() {
            Some(previous) => {
                previous.size_fingerprint() != map.size_fingerprint()
                    || previous.pixel_total() != map.pixel_total()
                    || previous.entity_count() != map.entity_count()
            }
            None => true,
        };

        if !changed {
            return;
        }

        self.map = Some(map);
        self.rendered = None;
        self.map_version = self.map_version.wrapping_add(1);
    }
}

impl RobotCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.read();
        Snapshot {
            info: inner.info.clone(),
            summary: inner.summary.clone(),
            consumables: inner.consumables.clone(),
            segments: inner.segments.clone(),
            fan_presets: inner.fan_presets.clone(),
            camera: inner.camera.clone(),
            map_version: inner.map_version,
            last_ok: inner.last_ok,
            last_error: inner.last_error.clone(),
            ever_ok: inner.ever_ok,
            obstacles: inner.obstacles.clone(),
            warm: inner.last_ok.is_some(),
        }
    }

    pub fn record_success(&self) {
        let mut inner = self.write();
        inner.last_ok = Some(Instant::now());
        inner.last_error = None;
        inner.ever_ok = true;
    }

    pub fn record_failure(&self, message: String) {
        let mut inner = self.write();
        inner.last_error = Some(message);
    }

    pub fn update_info(&self, info: RobotInfo) {
        self.write().info = Some(info);
    }

    pub fn update_meta(
        &self,
        consumables: Vec<Consumable>,
        segments: Vec<MapSegment>,
        fan_presets: Vec<String>,
        camera: Option<DuststreamProperties>,
    ) {
        let mut inner = self.write();
        inner.consumables = consumables;
        inner.segments = segments;
        inner.fan_presets = fan_presets;
        inner.camera = camera;
    }

    /// Fold a freshly polled state into the cache. The map is kept as geometry
    /// rather than a rendered SVG, because the rendered form depends on which
    /// rooms the *requesting browser* has selected.
    ///
    /// Replacing the map only when it actually differs matters: a full JSON
    /// parse plus re-serialise every two seconds for an unchanged map is waste,
    /// and parsing is the expensive half.
    pub fn update_state(&self, state: &RobotState) {
        let mut inner = self.write();
        inner.summary = Summary::from_state(state);

        let Some(raw) = state.map.as_ref() else {
            return;
        };
        let Ok(map) = serde_json::from_value::<MapData>(raw.clone()) else {
            return;
        };

        inner.absorb(map);
    }

    /// Replace the map directly, as when the upstream event stream says it
    /// moved. Same change detection as `update_state`: an identical map must not
    /// bump the version, or every browser re-fetches a document it already has.
    pub fn set_map(&self, map: MapData) {
        self.write().absorb(map);
    }

    /// Render the map for a particular selection, reusing the memoised result
    /// when the request asks for the same thing.
    pub fn render_map(&self, selected: &[String]) -> Option<String> {
        let mut inner = self.write();
        let map = inner.map.as_ref()?;

        if let Some((cached_for, svg)) = &inner.rendered {
            if cached_for.as_slice() == selected {
                return Some(svg.clone());
            }
        }

        let svg = crate::map::render_svg(map, selected);
        inner.rendered = Some((selected.to_vec(), svg.clone()));
        Some(svg)
    }

    /// Force a re-render on the next state update, e.g. after the map was reset.
    #[allow(dead_code, reason = "used with MapResetCapability")]
    pub fn invalidate_map(&self) {
        let mut inner = self.write();
        inner.map = None;
        inner.rendered = None;
    }

    /// Map geometry, for features that need coordinates rather than an already
    /// rendered SVG. Consumed by zone drawing in a later change; keeping the
    /// accessor now avoids re-plumbing the cache later.
    #[allow(dead_code, reason = "used by zone drawing")]
    pub fn map(&self) -> Option<MapData> {
        self.read().map.clone()
    }

    /// Pixel scale and mapped extent, for turning a drawn rectangle back into
    /// map coordinates: `(min_x, min_y, pixel_size, max_x)`.
    pub fn map_extent(&self) -> Option<(f64, f64, f64, f64)> {
        let inner = self.read();
        let map = inner.map.as_ref()?;
        // `extent` yields pixels; the caller needs pixels for the inverse of the
        // SVG transform and units for the request body.
        map.extent()
            .map(|(min_x, min_y, max_x)| (min_x, min_y, map.pixel_size, max_x))
    }

    /// Monotonic stamp for the rendered map, so clients can skip re-rendering
    /// an unchanged image.
    #[allow(dead_code, reason = "consumed by the live-map client")]
    pub fn map_version(&self) -> u64 {
        self.read().map_version
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        // A panic while holding the lock would only ever be a bug in this
        // process; recovering keeps the UI serving rather than cascading.
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// Build the payload the SSE stream publishes after every poll.
pub fn broadcast_payload(cache: &RobotCache) -> Value {
    let snap = cache.snapshot();
    serde_json::json!({
        "summary": snap.summary,
        "map_version": snap.map_version,
        "ok": snap.warm,
        "last_error": snap.last_error,
        "segments": snap.segments.iter().map(|s| serde_json::json!({
            "id": s.id,
            "name": s.name,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_map(pixels: Vec<i64>) -> RobotState {
        let pixels = pixels
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        serde_json::from_str(&format!(
            r#"{{
              "attributes": [
                {{"__class":"StatusStateAttribute","metaData":{{}},"value":"cleaning","flag":"none"}},
                {{"__class":"BatteryStateAttribute","metaData":{{}},"level":55.0,"flag":"discharging"}}
              ],
              "map": {{
                "metaData": {{"version": 2}},
                "pixelSize": 5,
                "layers": [{{"type":"floor","pixels":[{pixels}],"metaData":{{}}}}],
                "entities": []
              }}
            }}"#
        ))
        .expect("state should parse")
    }

    /// A map with two named segments, needed to test selection-aware rendering.
    fn state_with_segments() -> RobotState {
        serde_json::from_str(
            r#"{
              "attributes": [{"__class":"StatusStateAttribute","metaData":{},"value":"idle","flag":"none"}],
              "map": {
                "metaData": {"version": 2},
                "pixelSize": 5,
                "layers": [
                  {"type":"floor","pixels":[0,0, 1,0, 0,1, 1,1],"metaData":{}},
                  {"type":"segment","compressedPixels":[0,0,2],"metaData":{"segmentId":"1","name":"Kitchen"}},
                  {"type":"segment","pixels":[0,1,1,1],"metaData":{"segmentId":"2","name":"Hall"}}
                ],
                "entities": []
              }
            }"#,
        )
        .expect("segment map should parse")
    }

    #[test]
    fn cold_cache_reports_not_warm() {
        let cache = RobotCache::new();
        let snap = cache.snapshot();
        assert!(!snap.warm);
        assert!(snap.last_ok.is_none());
        assert!(!snap.ever_ok);
    }

    #[test]
    fn success_marks_warm_and_clears_error() {
        let cache = RobotCache::new();
        cache.record_failure("boom".into());
        assert!(cache.snapshot().last_error.is_some());

        cache.record_success();
        let snap = cache.snapshot();
        assert!(snap.warm);
        assert!(snap.ever_ok);
        assert!(snap.last_error.is_none());
        assert!(snap.last_ok.is_some());
    }

    #[test]
    fn map_is_stored_and_renderable() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0, 1, 0]));
        assert_eq!(cache.map_version(), 1);
        assert!(cache.render_map(&[]).is_some(), "map should render");
    }

    #[test]
    fn unchanged_map_does_not_bump_the_version() {
        let cache = RobotCache::new();
        let state = state_with_map(vec![0, 0, 1, 0]);

        cache.update_state(&state);
        assert_eq!(cache.map_version(), 1);

        // Identical map: re-parsing happens but the stored geometry is kept, so
        // the version and any memoised render stay valid.
        cache.update_state(&state);
        assert_eq!(
            cache.map_version(),
            1,
            "unchanged map must not bump version"
        );
    }

    #[test]
    fn changed_map_bumps_the_version() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0]));
        cache.update_state(&state_with_map(vec![0, 0, 1, 0, 2, 0]));
        assert_eq!(cache.map_version(), 2);
    }

    #[test]
    fn render_is_memoised_per_selection() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_segments());

        let first = cache.render_map(&[]).unwrap();
        assert_eq!(first, cache.render_map(&[]).unwrap(), "memoised");
        // Check the group attribute, not the bare token: the embedded <style>
        // block legitimately contains `data-selected="true"` as a selector.
        assert!(first.contains("data-segment-id=\"1\" data-selected=\"false\""));

        // A different selection must produce a different document, and must not
        // hand back the memoised one.
        let selected = cache.render_map(&["2".to_string()]).unwrap();
        assert_ne!(first, selected, "selection must reach the render");
        assert!(selected.contains("data-segment-id=\"2\" data-selected=\"true\""));
        // ...and switching back must not return the stale selected render.
        assert_eq!(first, cache.render_map(&[]).unwrap());
    }

    #[test]
    fn summary_tracks_the_latest_state() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0]));
        let snap = cache.snapshot();
        assert_eq!(snap.summary.status.as_deref(), Some("cleaning"));
        assert_eq!(snap.summary.battery, Some(55.0));
    }

    #[test]
    fn invalidate_forces_the_map_to_be_replaced() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0]));
        assert!(cache.render_map(&[]).is_some());

        cache.invalidate_map();
        assert!(cache.render_map(&[]).is_none(), "map should be gone");
    }

    #[test]
    fn state_without_map_keeps_the_previous_one() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0]));
        let before = cache.render_map(&[]).unwrap();

        let no_map: RobotState = serde_json::from_str(
            r#"{"attributes":[{"__class":"StatusStateAttribute","metaData":{},"value":"idle","flag":"none"}]}"#,
        )
        .unwrap();
        cache.update_state(&no_map);

        let snap = cache.snapshot();
        assert_eq!(snap.summary.status.as_deref(), Some("idle"));
        assert_eq!(
            cache.render_map(&[]).unwrap(),
            before,
            "a poll without a map must not blank the floor plan"
        );
    }

    #[test]
    fn broadcast_payload_is_serialisable() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0]));
        cache.record_success();
        let payload = broadcast_payload(&cache);
        assert_eq!(payload["ok"], Value::Bool(true));
        assert_eq!(payload["map_version"], Value::from(1));
        assert!(payload["segments"].is_array());
    }
}

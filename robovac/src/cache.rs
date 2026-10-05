use std::sync::RwLock;
use std::time::Instant;

use serde_json::Value;

use crate::map::MapData;
use crate::valetudo::{
    Consumable, DuststreamProperties, MapSegment, RobotInfo, RobotState, Summary,
};

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
    map_svg: Option<String>,
    /// Bumped whenever the map changes, so the UI can skip redundant re-renders.
    map_version: u64,
    /// Raw map, kept for callers that need geometry rather than a rendered SVG.
    map: Option<MapData>,
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
                map_svg: None,
                map_version: 0,
                map: None,
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
    pub map_svg: Option<String>,
    pub map_version: u64,
    pub last_ok: Option<Instant>,
    pub last_error: Option<String>,
    pub ever_ok: bool,
    /// False before the first successful poll, so handlers can distinguish
    /// "still warming up" from "the robot is gone".
    pub warm: bool,
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
            map_svg: inner.map_svg.clone(),
            map_version: inner.map_version,
            last_ok: inner.last_ok,
            last_error: inner.last_error.clone(),
            ever_ok: inner.ever_ok,
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

    /// Fold a freshly polled state into the cache, re-rendering the map only
    /// when it actually differs. Re-serialising a large SVG every two seconds
    /// for an unchanged map is pure waste.
    pub fn update_state(&self, state: &RobotState, render: impl Fn(&MapData) -> String) {
        let mut inner = self.write();
        inner.summary = Summary::from_state(state);

        let Some(raw) = state.map.as_ref() else {
            return;
        };
        let Ok(map) = serde_json::from_value::<MapData>(raw.clone()) else {
            return;
        };

        let changed = match inner.map.as_ref() {
            Some(previous) => {
                // Compare the cheap discriminant first: map_version only changes
                // when the pixels differ, and a version stamp would not survive
                // a restart, so compare sizes as a conservative proxy too.
                previous.size_fingerprint() != map.size_fingerprint()
                    || previous.pixel_total() != map.pixel_total()
                    || previous.entity_count() != map.entity_count()
            }
            None => true,
        };

        if changed {
            inner.map_svg = Some(render(&map));
            inner.map = Some(map);
            inner.map_version = inner.map_version.wrapping_add(1);
        }
    }

    /// Force a re-render on the next state update, e.g. after the map was reset.
    #[allow(dead_code, reason = "used with MapResetCapability")]
    pub fn invalidate_map(&self) {
        let mut inner = self.write();
        inner.map = None;
        inner.map_svg = None;
    }

    /// Map geometry, for features that need coordinates rather than an already
    /// rendered SVG. Consumed by zone drawing in a later change; keeping the
    /// accessor now avoids re-plumbing the cache later.
    #[allow(dead_code, reason = "used by zone drawing")]
    pub fn map(&self) -> Option<MapData> {
        self.read().map.clone()
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

    /// Counts how many times the map was actually re-rendered.
    fn counting_render<'c>(
        renders: &'c std::cell::Cell<u32>,
    ) -> impl for<'a> Fn(&'a MapData) -> String + 'c {
        move |_: &MapData| {
            renders.set(renders.get() + 1);
            "<svg/>".to_string()
        }
    }

    #[test]
    fn state_update_renders_map_once_and_reuses_it() {
        let cache = RobotCache::new();
        let renders = std::cell::Cell::new(0u32);

        let state = state_with_map(vec![0, 0, 1, 0]);
        cache.update_state(&state, counting_render(&renders));
        assert_eq!(renders.get(), 1);
        let version = cache.map_version();
        assert_eq!(version, 1);
        assert_eq!(cache.snapshot().map_svg.as_deref(), Some("<svg/>"));

        // Identical map: summary refreshed, but no re-render.
        cache.update_state(&state, counting_render(&renders));
        assert_eq!(renders.get(), 1, "unchanged map must not re-render");
        assert_eq!(cache.map_version(), version);
    }

    #[test]
    fn changed_map_triggers_rerender() {
        let cache = RobotCache::new();
        let renders = std::cell::Cell::new(0u32);

        cache.update_state(&state_with_map(vec![0, 0]), counting_render(&renders));
        cache.update_state(
            &state_with_map(vec![0, 0, 1, 0, 2, 0]),
            counting_render(&renders),
        );
        assert_eq!(renders.get(), 2);
        assert_eq!(cache.map_version(), 2);
    }

    #[test]
    fn summary_tracks_the_latest_state() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0]), |_| "<svg/>".into());
        let snap = cache.snapshot();
        assert_eq!(snap.summary.status.as_deref(), Some("cleaning"));
        assert_eq!(snap.summary.battery, Some(55.0));
    }

    #[test]
    fn invalidate_forces_next_render() {
        let cache = RobotCache::new();
        let renders = std::cell::Cell::new(0u32);
        cache.update_state(&state_with_map(vec![0, 0]), counting_render(&renders));
        cache.invalidate_map();
        cache.update_state(&state_with_map(vec![0, 0]), counting_render(&renders));
        assert_eq!(renders.get(), 2);
    }

    #[test]
    fn state_without_map_leaves_previous_map_alone() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0]), |_| "<svg/>".into());

        let no_map: RobotState = serde_json::from_str(
            r#"{"attributes":[{"__class":"StatusStateAttribute","metaData":{},"value":"idle","flag":"none"}]}"#,
        )
        .unwrap();
        cache.update_state(&no_map, |_| panic!("must not render without a map"));

        let snap = cache.snapshot();
        assert_eq!(snap.summary.status.as_deref(), Some("idle"));
        assert_eq!(
            snap.map_svg.as_deref(),
            Some("<svg/>"),
            "map should persist"
        );
    }

    #[test]
    fn broadcast_payload_is_serialisable() {
        let cache = RobotCache::new();
        cache.update_state(&state_with_map(vec![0, 0]), |_| "<svg/>".into());
        cache.record_success();
        let payload = broadcast_payload(&cache);
        assert_eq!(payload["ok"], Value::Bool(true));
        assert_eq!(payload["map_version"], Value::from(1));
        assert!(payload["segments"].is_array());
    }
}

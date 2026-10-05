use std::collections::BTreeMap;

use askama::Template;
use serde::Serialize;

use crate::cache::Snapshot;
use crate::valetudo::{Consumable, MapSegment, Summary};

#[derive(Template)]
#[template(path = "dashboard.html", escape = "html")]
pub struct Dashboard<'a> {
    pub title: &'a str,
    pub info: Option<Info>,
    pub summary: Summary,
    pub consumables: Vec<ConsumableView>,
    pub segments: Vec<SegmentView>,
    pub fan_presets: Vec<String>,
    pub camera: Option<CameraView>,
    /// True when at least one poll has ever succeeded.
    pub connected: bool,
    /// Distinguishes "still starting up" from "the robot is gone", which the
    /// template renders differently.
    pub ever_connected: bool,
    pub error: Option<String>,
    /// Classified cause, rendered as a short heading.
    pub failure_summary: Option<String>,
    /// What the user can do about it.
    pub failure_advice: Option<String>,
    pub contact_note: Option<String>,
    pub selected_segments: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Info {
    pub manufacturer: String,
    pub model_name: String,
    pub implementation: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConsumableView {
    pub label: String,
    pub value: f64,
    pub unit: String,
    pub percent: Option<f64>,
}

impl ConsumableView {
    pub fn from(c: &Consumable) -> Self {
        let base = match c.consumable_type.as_str() {
            "filter" => "Filter",
            "brush" => "Brush",
            "mop" => "Mop",
            "detergent" => "Detergent",
            "bin" => "Dust bin",
            "cleaning" => "Cleaning solution",
            other => other,
        };
        let label = if c.sub_type.is_empty() || c.sub_type == "none" {
            base.to_string()
        } else {
            format!("{base} ({})", prettify(&c.sub_type))
        };

        let percent =
            (c.remaining.unit == "percent").then_some(c.remaining.value.clamp(0.0, 100.0));

        Self {
            label,
            value: c.remaining.value,
            unit: c.remaining.unit.clone(),
            percent,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SegmentView {
    pub id: String,
    pub name: String,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CameraView {
    pub width: u32,
    pub height: u32,
    pub available: bool,
}

fn prettify(value: &str) -> String {
    value.replace('_', " ")
}

impl<'a> Dashboard<'a> {
    /// Battery as a ready-to-print string; the template has no arithmetic.
    pub fn battery_display(&self) -> String {
        match self.summary.battery {
            Some(level) => format!("{level:.0}%"),
            None => "\u{2014}".to_string(),
        }
    }

    /// The first poll has not succeeded yet, and never has: the poller may not
    /// even have run. Distinct from "was working, now isn't".
    pub fn warming() -> Self {
        Self {
            title: "Robovac",
            info: None,
            summary: Summary::default(),
            consumables: Vec::new(),
            segments: Vec::new(),
            fan_presets: Vec::new(),
            camera: None,
            connected: false,
            ever_connected: false,
            error: None,
            failure_summary: None,
            failure_advice: None,
            contact_note: None,
            selected_segments: Vec::new(),
        }
    }

    pub fn from_snapshot(snap: &Snapshot, selected: Vec<String>) -> Self {
        let info = snap.info.as_ref().map(|i| Info {
            manufacturer: i.manufacturer.clone(),
            model_name: i.model_name.clone(),
            implementation: i.implementation.clone(),
        });

        let consumables = snap.consumables.iter().map(ConsumableView::from).collect();

        let segments: Vec<SegmentView> = snap
            .segments
            .iter()
            .map(|s: &MapSegment| {
                let name = s
                    .name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| s.id.clone());
                SegmentView {
                    selected: selected.contains(&s.id),
                    id: s.id.clone(),
                    name,
                }
            })
            .collect();

        let camera = snap.camera.as_ref().map(|p| CameraView {
            width: p.width,
            height: p.height,
            available: p.duststreamer_installed,
        });

        // "Connected 4s ago" is far more useful than a bare timestamp, and it is
        // the difference between a robot that is idle and one that fell off.
        let contact_note = snap.last_ok.map(|at| {
            let secs = at.elapsed().as_secs();
            if secs < 5 {
                "just now".to_string()
            } else if secs < 90 {
                format!("{secs}s ago")
            } else if secs < 5400 {
                format!("{}m ago", secs / 60)
            } else {
                format!("{}h ago", secs / 3600)
            }
        });

        let title = if snap.warm {
            "Robovac"
        } else if snap.ever_ok {
            "Robovac \u{2014} unreachable"
        } else {
            "Robovac \u{2014} connecting"
        };

        Self {
            title,
            info,
            summary: snap.summary.clone(),
            consumables,
            segments,
            fan_presets: snap.fan_presets.clone(),
            camera,
            connected: snap.warm,
            ever_connected: snap.ever_ok,
            error: snap.last_error.clone(),
            failure_summary: snap.last_failure.as_ref().map(|f| f.summary().to_string()),
            failure_advice: snap.last_failure.as_ref().map(|f| f.advice().to_string()),
            contact_note,
            selected_segments: selected,
        }
    }
}

/// Parse the comma-separated `segments` query parameter.
pub fn parse_selected(raw: Option<&String>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut seen = BTreeMap::new();
    for part in raw.split(',') {
        let id = part.trim();
        // Segment ids are opaque; keep them short and printable so they cannot
        // bloat the page or smuggle markup.
        if id.is_empty() || id.len() > 64 {
            continue;
        }
        // Hyphens and underscores are allowed, but an id made only of them is
        // not a real segment id.
        let allowed = id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !allowed || !id.chars().any(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        seen.insert(id.to_string(), ());
    }
    seen.into_keys().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_dedupes_selected_segments() {
        let raw = " 3,1 ,3,bad id,<>,-".to_string();
        assert_eq!(parse_selected(Some(&raw)), vec!["1", "3"]);
    }

    #[test]
    fn selected_absent_is_empty() {
        assert!(parse_selected(None).is_empty());
    }

    #[test]
    fn consumable_labels_are_humanised() {
        let c = Consumable {
            consumable_type: "brush".into(),
            sub_type: "side_right".into(),
            remaining: crate::valetudo::Remaining {
                value: 1200.0,
                unit: "minutes".into(),
            },
        };
        let view = ConsumableView::from(&c);
        assert_eq!(view.label, "Brush (side right)");
        assert_eq!(view.percent, None);
    }

    #[test]
    fn percent_consumables_expose_percent() {
        let c = Consumable {
            consumable_type: "filter".into(),
            sub_type: "none".into(),
            remaining: crate::valetudo::Remaining {
                value: 42.0,
                unit: "percent".into(),
            },
        };
        assert_eq!(ConsumableView::from(&c).percent, Some(42.0));
    }

    #[test]
    fn warming_is_distinct_from_unreachable() {
        let warming = Dashboard::warming();
        assert!(!warming.connected);
        assert!(!warming.ever_connected);

        // Never connected: "connecting", not "unreachable".
        let snap = Snapshot::default();
        assert!(Dashboard::from_snapshot(&snap, vec![])
            .title
            .contains("connecting"));
    }

    #[test]
    fn snapshot_drives_segments_and_selection() {
        let snap = Snapshot {
            segments: vec![
                MapSegment {
                    id: "1".into(),
                    name: Some("Kitchen".into()),
                },
                MapSegment {
                    id: "2".into(),
                    name: None,
                },
            ],
            ..Default::default()
        };
        let dash = Dashboard::from_snapshot(&snap, vec!["2".to_string()]);
        assert_eq!(dash.segments.len(), 2);
        assert!(!dash.segments[0].selected);
        assert!(
            dash.segments[1].selected,
            "unnamed segment should fall back to its id"
        );
        assert_eq!(dash.segments[1].name, "2");
    }
}

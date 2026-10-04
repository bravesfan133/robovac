use std::collections::BTreeMap;

use askama::Template;
use serde::Serialize;

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
    pub connected: bool,
    pub error: Option<String>,
}

impl Dashboard<'_> {
    /// Battery as a ready-to-print string; the template has no arithmetic.
    pub fn battery_display(&self) -> String {
        match self.summary.battery {
            Some(level) => format!("{level:.0}%"),
            None => "\u{2014}".to_string(),
        }
    }
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
    pub sub_type: String,
    pub value: f64,
    pub unit: String,
    /// Percentage remaining, when the consumable reports in percent.
    pub percent: Option<f64>,
}

impl ConsumableView {
    pub fn from(c: &Consumable) -> Self {
        let label = match c.consumable_type.as_str() {
            "filter" => "Filter",
            "brush" => "Brush",
            "mop" => "Mop",
            "detergent" => "Detergent",
            "bin" => "Dust bin",
            "cleaning" => "Cleaning solution",
            other => other,
        };
        let label = if c.sub_type.is_empty() || c.sub_type == "none" {
            label.to_string()
        } else {
            format!("{label} ({})", prettify(&c.sub_type))
        };

        let percent =
            (c.remaining.unit == "percent").then_some(c.remaining.value.clamp(0.0, 100.0));

        Self {
            label,
            sub_type: c.sub_type.clone(),
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

/// Everything the dashboard needs, gathered concurrently so a slow capability
/// does not serialise behind the others.
pub async fn gather(
    valetudo: &crate::valetudo::Valetudo,
    selected: Vec<String>,
) -> Result<(Dashboard<'_>,), String> {
    let (info, state, consumables, segments, fan_presets, camera) = futures_util::join!(
        valetudo.info(),
        valetudo.state(),
        valetudo.consumables(),
        valetudo.segments(),
        valetudo.fan_speed_presets(),
        valetudo.duststreaming_properties(),
    );

    // State is the one thing the page cannot render without.
    let state = state.map_err(|e| e.to_string())?;

    // Everything else degrades to an empty section rather than a broken page.
    let info = info.ok().map(|i| Info {
        manufacturer: i.manufacturer,
        model_name: i.model_name,
        implementation: i.implementation,
    });

    let warnings: Vec<String> = [
        consumables.as_ref().err().map(|e| e.to_string()),
        segments.as_ref().err().map(|e| e.to_string()),
        fan_presets.as_ref().err().map(|e| e.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect();

    let consumables = consumables
        .unwrap_or_default()
        .iter()
        .map(ConsumableView::from)
        .collect();

    let segment_views: Vec<SegmentView> = segments
        .unwrap_or_default()
        .into_iter()
        .map(|s: MapSegment| {
            let name = s
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| s.id.clone());
            SegmentView {
                selected: selected.contains(&s.id),
                id: s.id,
                name,
            }
        })
        .collect();

    let camera = camera.ok().flatten().map(|p| CameraView {
        width: p.width,
        height: p.height,
        available: p.duststreamer_installed,
    });

    let dashboard = Dashboard {
        title: "Robovac",
        info,
        summary: Summary::from_state(&state),
        consumables,
        segments: segment_views,
        fan_presets: fan_presets.unwrap_or_default(),
        camera,
        connected: true,
        error: warnings.first().cloned(),
    };

    Ok((dashboard,))
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
        // be used to bloat the page or smuggle markup.
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
        let view = ConsumableView::from(&c);
        assert_eq!(view.label, "Filter");
        assert_eq!(view.percent, Some(42.0));
    }
}

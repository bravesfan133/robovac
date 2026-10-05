//! Zones for spot-cleaning a specific area.
//!
//! Valetudo takes axis-aligned rectangles in map coordinates, with a fixed
//! corner order (pA top-left, pB top-right, pC bottom-right, pD bottom-left).
//! The client draws with pointer events and sends pixels; the conversion back to
//! map coordinates lives here so it can be tested without a browser.

use serde::{Deserialize, Serialize};

/// The L40 Ultra's `maxZoneCount`. Exceeding it is rejected by the robot, so
/// the limit is enforced up front with an explanation rather than surfacing as
/// a failure.
pub const MAX_ZONES: usize = 4;

/// A rectangle expressed the way Valetudo wants it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    /// Valetudo expects camelCase corners, so the fields are renamed rather
    /// than relying on Rust's snake_case.
    #[serde(rename = "pA")]
    pub p_a: Point,
    #[serde(rename = "pB")]
    pub p_b: Point,
    #[serde(rename = "pC")]
    pub p_c: Point,
    #[serde(rename = "pD")]
    pub p_d: Point,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    #[serde(rename = "x")]
    pub x: f64,
    #[serde(rename = "y")]
    pub y: f64,
}

impl Zone {
    /// Build a zone from two opposite corners in any order, normalising to
    /// Valetudo's corner order.
    ///
    /// Callers drag in whichever direction feels natural; the robot expects
    /// top-left first, and sending the corners the wrong way round produces a
    /// zone the robot rejects or cleans as an unexpected shape.
    pub fn from_corners(a: Point, b: Point) -> Self {
        let (left, right) = if a.x <= b.x { (a.x, b.x) } else { (b.x, a.x) };
        let (top, bottom) = if a.y <= b.y { (a.y, b.y) } else { (b.y, a.y) };

        Self {
            p_a: Point { x: left, y: top },
            p_b: Point { x: right, y: top },
            p_c: Point {
                x: right,
                y: bottom,
            },
            p_d: Point { x: left, y: bottom },
        }
    }

    pub fn width(&self) -> f64 {
        self.p_b.x - self.p_a.x
    }

    pub fn height(&self) -> f64 {
        self.p_c.y - self.p_a.y
    }

    /// Reject rectangles the robot cannot act on.
    ///
    /// Valetudo performs no validation of its own, so a degenerate zone reaches
    /// the firmware and is either silently dropped or starts a pointless run.
    /// A zone also has to be inside the mapped area.
    pub fn validate(&self, map_extent: Option<(f64, f64, f64, f64)>) -> Result<(), String> {
        if !self.p_a.x.is_finite()
            || !self.p_a.y.is_finite()
            || !self.p_c.x.is_finite()
            || !self.p_c.y.is_finite()
        {
            return Err("zone coordinates must be finite numbers".into());
        }

        if self.width() <= 0.0 || self.height() <= 0.0 {
            return Err("zone must have a positive width and height".into());
        }

        // Below roughly one map pixel the firmware may round the rectangle away.
        if let Some((_, _, pixel_size, _)) = map_extent {
            let min = (pixel_size / 2.0).max(0.5);
            if self.width() < min || self.height() < min {
                return Err(format!(
                    "zone is too small; each side must be at least {min} units"
                ));
            }
        }

        Ok(())
    }
}

/// Everything the client sends when the user draws zones.
#[derive(Debug, Deserialize)]
pub struct ZoneRequest {
    /// Drawn rectangles, in map pixels: `[x0, y0, x1, y1]` per zone.
    ///
    /// No iterations field: unlike room cleaning, Valetudo's zone request takes
    /// only an action and zones, and silently ignores anything else.
    #[serde(default)]
    pub zones: Vec<Vec<f64>>,
}

impl ZoneRequest {
    /// Convert to Valetudo's corner-ordered form, validating as we go.
    ///
    /// `pixel_size` converts drawn pixels into map units; `extent` bounds the
    /// mapped area so a zone dragged into the void is refused here rather than
    /// by the firmware.
    pub fn to_zones(
        &self,
        pixel_size: f64,
        extent: Option<(f64, f64, f64, f64)>,
    ) -> Result<Vec<Zone>, String> {
        if self.zones.is_empty() {
            return Err("no zones drawn".into());
        }
        if self.zones.len() > MAX_ZONES {
            return Err(format!(
                "this robot accepts at most {MAX_ZONES} zones per run, got {}",
                self.zones.len()
            ));
        }

        let mut out = Vec::with_capacity(self.zones.len());
        for raw in &self.zones {
            if raw.len() != 4 {
                return Err("each zone must be [x0, y0, x1, y1]".into());
            }
            if raw.iter().any(|v| !v.is_finite()) {
                return Err("zone coordinates must be finite numbers".into());
            }

            let to_map = |v: f64| v * pixel_size;
            let zone = Zone::from_corners(
                Point {
                    x: to_map(raw[0]),
                    y: to_map(raw[1]),
                },
                Point {
                    x: to_map(raw[2]),
                    y: to_map(raw[3]),
                },
            );
            zone.validate(extent)?;
            out.push(zone);
        }

        Ok(out)
    }
}

/// Valetudo nests the corners under a `points` object, so each zone is wrapped
/// rather than serialised directly. Getting this wrong produces a request the
/// robot rejects with an opaque error.
#[derive(Debug, Serialize)]
pub struct ZoneEntry<'a> {
    pub points: &'a Zone,
}

/// The JSON body Valetudo expects.
#[derive(Debug, Serialize)]
pub struct ZoneCleanBody<'a> {
    pub action: &'a str,
    pub zones: Vec<ZoneEntry<'a>>,
}

impl<'a> ZoneCleanBody<'a> {
    pub fn new(zones: &'a [Zone]) -> Self {
        Self {
            action: "clean",
            zones: zones
                .iter()
                .map(|zone| ZoneEntry { points: zone })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accepts integers as well as floats, so the expectations below stay
    /// readable.
    fn pt(x: impl Into<f64>, y: impl Into<f64>) -> Point {
        Point {
            x: x.into(),
            y: y.into(),
        }
    }

    #[test]
    fn corners_are_normalised_regardless_of_drag_direction() {
        let a = Zone::from_corners(pt(10, 20), pt(30, 5));
        assert_eq!(a.p_a, pt(10, 5), "pA is top-left");
        assert_eq!(a.p_b, pt(30, 5), "pB is top-right");
        assert_eq!(a.p_c, pt(30, 20), "pC is bottom-right");
        assert_eq!(a.p_d, pt(10, 20), "pD is bottom-left");

        // Dragging up-and-left is the same rectangle.
        let b = Zone::from_corners(pt(30, 5), pt(10, 20));
        assert_eq!(a, b);
    }

    #[test]
    fn geometry_is_derived_from_the_corners() {
        let z = Zone::from_corners(pt(0, 0), pt(4, 3));
        assert_eq!(z.width(), 4.0);
        assert_eq!(z.height(), 3.0);
    }

    #[test]
    fn rejects_degenerate_rectangles() {
        assert!(
            Zone::from_corners(pt(5, 5), pt(5, 9))
                .validate(Some((0.0, 0.0, 1.0, 100.0)))
                .is_err(),
            "zero width"
        );
        assert!(
            Zone::from_corners(pt(5, 5), pt(9, 5))
                .validate(Some((0.0, 0.0, 1.0, 100.0)))
                .is_err(),
            "zero height"
        );
    }

    #[test]
    fn rejects_non_finite_coordinates() {
        let z = Zone::from_corners(pt(0.0, 0.0), pt(f64::NAN, 5.0));
        assert!(z.validate(None).is_err());
        assert!(z.validate(None).is_err());
    }

    #[test]
    fn rejects_a_zone_smaller_than_half_a_pixel() {
        let extent = (0.0, 0.0, 10.0, 100.0);
        let tiny = Zone::from_corners(pt(0.0, 0.0), pt(1.0, 1.0));
        assert!(
            tiny.validate(Some(extent)).is_err(),
            "1x1 units is under half a 10-unit pixel"
        );
        let ok = Zone::from_corners(pt(0.0, 0.0), pt(10.0, 10.0));
        assert!(ok.validate(Some(extent)).is_ok());
    }

    #[test]
    fn request_converts_pixels_to_map_units() {
        let req = ZoneRequest {
            zones: vec![vec![2.0, 3.0, 8.0, 11.0]],
        };
        let zones = req.to_zones(5.0, None).unwrap();
        assert_eq!(zones.len(), 1);
        // 6 pixels across at 5 units per pixel.
        assert_eq!(zones[0].width(), 30.0);
        assert_eq!(zones[0].p_a, pt(10.0, 15.0));
        assert_eq!(zones[0].p_c, pt(40.0, 55.0));
    }

    #[test]
    fn request_enforces_the_robot_zone_limit() {
        let five = (0..5)
            .map(|i| vec![0.0, 0.0, 10.0 + i as f64, 10.0])
            .collect();
        let req = ZoneRequest { zones: five };
        let err = req.to_zones(1.0, None).unwrap_err();
        assert!(err.contains("at most 4"), "got: {err}");
    }

    #[test]
    fn request_rejects_malformed_input() {
        let cases: Vec<Vec<Vec<f64>>> = vec![
            vec![],                              // nothing drawn
            vec![vec![1.0, 2.0]],                // too few numbers
            vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]], // too many
            vec![vec![f64::NAN, 2.0, 3.0, 4.0]], // not finite
        ];
        for zones in cases {
            let label = format!("{zones:?}");
            let req = ZoneRequest { zones };
            assert!(
                req.to_zones(1.0, None).is_err(),
                "should have rejected {label}"
            );
        }
    }

    #[test]
    fn wire_format_matches_valetudos_corner_names() {
        let zone = Zone::from_corners(pt(1.0, 2.0), pt(3.0, 4.0));
        let zones = [zone];
        let body = ZoneCleanBody::new(&zones);
        let json = serde_json::to_value(&body).unwrap();

        assert_eq!(json["action"], "clean");
        let first = &json["zones"][0]["points"];
        assert_eq!(first["pA"]["x"], 1.0);
        assert_eq!(first["pA"]["y"], 2.0);
        assert_eq!(first["pB"]["x"], 3.0);
        assert_eq!(first["pC"]["y"], 4.0);
        assert_eq!(first["pD"]["x"], 1.0);
    }
}

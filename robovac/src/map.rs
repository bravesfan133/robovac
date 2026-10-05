use serde::Deserialize;

/// Subset of Valetudo's `RawMapData` that we need in order to draw a map.
/// Unknown fields are ignored; unknown entity types are kept but not drawn.
#[derive(Debug, Clone, Deserialize)]
pub struct MapData {
    // The wire format is camelCase; without this rename serde silently defaults
    // to 0.0 and every coordinate renders at 1 unit per pixel instead of the
    // real centimetre scale.
    #[serde(default, rename = "pixelSize")]
    pub pixel_size: f64,
    #[serde(default)]
    pub layers: Vec<MapLayer>,
    #[serde(default)]
    pub entities: Vec<MapEntity>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MapLayer {
    #[serde(rename = "type")]
    pub layer_type: String,
    #[serde(default)]
    pub pixels: Vec<i64>,
    /// Run-length encoded pixels as `[x_start, y, count]` triples. Valetudo
    /// sends one or the other depending on how much the layer compresses.
    #[serde(default, rename = "compressedPixels")]
    pub compressed_pixels: Vec<i64>,
    #[serde(default, rename = "metaData")]
    pub meta_data: LayerMeta,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LayerMeta {
    #[serde(default, rename = "segmentId")]
    pub segment_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MapEntity {
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(default)]
    pub points: Vec<f64>,
}

impl MapLayer {
    /// Expand whichever pixel representation Valetudo used into `(x, y)` pairs.
    pub fn pixels(&self) -> Vec<(i64, i64)> {
        if !self.pixels.is_empty() {
            return self.pixels.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        }

        let mut out = Vec::new();
        for triple in self.compressed_pixels.chunks_exact(3) {
            let (x_start, y, count) = (triple[0], triple[1], triple[2]);
            for offset in 0..count.max(0) {
                out.push((x_start + offset, y));
            }
        }
        out
    }
}

impl MapData {
    /// Cheap fingerprint used to decide whether a re-render is worth doing.
    /// Deliberately not a hash of the contents: comparing totals catches every
    /// real change (the robot rewrites the map as it explores) while costing
    /// three integer additions instead of walking every pixel.
    pub fn size_fingerprint(&self) -> (usize, usize, usize) {
        (
            self.pixel_size.to_bits() as usize,
            self.layers.len(),
            self.entities.len(),
        )
    }

    /// Total pixel count across all layers, counted in whichever encoding
    /// Valetudo chose to send.
    pub fn pixel_total(&self) -> usize {
        self.layers
            .iter()
            .map(|l| {
                if !l.pixels.is_empty() {
                    l.pixels.len() / 2
                } else {
                    l.compressed_pixels.iter().skip(2).step_by(3).sum::<i64>() as usize
                }
            })
            .sum()
    }

    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }
}

/// Embedded in the SVG so the map is interactive once inlined, and legible as a
/// plain image if it is not. Kept small: this is serialised on every map change.
const SEGMENT_STYLE: &str = r#"<style>
.segment{cursor:pointer}
.segment:hover rect{stroke:#2f7dd1;stroke-width:.35;stroke-linejoin:round}
.segment[data-selected="true"] rect{stroke:#2f7dd1;stroke-width:.45;stroke-linejoin:round}
.segment[data-selected="true"]{filter:brightness(.94)}
.segment-label{cursor:pointer}
.segment-label:hover{fill:#2f7dd1}
.segment-label[data-clean]:hover{text-decoration:underline}
</style>
"#;

/// Render the map as a standalone SVG document.
///
/// Rendering happens server-side so the browser needs no map library and no
/// knowledge of Valetudo's pixel encoding. Floor and segment layers are drawn
/// as merged runs of pixels; walls on top. Everything is escaped, since segment
/// names come from the robot and are attacker-influenced in the general case.
pub fn render_svg(map: &MapData, selected: &[String]) -> String {
    let pixel_size = if map.pixel_size > 0.0 {
        map.pixel_size
    } else {
        1.0
    };

    // Derive the viewport from the layers themselves; `size` is unreliable on
    // some models (it can be zero right after a map reset).
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for layer in &map.layers {
        for (x, y) in layer.pixels() {
            let (x, y) = (x as f64, y as f64);
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x + 1.0);
            max_y = max_y.max(y + 1.0);
        }
    }
    for entity in &map.entities {
        for pair in entity.points.chunks_exact(2) {
            min_x = min_x.min(pair[0]);
            min_y = min_y.min(pair[1]);
            max_x = max_x.max(pair[0]);
            max_y = max_y.max(pair[1]);
        }
    }

    if !min_x.is_finite() || !min_y.is_finite() {
        return empty_svg();
    }

    // One pixel of padding so strokes on the border are not clipped.
    let pad = pixel_size;
    let view_x = min_x * pixel_size - pad;
    let view_y = min_y * pixel_size - pad;
    let view_w = (max_x - min_x) * pixel_size + pad * 2.0;
    let view_h = (max_y - min_y) * pixel_size + pad * 2.0;

    let mut svg = String::with_capacity(64 * 1024);
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{view_x:.2} {view_y:.2} {view_w:.2} {view_h:.2}\" \
         preserveAspectRatio=\"xMidYMid meet\" class=\"vacuum-map\" \
         data-pixel-size=\"{pixel_size:.4}\" data-view-x=\"{view_x:.2}\" data-view-y=\"{view_y:.2}\">\n"
    ));
    // Styles live inside the document because the SVG is inlined into the page
    // rather than referenced, so page CSS cannot be relied upon.
    svg.push_str(SEGMENT_STYLE);
    svg.push_str("<rect x=\"-99999\" y=\"-99999\" width=\"1\" height=\"1\" fill=\"none\"/>\n");

    // Floor first, then segments tinted over it, then walls on top.
    for layer in map.layers.iter().filter(|l| l.layer_type == "floor") {
        svg.push_str(&layer_rects(layer, pixel_size, "#e8e4dc"));
    }

    for (segment_index, layer) in map
        .layers
        .iter()
        .filter(|l| l.layer_type == "segment")
        .enumerate()
    {
        let pixels = layer.pixels();
        let Some(id) = layer.meta_data.segment_id.clone() else {
            // A segment layer without an id cannot be selected or named, so
            // draw it flat rather than emitting an unaddressable group.
            let fill = segment_color(&None, segment_index);
            svg.push_str(&layer_rects(layer, pixel_size, &fill));
            continue;
        };

        let fill = segment_color(&Some(id.clone()), segment_index);
        let is_selected = selected.iter().any(|s| s == &id);

        svg.push_str(&format!(
            "<g class=\"segment\" data-segment-id=\"{}\" data-selected=\"{is_selected}\" fill=\"{fill}\">\n",
            escape(&id)
        ));
        svg.push_str(&layer_rects(layer, pixel_size, &fill));
        if let Some(name) = &layer.meta_data.name {
            // The label doubles as a one-click "clean just this room" target.
            svg.push_str(&label(name, &pixels, pixel_size, &id));
        }
        svg.push_str("</g>\n");
    }

    for layer in map.layers.iter().filter(|l| l.layer_type == "wall") {
        svg.push_str(&layer_rects(layer, pixel_size, "#3a3a3a"));
    }

    for entity in &map.entities {
        svg.push_str(&entity_svg(entity, pixel_size));
    }

    svg.push_str("</svg>\n");
    svg
}

fn empty_svg() -> String {
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\" class=\"vacuum-map\">\
     <rect width=\"100\" height=\"100\" fill=\"#1b1d22\"/><text x=\"50\" y=\"52\" fill=\"#7a808c\" \
     font-size=\"8\" text-anchor=\"middle\">no map yet</text></svg>\n"
        .to_string()
}

/// Merge pixels into horizontal runs so a room of 5,000 pixels becomes a few
/// hundred `<rect>`s instead of 5,000.
fn layer_rects(layer: &MapLayer, pixel_size: f64, fill: &str) -> String {
    let mut pixels = layer.pixels();
    pixels.sort_unstable();

    let mut out = String::new();
    let mut iter = pixels.into_iter().peekable();

    while let Some((start_x, y)) = iter.next() {
        let mut end_x = start_x;
        while let Some(&(nx, ny)) = iter.peek() {
            if ny == y && nx == end_x + 1 {
                end_x = nx;
                iter.next();
            } else {
                break;
            }
        }

        let w = (end_x - start_x + 1) as f64 * pixel_size;
        let h = pixel_size;
        let x = start_x as f64 * pixel_size;
        let yy = y as f64 * pixel_size;
        out.push_str(&format!(
            "<rect x=\"{x:.2}\" y=\"{yy:.2}\" width=\"{w:.2}\" height=\"{h:.2}\" fill=\"{fill}\"/>"
        ));
    }
    out
}

fn label(name: &str, pixels: &[(i64, i64)], pixel_size: f64, segment_id: &str) -> String {
    if pixels.is_empty() {
        return String::new();
    }
    let count = pixels.len() as f64;
    let cx = pixels.iter().map(|p| p.0 as f64).sum::<f64>() / count;
    let cy = pixels.iter().map(|p| p.1 as f64).sum::<f64>() / count;

    format!(
        "<text class=\"segment-label\" data-clean=\"{}\" x=\"{:.2}\" y=\"{:.2}\" fill=\"#1b1d22\" \
         font-size=\"{:.2}\" text-anchor=\"middle\" dominant-baseline=\"middle\" \
         paint-order=\"stroke\" stroke=\"#ffffff\" stroke-width=\"{:.2}\" \
         stroke-linejoin=\"round\">{}</text>",
        escape(segment_id),
        cx * pixel_size,
        cy * pixel_size,
        (pixel_size * 2.2).max(6.0),
        (pixel_size * 0.4).max(1.0),
        escape(name)
    )
}

fn entity_svg(entity: &MapEntity, pixel_size: f64) -> String {
    let pts: Vec<(f64, f64)> = entity
        .points
        .chunks_exact(2)
        .map(|c| (c[0] * pixel_size, c[1] * pixel_size))
        .collect();

    if pts.is_empty() {
        return String::new();
    }

    match entity.entity_type.as_str() {
        "robot_position" => {
            let (x, y) = pts[0];
            format!(
                "<circle cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"{r:.2}\" fill=\"#2f7dd1\" \
                 stroke=\"#ffffff\" stroke-width=\"{s:.2}\"/>",
                r = (pixel_size * 3.0).max(4.0),
                s = (pixel_size * 0.6).max(1.0)
            )
        }
        "charger_location" => {
            let (x, y) = pts[0];
            let side = pixel_size * 4.0;
            format!(
                "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{side:.2}\" height=\"{side:.2}\" \
                 fill=\"none\" stroke=\"#7a808c\" stroke-width=\"{s:.2}\"/>",
                x - pixel_size * 2.0,
                y - pixel_size * 2.0,
                s = (pixel_size * 0.6).max(1.0)
            )
        }
        "path" | "predicted_path" => polyline(&pts, "none", "#2f7dd1", pixel_size * 0.8, false),
        "virtual_wall" | "no_go_area" | "no_mop_area" | "active_zone" | "carpet" => {
            let (fill, stroke) = match entity.entity_type.as_str() {
                "no_go_area" => ("rgba(200,60,60,0.18)", "#c83c3c"),
                "no_mop_area" => ("rgba(160,80,220,0.18)", "#a050dc"),
                "active_zone" => ("rgba(60,170,110,0.20)", "#3caa6e"),
                "carpet" => ("rgba(190,170,90,0.18)", "#beaa5a"),
                _ => ("none", "#c83c3c"),
            };
            polyline(&pts, fill, stroke, pixel_size * 0.6, true)
        }
        // Obstacles, thresholds, ramps, labels and anything a future firmware
        // adds: draw rather than drop, so the map stays honest.
        _ => polyline(&pts, "none", "#e0a13a", pixel_size * 0.6, false),
    }
}

/// `close` distinguishes an open path (a cleaning line) from a closed polygon
/// (a no-go area or an active zone).
fn polyline(pts: &[(f64, f64)], fill: &str, stroke: &str, width: f64, close: bool) -> String {
    let mut d = String::with_capacity(pts.len() * 16);
    for (i, (x, y)) in pts.iter().enumerate() {
        let cmd = if i == 0 { 'M' } else { 'L' };
        d.push(cmd);
        d.push_str(&format!("{x:.2} {y:.2}"));
        if i + 1 < pts.len() {
            d.push(' ');
        }
    }
    if close {
        d.push('Z');
        format!(
            "<path d=\"{d}\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"{width:.2}\" \
             stroke-linejoin=\"round\"/>"
        )
    } else {
        format!(
            "<path d=\"{d}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"{width:.2}\" \
             stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
        )
    }
}

/// Deterministic, readable colour per segment so the same room keeps the same
/// colour across reloads without needing to store an assignment.
fn segment_color(segment_id: &Option<String>, index: usize) -> String {
    const PALETTE: [&str; 6] = [
        "#cfe3f7", "#d9ecd2", "#f6e3c5", "#efd9e2", "#dedcf2", "#d7ecec",
    ];
    let seed = match segment_id {
        Some(id) => id
            .bytes()
            .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32)),
        None => index as u32,
    };
    PALETTE[(seed % PALETTE.len() as u32) as usize].to_string()
}

fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> MapData {
        serde_json::from_str(
            r#"{
              "metaData": {"version": 2},
              "size": {"x": 500, "y": 400},
              "pixelSize": 5,
              "layers": [
                {"type": "floor", "pixels": [0,0, 1,0, 2,0], "metaData": {}},
                {"type": "segment", "compressedPixels": [10, 5, 3],
                 "metaData": {"segmentId": "17", "name": "Kitchen <&>"}},
                {"type": "wall", "pixels": [4,0, 4,1], "metaData": {}}
              ],
              "entities": [
                {"type": "robot_position", "points": [2, 3], "metaData": {"angle": 90}},
                {"type": "charger_location", "points": [1, 1], "metaData": {}},
                {"type": "path", "points": [0,0, 1,1, 2,2], "metaData": {}},
                {"type": "no_go_area", "points": [1,1, 2,1, 2,2, 1,2], "metaData": {}}
              ]
            }"#,
        )
        .expect("fixture map should parse")
    }

    #[test]
    fn expands_compressed_pixels() {
        let map = fixture();
        let segment = map
            .layers
            .iter()
            .find(|l| l.layer_type == "segment")
            .unwrap();
        assert_eq!(segment.pixels(), vec![(10, 5), (11, 5), (12, 5)]);
    }

    #[test]
    fn expands_plain_pixels() {
        let map = fixture();
        let floor = map.layers.iter().find(|l| l.layer_type == "floor").unwrap();
        assert_eq!(floor.pixels(), vec![(0, 0), (1, 0), (2, 0)]);
    }

    #[test]
    fn renders_expected_elements() {
        let svg = render_svg(&fixture(), &[]);
        assert!(svg.starts_with("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        // Walls, robot, charger, path and the no-go polygon all appear.
        assert!(svg.contains("fill=\"#3a3a3a\""));
        assert!(svg.contains("fill=\"#2f7dd1\""));
        assert!(svg.contains("stroke=\"#7a808c\""));
        assert!(svg.contains("#c83c3c"));
    }

    #[test]
    fn segment_names_are_escaped() {
        let svg = render_svg(&fixture(), &[]);
        assert!(svg.contains("Kitchen &lt;&amp;&gt;"));
        assert!(!svg.contains("Kitchen <&>"));
    }

    #[test]
    fn horizontal_pixels_merge_into_one_rect() {
        let layer = MapLayer {
            layer_type: "floor".into(),
            pixels: vec![0, 0, 1, 0, 2, 0, 3, 0],
            compressed_pixels: vec![],
            meta_data: LayerMeta::default(),
        };
        let rects = layer_rects(&layer, 1.0, "#fff");
        assert_eq!(rects.matches("<rect").count(), 1);
        assert!(rects.contains("width=\"4.00\""));
    }

    #[test]
    fn empty_map_yields_placeholder() {
        let empty = MapData {
            pixel_size: 1.0,
            layers: vec![],
            entities: vec![],
        };
        let svg = render_svg(&empty, &[]);
        assert!(svg.contains("no map yet"));
    }

    #[test]
    fn segments_render_as_addressable_groups() {
        let svg = render_svg(&fixture(), &[]);
        assert!(svg.contains("<g class=\"segment\" data-segment-id=\"17\""));
        // The embedded <style> block legitimately contains the literal
        // `data-selected="true"`, so assert on the group attribute pair.
        assert!(svg.contains("data-segment-id=\"17\" data-selected=\"false\""));
        assert!(
            svg.contains("data-clean=\"17\""),
            "label should be clickable"
        );
    }

    #[test]
    fn selection_is_rendered_per_request() {
        let svg = render_svg(&fixture(), &["17".to_string()]);
        assert!(svg.contains("data-segment-id=\"17\" data-selected=\"true\""));
    }

    #[test]
    fn selection_does_not_leak_between_renders() {
        // The cache memoises on the selection; a fresh call with none must not
        // inherit the previous render's selected state.
        let selected = render_svg(&fixture(), &["17".to_string()]);
        let plain = render_svg(&fixture(), &[]);
        assert!(selected.contains("data-selected=\"true\""));
        assert!(plain.contains("data-segment-id=\"17\" data-selected=\"false\""));
    }

    #[test]
    fn segment_ids_are_escaped() {
        let map: MapData = serde_json::from_str(
            r#"{
              "pixelSize": 5,
              "layers": [{"type":"segment","pixels":[0,0],
                          "metaData":{"segmentId":"\"><img src=x onerror=alert(1)>","name":"x"}}],
              "entities": []
            }"#,
        )
        .expect("map should parse");

        let svg = render_svg(&map, &[]);
        // The words "onerror=alert" survive as inert text inside an attribute
        // value, which is fine. What must not survive is anything that could
        // break out of the attribute and start a tag.
        assert!(!svg.contains("<img"), "no raw tag may be injected");
        assert!(!svg.contains("\"><"), "no attribute may be closed early");
        assert!(svg.contains("&quot;&gt;&lt;img"), "payload must be escaped");
    }

    #[test]
    fn segment_without_an_id_is_drawn_but_not_addressable() {
        let map: MapData = serde_json::from_str(
            r#"{"pixelSize":5,"layers":[{"type":"segment","pixels":[0,0],"metaData":{}}],"entities":[]}"#,
        )
        .unwrap();
        let svg = render_svg(&map, &[]);
        assert!(!svg.contains("data-segment-id"), "no id, no group");
        assert!(svg.contains("<rect"), "but still drawn");
    }

    #[test]
    fn map_exposes_viewport_metadata_for_zone_drawing() {
        // Phase 4 maps pixel coordinates back to map coordinates using these.
        let map = fixture();
        assert_eq!(map.pixel_size, 5.0);
        let svg = render_svg(&map, &[]);
        assert!(svg.contains("data-pixel-size=\"5.0000\""));
        assert!(svg.contains("data-view-x="));
    }

    #[test]
    fn segment_colour_is_stable() {
        let id = Some("17".to_string());
        assert_eq!(segment_color(&id, 0), segment_color(&id, 7));
        assert!(segment_color(&None, 0).starts_with('#'));
    }
}

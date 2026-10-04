#!/usr/bin/env node
// Minimal stand-in for Valetudo's API v2, so the UI can be developed and
// smoke-tested before the vacuum is rooted.
//
//   node scripts/fake-valetudo.mjs [port]
//
// Serves just enough of the surface that robovac uses, with a plausible map.

import { createServer } from "node:http";

const port = Number(process.argv[2] ?? 8081);

const robot = {
  status: "docked",
  battery: 100,
  dockStatus: "idle",
  fanSpeed: "medium",
  operationMode: "vacuum_and_mop",
  waterGrade: "medium",
};

// A 6x4 grid of floor pixels with a wall line and two segments.
const layers = [
  { type: "floor", pixels: [], metaData: {} },
  { type: "segment", compressedPixels: [], metaData: { segmentId: "1", name: "Kitchen" } },
  { type: "segment", compressedPixels: [], metaData: { segmentId: "2", name: "Hall" } },
  { type: "wall", pixels: [], metaData: {} },
];

for (let y = 0; y < 4; y++) {
  for (let x = 0; x < 6; x++) {
    if (x === 5) continue;
    layers[0].pixels.push(x, y);
    if (x < 3) {
      layers[1].compressedPixels.push(x, y, 1);
    } else {
      layers[2].compressedPixels.push(x, y, 1);
    }
  }
}
for (let y = 0; y < 4; y++) {
  layers[3].pixels.push(5, y);
}

const map = {
  metaData: { version: 2 },
  size: { x: 300, y: 200 },
  pixelSize: 50,
  layers,
  entities: [
    { type: "charger_location", points: [0, 0], metaData: {} },
    { type: "robot_position", points: [2, 1], metaData: { angle: 90 } },
    { type: "path", points: [2, 1, 3, 2, 4, 1], metaData: {} },
    { type: "no_go_area", points: [1, 2, 2, 2, 2, 3, 1, 3], metaData: {} },
  ],
};

const consumables = [
  { __class: "ValetudoConsumable", metaData: {}, type: "brush", subType: "main", remaining: { value: 182000, unit: "minutes" } },
  { __class: "ValetudoConsumable", metaData: {}, type: "brush", subType: "side_right", remaining: { value: 41000, unit: "minutes" } },
  { __class: "ValetudoConsumable", metaData: {}, type: "filter", subType: "none", remaining: { value: 78, unit: "percent" } },
  { __class: "ValetudoConsumable", metaData: {}, type: "mop", subType: "none", remaining: { value: 61, unit: "percent" } },
];

const segments = [
  { id: "1", name: "Kitchen" },
  { id: "2", name: "Hall" },
];

const state = () => ({
  metaData: { version: 2 },
  attributes: [
    { __class: "StatusStateAttribute", metaData: {}, value: robot.status, flag: "none" },
    { __class: "BatteryStateAttribute", metaData: {}, level: robot.battery, flag: robot.dockStatus === "idle" ? "charging" : "discharging" },
    { __class: "PresetSelectionStateAttribute", metaData: {}, type: "fan_speed", value: robot.fanSpeed },
    { __class: "PresetSelectionStateAttribute", metaData: {}, type: "operation_mode", value: robot.operationMode },
    { __class: "PresetSelectionStateAttribute", metaData: {}, type: "water_grade", value: robot.waterGrade },
    { __class: "DockStatusStateAttribute", metaData: {}, value: robot.dockStatus },
  ],
  map,
});

const json = (res, body, code = 200) => {
  const payload = JSON.stringify(body);
  res.writeHead(code, { "content-type": "application/json", "content-length": Buffer.byteLength(payload) });
  res.end(payload);
};

createServer((req, res) => {
  const url = new URL(req.url, "http://localhost");
  const path = url.pathname;

  // Consume the body so keep-alive stays in sync.
  req.resume();

  if (path === "/api/v2/robot") {
    return json(res, { manufacturer: "Dreame", modelName: "L40 Ultra", implementation: "DreameL40UltraValetudoRobot" });
  }
  if (path === "/api/v2/robot/state") {
    return json(res, state());
  }
  if (path === "/api/v2/robot/state/map") {
    return json(res, map);
  }
  if (path === "/api/v2/robot/capabilities") {
    return json(res, [
      { type: "BasicControlCapability" },
      { type: "BatteryStateAttribute" },
      { type: "ConsumableMonitoringCapability" },
      { type: "MapSegmentationCapability" },
      { type: "FanSpeedControlCapability" },
      { type: "DuststreamingCapability" },
    ]);
  }
  if (path === "/api/v2/robot/capabilities/ConsumableMonitoringCapability") {
    return json(res, consumables);
  }
  if (path === "/api/v2/robot/capabilities/MapSegmentationCapability") {
    return json(res, segments);
  }
  if (path === "/api/v2/robot/capabilities/FanSpeedControlCapability/presets") {
    return json(res, ["quiet", "medium", "turbo", "max"]);
  }
  if (path === "/api/v2/robot/capabilities/DuststreamingCapability/properties") {
    return json(res, { width: 640, height: 480, duststreamerInstalled: false });
  }
  if (path === "/api/v2/robot/capabilities/FanSpeedControlCapability/preset") {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      try {
        robot.fanSpeed = JSON.parse(body || "{}").name ?? robot.fanSpeed;
      } catch {
        /* ignore */
      }
      json(res, {});
    });
    return;
  }
  if (path === "/api/v2/robot/capabilities/MapSegmentationCapability" && req.method === "PUT") {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      robot.status = "cleaning";
      json(res, {});
    });
    return;
  }
  if (path === "/api/v2/robot/capabilities/BasicControlCapability") {
    const action = robot.status;
    if (req.method === "PUT") {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        try {
          const { action: a } = JSON.parse(body || "{}");
          robot.status = a === "home" ? "returning" : a === "start" ? "cleaning" : "idle";
        } catch {
          /* ignore */
        }
        json(res, {});
      });
      return;
    }
    return json(res, { action });
  }

  res.writeHead(404, { "content-type": "text/plain" });
  res.end("not found");
}).listen(port, () => {
  console.log(`fake Valetudo listening on http://127.0.0.1:${port}`);
});
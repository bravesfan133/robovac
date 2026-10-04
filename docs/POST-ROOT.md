# After the root: bringing up the stack

Assumes Valetudo is installed and the robot is on your LAN at `192.0.2.46`.

## 1. Confirm Valetudo is healthy

```sh
curl -s http://192.0.2.46/api/v2/robot | jq
curl -s http://192.0.2.46/healthz 2>/dev/null || true
```

You should see `DreameL40UltraValetudoRobot`. The raw camera stream is at
`/api/v2/robot/capabilities/DuststreamingCapability/stream` (MPEG-TS) but must be
enabled in Valetudo's UI first; `properties` tells you whether a streamer is
installed.

## 2. Bring up the containers

```sh
cp .env.example .env
$EDITOR .env          # VALETUDO_URL, MOSQUITTO_HOST (your host's LAN IP, no port)
docker compose up -d --build
curl -s localhost:8080/healthz | jq
```

`MOSQUITTO_HOST` must be your host's LAN address, not `127.0.0.1`, or the vacuum
cannot reach the broker. It is host-only; the port comes from `MOSQUITTO_PORT`
(default 1883). To check the file parses before handing it to a GUI:

```sh
./scripts/check-compose.sh
```

On macOS Docker Desktop, `network_mode: host` does not behave like Linux; test
the UI through the published port or run the stack on the real server.

## 3. Wire MQTT in Valetudo

In Valetudo: Settings → Connectivity → MQTT. Point it at your host's LAN
address, port 1883. Enable Home Assistant discovery if you want it — harmless
even without Home Assistant, and it makes the broker self-describing.

Note the **base topic** shown in that page. It is the robot id you will see in
MQTT topics.

## 4. Expose it over Tailscale

No reverse proxy container needed; Tailscale terminates TLS and your tailnet ACL
is the access control:

```sh
tailscale serve --bg --https=443 http://127.0.0.1:8080
```

That gives `https://<host>.<tailnet>.ts.net`. Valetudo's own
`blockExternalAccess: true` keeps the robot reachable only from your LAN, so it
needs no configuration change at all.

## 5. Lock the robot down

The L40 has a camera, so treat this seriously.

- Turn on Valetudo's `webserver.basicAuth` and set `VALETUDO_USERNAME` /
  `VALETUDO_PASSWORD` in `.env`.
- Set `WEB_USERNAME` / `WEB_PASSWORD` for a second layer in front of the UI.
- Switch the vacuum's SSH to public-key auth and disable password login.
- Keep the camera disabled unless you are actively using it. Enabling it opens
  an unauthenticated video path on the LAN.
- Do not port-forward anything on the router. Tailscale or nothing.

## 6. HomeKit

Install `homebridge-valetudo-l40` into your existing Homebridge:

```sh
cd homebridge-valetudo-l40
npm install
```

Then in Homebridge's `config.json`:

```json
{
  "accessories": [
    {
      "accessory": "Valetudo",
      "name": "Vacuum",
      "host": "192.0.2.46",
      "username": "",
      "password": "",
      "refreshInterval": 30,
      "exposeDockTriggers": true
    }
  ]
}
```

You get power, running state, a fan-speed selector, battery, percentage
consumables as their own sensors, and switches for auto-empty / mop wash / mop
dry.

What you will **not** get in the Home app: the floor plan, obstacles, room
segments, zones. HomeKit has no way to represent them. That is what the robovac
UI is for — bookmark it in Home as a Safari web app and it behaves like a native
app tile.

## 7. Firmware updates

The rooted robot will never take an OTA update through Dreame's cloud. Updating
means installing another rooted image:

1. Build a new firmware at dustbuilder with **"for manual installation using SSH"**.
2. Copy it to the robot.
3. Run the bundled `install.sh` — twice, since the device has A/B slots.

The robot must be docked and charging. Upstream notes that firmware on supported
bots has only ever improved, so it is worth doing occasionally; the newest
rootable build is whatever dustbuilder lists, which may be *older* than what the
Dreame app is offering.

## 8. Multi-floor, if you ever need it

Upstream Valetudo is single-map on Dreame — `MapSnapshotCapability` is
Roborock-only. Persistent maps keep your existing map across re-runs, which is
enough for one floor. For a second floor:

- [`pkoehlers/maploader`](https://github.com/pkoehlers/maploader) over MQTT:
  save and restore the map folder, then restart Valetudo.
- Or the [`Algid/Valetudo-plus`](https://github.com/Algid/Valetudo-plus) fork,
  which adds multi-map, map rotation and segment ordering, and is tested on the
  L40 Ultra.

Only fork if you need it; upstream is better tested.
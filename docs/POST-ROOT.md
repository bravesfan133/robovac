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

## 2. Give the vacuum a hostname

Use a name rather than an IP. A DHCP lease can move, and the vacuum's address is
not something to hardcode in a config file.

**Option 1 — mDNS, nothing to configure.** Valetudo advertises itself over
Bonjour as `valetudo-<robot-id>.local`, publishing both an `_http` and a
`_valetudo` service. The exact name is printed in Valetudo's log output at
startup:

```
INFO ... Valetudo can be reached via: valetudo-dreame_vacuum_r2492b.local
```

So:

```sh
VALETUDO_URL=http://valetudo-dreame_vacuum_r2492b.local
```

**Option 2 — DHCP reservation plus a DNS record. Most reliable.** In your
router, reserve the vacuum's current address and give it a local DNS name such
as `vacuum`. Then:

```sh
VALETUDO_URL=http://vacuum
```

Prefer this over mDNS when the vacuum and the server are on different subnets or
VLANs, or if your access point or switch filters multicast — mDNS is UDP 5353 and
is easy to break accidentally, while DNS is not.

**Option 3 — Tailscale.** If you install Tailscale on the vacuum itself (see
the firewall notes below), its MagicDNS name works too, and
`VALETUDO_URL=http://vacuum.your-tailnet.ts.net`.

Whichever you pick, confirm it resolves *from the server*, not from your laptop:

```sh
getent hosts valetudo-dreame_vacuum_r2492b.local   # or: dig +short vacuum
curl -s http://vacuum/api/v2/robot | jq
```

If that works from a shell on the server it will work from the container, because
`network_mode: host` means the container shares the host's resolver.

## 3. Bring up the containers

```sh
cp .env.example .env
$EDITOR .env          # VALETUDO_URL is the only required value
docker compose up -d --build
curl -s localhost:8080/healthz | jq   # process is up
curl -s localhost:8080/readyz  | jq   # 503 until the robot answers
```

`/healthz` is deliberately independent of the robot. The vacuum is off the
network, asleep or unrooted for long stretches, and a healthcheck that reported
unhealthy then would just invite the orchestrator to restart something that is
fine. Use `/readyz` when you want to know the robot is actually reachable.

Validate before handing the file to a GUI, since Compose's interpolation errors
are terse:

```sh
./scripts/check-compose.sh
```

If you deploy through a manager UI rather than the CLI, it will not read `.env`
from disk — set `VALETUDO_URL` in its own environment panel instead. On macOS
Docker Desktop, `network_mode: host` does not behave like Linux; test the UI
through the published port or run the stack on the real server.

## 4. MQTT (only if you need it)

**Skip this unless you are adding maploader or Home Assistant.** robovac does not
use MQTT: it polls Valetudo over HTTP and streams updates to the browser over
SSE. Starting the stack without a broker is the supported configuration.

When you do want one — for multi-floor map switching, for instance — bring it up
as an overlay:

```sh
$EDITOR .env            # uncomment MOSQUITTO_HOST, set it to this host's LAN IP
docker compose -f docker-compose.yml -f docker-compose.mqtt.yml up -d
```

Then in Valetudo: Settings → Connectivity → MQTT, pointing at that address and
port 1883. Note the **base topic** shown there; it is the robot id that appears
in MQTT topics.

Security note: the bundled broker allows anonymous clients, which is fine on an
isolated LAN and wrong anywhere else. See `deploy/mosquitto/mosquitto.conf` for
adding a password file and ACLs.

## 5. Expose it over Tailscale

No reverse proxy container needed; Tailscale terminates TLS and your tailnet ACL
is the access control:

```sh
tailscale serve --bg --https=443 http://127.0.0.1:8080
```

That gives `https://<host>.<tailnet>.ts.net`. Valetudo's own
`blockExternalAccess: true` keeps the robot reachable only from your LAN, so it
needs no configuration change at all.

## 6. Lock the robot down

The L40 has a camera, so treat this seriously.

- Turn on Valetudo's `webserver.basicAuth` and set `VALETUDO_USERNAME` /
  `VALETUDO_PASSWORD` in `.env`.
- Set `WEB_USERNAME` / `WEB_PASSWORD` for a second layer in front of the UI.
- Switch the vacuum's SSH to public-key auth and disable password login.
- Keep the camera disabled unless you are actively using it. Enabling it opens
  an unauthenticated video path on the LAN.
- Do not port-forward anything on the router. Tailscale or nothing.

## 7. HomeKit

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

## 8. Firmware updates

The rooted robot will never take an OTA update through Dreame's cloud. Updating
means installing another rooted image:

1. Build a new firmware at dustbuilder with **"for manual installation using SSH"**.
2. Copy it to the robot.
3. Run the bundled `install.sh` — twice, since the device has A/B slots.

The robot must be docked and charging. Upstream notes that firmware on supported
bots has only ever improved, so it is worth doing occasionally; the newest
rootable build is whatever dustbuilder lists, which may be *older* than what the
Dreame app is offering.

## 9. Multi-floor, if you ever need it

Upstream Valetudo is single-map on Dreame — `MapSnapshotCapability` is
Roborock-only. Persistent maps keep your existing map across re-runs, which is
enough for one floor. For a second floor:

- [`pkoehlers/maploader`](https://github.com/pkoehlers/maploader) over MQTT:
  save and restore the map folder, then restart Valetudo.
- Or the [`Algid/Valetudo-plus`](https://github.com/Algid/Valetudo-plus) fork,
  which adds multi-map, map rotation and segment ordering, and is tested on the
  L40 Ultra.

Only fork if you need it; upstream is better tested.
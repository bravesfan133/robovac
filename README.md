# Robovac

A small, self-hosted control interface for a Dreame L40 Ultra that has been
rooted with [Valetudo](https://valetudo.cloud) — which permanently removes the
Dreame app and Dreame's cloud from the loop.

Three pieces:

| | |
|---|---|
| `robovac/` | Rust/axum web UI. Talks to Valetudo's REST API v2 and renders the floor plan server-side as SVG, so the browser needs no map library. |
| `homebridge-valetudo-l40/` | HomeKit bridge, for the subset of vacuum control HomeKit can actually model. |
| `scripts/fake-valetudo.mjs` | Stand-in for Valetudo's API so both of the above can be developed and tested before the vacuum is rooted. |

Plus `docs/`, which is the rooting runbook: `PREFLIGHT.md`, `WIRING.md`,
`ROOTING.md`, `POST-ROOT.md`.

**This repo is public, so it contains no hardware identifiers.** Your unit's
serial, MAC and LAN address belong in `local/unit.md`, which is gitignored. The
docs use RFC 5737 documentation addresses (`192.0.2.x`) throughout; substitute
your own in a local copy or in `.env`.

## Why not just use Valetudo's own UI

You can, and you should — it ships with the root and it is good. This exists
because:

- it needs no Home Assistant, and this stack is a ~40MB Rust binary plus a
  MQTT broker;
- the map renders server-side from Valetudo's raw pixel data, so there is no
  JavaScript map engine to download;
- the camera stream is proxied, so the browser only needs to reach one host;
- it is a place to add the things HomeKit cannot express.

## Development

Everything runs against the fake, no vacuum required.

```sh
# terminal 1
node scripts/fake-valetudo.mjs 8081

# terminal 2 — UI
cd robovac
VALETUDO_URL=http://127.0.0.1:8081 cargo run
# http://127.0.0.1:8080

# terminal 2 — Homebridge plugin
cd homebridge-valetudo-l40 && npm install && node test/smoke.js
```

Tests:

```sh
cd robovac && cargo test
```

93 tests: unit coverage for the map renderer, coordinate conversion, SSE parser,
byte cache and auth, plus HTTP-level tests that build the real router and assert
routing, status codes and rendered output. No Node and no network required.

### Against the real thing

Valetudo ships a mock robot implementation, which is useful for exercising the
real API shapes:

```sh
git clone https://github.com/Hypfer/Valetudo
cd Valetudo && npm install && npm run generate_code --workspace=backend
npm run start:dev --workspace=backend   # exits; it expects a config
```

Then point `VALETUDO_URL` at it with `robot.implementation` set to
`MockValetudoRobot` in the generated `local/valetudo/valetudo_config.json`.

## Deployment

```sh
cp .env.example .env && $EDITOR .env
docker compose up -d --build
tailscale serve --bg --https=443 http://127.0.0.1:8080
```

The image is built from source on the machine that runs it: no registry, no
publishing step, no prebuilt artefact to keep in sync. First build takes a couple
of minutes and needs roughly 2GB of scratch space for the Rust toolchain;
`docker compose build` caches that, so later rebuilds only recompile the crate.

Deploying through a Docker manager UI rather than the CLI? Note that:

- set the **compose file path** to `docker-compose.yml` — the usual default is
  `compose.yaml`, which does not exist here;
- environment variables go in the manager's own env panel, so no `.env` file is
  needed on disk. Only `VALETUDO_URL` is required;
- `network_mode: host` requires Linux; it is ignored on Docker Desktop.

See `docs/POST-ROOT.md` for MQTT, Tailscale and the security steps worth taking
when a camera is involved.

## Configuration

| Variable | Required | Meaning |
|---|---|---|
| `VALETUDO_URL` | yes | Where to reach the vacuum. Prefer a hostname over an IP — see `docs/POST-ROOT.md` §2 |
| `VALETUDO_USERNAME` / `VALETUDO_PASSWORD` | no | Valetudo's basic auth |
| `WEB_USERNAME` / `WEB_PASSWORD` | no | A second auth layer in front of this UI. Both or neither. |
| `BIND` | no | Listen address, default `0.0.0.0:8080` |
| `POLL_INTERVAL_MS` | no | State poll interval, default 2000 |
| `RUST_LOG` | no | `info,robovac=debug` is useful |

## How it works

The design point worth knowing: **only a background poller contacts the robot.**

Valetudo has an expensive read and cheap ones. `GET /state` calls `pollState()`
and talks to the vacuum over miio, costing about a second. `GET /state/map` and
`GET /state/attributes` serve Valetudo's own cached copy for free. Every handler
here answers from an in-process cache that the poller fills, so a page load
costs zero robot round trips and renders instantly.

Map changes arrive over a single upstream event subscription, fanned out to
browsers. Valetudo caps that endpoint at five clients, so one per browser would
waste the budget; five browsers still produce exactly one upstream connection.

## Features

- Click a room on the map to select it, or click its name to clean just that
  room. Selection lives in the URL, so it survives a reload.
- Draw zones to clean a specific area, capped at 4 per run, which is this model's
  own limit.
- Live map and status with no page reload.
- Obstacle photos, loaded on demand and cached, since Valetudo rate-limits that
  endpoint hard.
- Failures are classified and explained rather than reported as "error sending
  request".
- Installable as an app; the shell works with no network.

## HTTP API

| Route | |
|---|---|
| `GET /` | Dashboard |
| `GET /healthz` | Liveness. Always 200 while the process serves; the container healthcheck uses this, so it never depends on the robot |
| `GET /readyz` | Readiness. 503 while the vacuum is unreachable |
| `GET /map.svg` | Floor plan, rendered server-side. `?segments=1,2` renders a selection |
| `GET /api/state` | Flattened state for the frontend |
| `GET /api/capabilities` | Raw capability list |
| `GET /api/obstacles` | Obstacles the robot reported |
| `GET /api/obstacles/image?id=` | One obstacle photo, proxied and cached |
| `GET /api/camera/properties` | Camera dimensions / streamer presence |
| `GET /api/camera/stream` | Proxied MPEG-TS stream |
| `GET /events` | SSE state updates |
| `POST /api/control/{start,stop,pause,home}` | Basic control |
| `POST /api/fan-speed` | `{"name":"turbo"}` |
| `POST /api/clean-segments` | `{"segment_ids":["1","2"],"iterations":1}` |
| `POST /api/clean-zones` | `{"zones":[[x0,y0,x1,y1]]}` in map pixels |
| `POST /api/obstacles/enabled` | `{"enabled":true}` |

## Troubleshooting

`docs/TROUBLESHOOTING.md` covers what each diagnostic means, how to confirm a
problem is on the server rather than the robot, and recovery after a failed
flash.

## Licence

MIT. Not affiliated with or endorsed by Dreame Technology or the Valetudo
project.
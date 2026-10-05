# Troubleshooting

## Which stage are you at?

| Symptom | Where to look |
|---|---|
| Nothing answers on the robot's port | Rooting, `ROOTING.md` |
| Valetudo installed but the UI says unreachable | Below |
| The UI works but has no map | [No map](#no-map) |
| The robot keeps dropping off Wi-Fi | [Wi-Fi](#wi-fi-keeps-dropping) |
| A flash failed partway | [Recovering](#recovering-from-a-failed-flash) |

## What the diagnostics mean

The banner at the top of the UI classifies the failure rather than repeating
"error sending request". Each cause means something different:

| Shown | Meaning | Do |
|---|---|---|
| `name not resolving` | The hostname does not resolve | Check the name. Valetudo advertises itself as `valetudo-<robot-id>.local`; the exact string is in Valetudo's log at startup. An IP also works. |
| `nothing listening` | The address is right, nothing is serving | Is the robot powered on **and rooted**? Stock firmware does not serve HTTP on port 80. |
| `host unreachable` | No answer at all | Wrong subnet, or the robot is on Wi-Fi and your server is not. |
| `timed out` | No answer in time | Usually the robot is mid-cleanup. This normally resolves itself. |
| `needs credentials` | Valetudo's basic auth is on | Set `VALETUDO_USERNAME` and `VALETUDO_PASSWORD` in `.env`. |
| `unexpected reply` | Valetudo answered with an error | Check Valetudo's own logs on the robot. |

Before Valetudo is installed, `nothing listening` is the *correct* answer. The
UI shows "connecting" rather than "offline" until the first successful contact,
so you can tell a fresh start from a broken one.

## Checks, in order

```sh
# 1. Is the robot on the network at all?
ping valetudo-dreame_vacuum_r2492b.local     # or the IP

# 2. Is Valetudo serving?
curl -s http://<robot>/api/v2/robot | jq
#    {"manufacturer":"Dreame","modelName":"L40 Ultra",...}

# 3. Can the *server* reach it? A laptop proving it works proves nothing.
curl -s http://<robot>/api/v2/robot | jq

# 4. Is the stack up?
curl -s localhost:8080/healthz | jq   # always 200 while the process runs
curl -s localhost:8080/readyz  | jq   # 503 until the robot answers
```

`/healthz` is deliberately independent of the robot: it reports the UI process,
not the vacuum. A container healthcheck that depended on the robot would sit
permanently unhealthy while the vacuum was off, and restarting would not fix it.

## No map

`map unavailable` in the map frame means Valetudo has not produced a map yet.

- The robot needs one completed mapping run. Watch for it in Valetudo's UI.
- A factory reset wipes the map; it rebuilds on the next run.
- Persistent maps need `PersistentMapControlCapability`, which the L40 Ultra
  supports. Confirm via `/api/v2/robot/capabilities` that it is listed.
- If `/api/v2/robot/state/map` returns layers but the UI shows nothing, it is a
  rendering problem rather than a robot one, and the raw response is the place to
  look.

## Wi-Fi keeps dropping

Known issue with some units. Clear the stored configuration and rejoin:

```sh
ssh root@192.168.5.1
rm -f /data/config/miio/wifi.conf /data/config/wifi/wpa_supplicant.conf \
      /var/run/wpa_supplicant.conf
dreame_release.na -c 9 -i ap_info -m " "
reboot
```

Then rejoin Wi-Fi through Valetudo rather than by other means.

## Valetudo cannot detect the robot

Units made around 08/2025 or later sometimes report a negative device id, which
miio does not expect:

```sh
cat /mnt/private/ULI/factory/did.txt        # negative?
mount -o remount,rw /mnt/private
cp /mnt/private/ULI/factory/did.txt /mnt/private/ULI/factory/did_orig.txt && sync
# edit did.txt so the number is positive
rm /data/config/miio/device.conf            # regenerated on boot
reboot
```

Back up `did_orig.txt` first; it is part of the robot's identity.

## Recovering from a failed flash

The good news: FEL lives in the SoC's mask ROM and is selected by the button
**before** the eMMC is read, so it works regardless of the state of the flash.

If a flash failed partway:

1. The likely cause is an eMMC write timeout, not your wiring. On the UART it
   reads `mmc 2 data timeout` and `cmd 25 STO`. It appears after roughly 80–100MB
   of writes in one session.
2. Power off (hold 15s), re-enter FEL, and write less per session — the rootfs in
   pieces, verifying each.
3. If the config value changed because the env was written, **build a new
   dustbuilder job**. Do not edit around the check.
4. The stage-1 samples decrypt back into stock firmware, so a full restore is
   possible if you kept them. If you did not keep them, that option is gone.

## The UI shows stale data

The map re-renders only when Valetudo reports the geometry changed, so a
refresh should always reflect reality. If the map looks frozen:

```sh
curl -s localhost:8080/api/state | jq '.map_version'
```

An unchanged value means Valetudo itself is not reporting map changes. Valetudo
only updates its map when something polls it; robovac polls every
`POLL_INTERVAL_MS`. If Valetudo's own UI is open in another tab, it may be
changing the map in ways this UI does not see.

## Blocking a room does nothing

Two different mechanisms: room selection uses `MapSegmentationCapability` with
segment ids; drawn zones use `ZoneCleaningCapability`. Both require the map to
exist. Zones are capped at 4 per run on this model, which is the robot's own
limit, not the UI's.

## Camera shows nothing

- Capture is **off by default on the robot**. Enable it under Obstacles.
- It must also be enabled in Valetudo's own camera settings; `duststreamerInstalled`
  in `/api/camera/properties` reports whether a streamer is installed.
- The stream is MPEG-TS proxied from the robot. If it works from Valetudo's own
  page but not here, the path being proxied has changed.
- Keep it off when not in use. This is a camera in your home.

## Confirming no cloud traffic

After rooting, the robot should not reach Dreame. Check:

- The robot's DNS resolution and outbound connections from your router or
  firewall logs
- Valetudo's `patch_dns` option was ticked in the dustbuilder build; without it
  the robot still phones home even though nothing depends on it

## Resetting the UI

The UI holds no state worth keeping; everything comes from the robot. To reset
it completely:

```sh
docker compose down
docker compose up -d --build
```

Removing the container does not touch the robot. The MQTT volumes, if you enabled
that overlay, are removed separately with `docker compose down -v`.
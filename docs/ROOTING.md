# Rooting runbook: Dreame L40 Ultra → Valetudo

Target unit: `dreame.vacuum.r2492b`, MAC `aa:bb:cc:dd:ee:ff`, LAN
`192.0.2.46`.

Host: the Omarchy MacBook (Linux). Google's `fastboot` works there, which it does
not on macOS — the FEL payload's USB gadget is `bDeviceClass 0xff` and macOS
never configures it.

Read `docs/PREFLIGHT.md` and `docs/WIRING.md` first.

## Phase 1 — Recon (read-only)

```sh
sudo pacman -S sunxi-tools android-tools
```

Enter FEL as described in `docs/WIRING.md`, then:

```sh
sunxi-fel ver              # must answer before anything else matters
```

Once in fastboot:

```sh
fastboot devices
fastboot getvar config     # MUST be first. Write the value down.
fastboot getvar dustversion
```

`config` is needed for every future firmware update. Note that
[Max Ammann's write-up](https://maxammann.org/posts/2025/06/dreame-fel-mode/)
documents the full set of vendor commands: `get_staged`, `oem stage1`,
`oem stage2`, `oem bko`, `oem upload`, `oem bypass`, `oem debug`.

### Samples

```sh
fastboot get_staged dustx100.bin   # ~400MB
fastboot oem stage1
fastboot get_staged dustx101.bin   # ~400MB
fastboot oem stage2
fastboot get_staged dustx102.bin   # ~400MB
```

Then:

```sh
zip dreame_r2492_samples.zip dustx100.bin dustx101.bin dustx102.bin   # ~1.2GB
```

**Copy this off the machine now.** It is an encrypted copy of the first ~1.2GB of
eMMC: both bootloaders, both firmware slots, the U-Boot env, and your calibration
in `private`/`misc`. Max Ammann's `dustdecrypt` work shows it also tends to
contain leftover photos of your flat, so keep it somewhere you trust.

If a session times out, just power off (hold power 15s), re-enter FEL and resume.

## Phase 2 — Firmware build

Submit at [builder.dontvacuum.me](https://builder.dontvacuum.me), `r2492` page:

- `config.txt` and `dreame_samples.zip`
- your serial number — **never fake it**
- `id_rsa.pub`
- tick **FEL image**
- tick **Patch DNS** and **preinstall tools**

`Patch DNS` is what permanently stops the robot talking to Dreame's servers.
`preinstall tools` bundles the helpers Valetudo expects.

If the build fails with `Error: invalid config value.`, there is no workaround:
upload your samples zip at
[check.builder.dontvacuum.me](https://check.builder.dontvacuum.me/) and wait for
the maintainer. Do not begin flashing until you have an image.

Download the job immediately — dustbuilder deletes jobs after a few days. You
want `dreame.vacuum.r2492_*_fel_ng.zip`, `md5.txt` and `_buildflags.sh`.

## Phase 3 — Flash

Re-enter FEL, this time using the `fsbl.bin` and `payload.bin` **from the job
zip**. Before flashing, run the recon's `getvar config` once more and keep the
UART capture running in a second terminal.

```sh
fastboot getvar config
fastboot oem dust <value from check.txt>   # must print OKAY
fastboot oem prep                         # must print OKAY
fastboot flash toc1 toc1.img               # must print OKAY
fastboot flash boot1 boot.img
fastboot flash rootfs1 rootfs.img
fastboot flash boot2 boot.img
fastboot flash rootfs2 rootfs.img
fastboot reboot
```

Any command that does not print `OKAY` — stop. `Invalid sparse file format at
header magic` is expected and harmless.

### If it fails: the eMMC timeout

A bare `FAIL` or a USB timeout partway through is almost always this, and it is
not your wiring. On the UART:

```
[mmc]: mmc 2 data timeout 0 status 14
[mmc]: smc 2 err, cmd 25,  STO
[mmc]: mmc write failed
```

The timeout shows up consistently after roughly 80–100MB of writes in a session.
After it, the card refuses reads too, so nothing else works in that session — not
`getvar config`, not `resume`. Power off, re-enter FEL, and write less per
session: the rootfs has to be split into pieces and written across several FEL
sessions, verifying each one.

Stripping HS400/HS200/DDR from the payload's device tree does *not* help — in
FEL mode the eMMC tuning walks its own list regardless.

Incremental approach worth taking: land `toc1`, `boot1` and `rootfs1`, verify,
reboot, and confirm a root shell on the serial console. Only then consider
`rootfs2`. Slot 2 can stay stock as a fallback, because `oem prep` already points
the robot at slot 1.

A successful flash shows on the UART:

```
login[...]: root login on 'ttyS0'
 Athena Linux (r2416_release)
built with dustbuilder (https://builder.dontvacuum.me)
[root@r2416_release:~]#
```

A root login on serial that stock firmware never provides.

### Two traps

- **A failed flash changes `getvar config`.** Once the env and the `toc1` backup
  are written, the robot reports a different config. Build a new dustbuilder job
  for the new config; do not edit around the check. Only `rootfs.img` and
  `payload.bin` differ between jobs — they carry your key and config.
- **`busybox stty -echo` segfaults the robot's shell.** Use `read -s` to pass a
  secret to it.

## Phase 4 — Valetudo

Robot Wi-Fi AP on: hold the two outer buttons for 3 seconds. Join it from the
Mac and:

```sh
ssh root@192.168.5.1
```

Back up calibration and identity **before** installing anything. This is the data
that cannot be regenerated if lost:

```sh
tar cvf /tmp/backup.tar /mnt/private/ /mnt/misc/
```

Pull it to your laptop, then install:

```sh
# transfer valetudo-aarch64 from the latest Valetudo release, named `valetudo`
mv /tmp/valetudo /data/valetudo
chmod +x /data/valetudo
cp /misc/_root_postboot.sh.tpl /data/_root_postboot.sh
chmod +x /data/_root_postboot.sh
reboot
```

Valetudo answers on `http://192.168.5.1`.

The Mac cannot be on two Wi-Fi networks at once, which is why fetching the binary
has to happen before joining the robot's AP.

### Known L40 quirks

- **Negative deviceId.** Units made around 08/2025 or later may report one,
  which miio does not expect, and Valetudo will not auto-detect the robot.
  ```sh
  cat /mnt/private/ULI/factory/did.txt      # negative number?
  mount -o remount,rw /mnt/private
  cp /mnt/private/ULI/factory/did.txt /mnt/private/ULI/factory/did_orig.txt && sync
  # edit did.txt to be positive
  rm /data/config/miio/device.conf          # regenerated on next boot
  reboot
  ```
- **Wi-Fi not sticking.** Try
  `rm -f /data/config/miio/wifi.conf /data/config/wifi/wpa_supplicant.conf /var/run/wpa_supplicant.conf; dreame_release.na -c 9 -i ap_info -m " "; reboot`
  then rejoin Wi-Fi through Valetudo.

## Phase 5 — Join the network

2.4GHz only, via Valetudo → Settings → Connectivity → Wi-Fi. Setting it over
UART with `read -s` keeps the password out of the serial log.

If Valetudo's basic auth is on, configure `VALETUDO_USERNAME` /
`VALETUDO_PASSWORD` in `.env` too.

Then follow `docs/POST-ROOT.md`.
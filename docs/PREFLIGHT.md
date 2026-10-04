# Pre-flight checklist

Do every step here before touching the debug connector. Each one exists because
skipping it has cost somebody a brick or a dead calibration blob.

## Identity

- [ ] Serial number under the dustbin starts with `R2492`.
      Not the base, not the box, and not the app — the sticker in the empty dust
      compartment. `R2492` means L40 Ultra.
- [ ] If it says anything else, **stop**. The L40 Ultra **AE**, the L40s and the
      rebranded L10s Pro Gen3 are different robots and are not supported.
- [ ] Model string in the Dreame app is `dreame.vacuum.r2492a`, `r2492b` or
      `r2492j`. (Confirmed for this unit: `r2492b`.)
- [ ] Write down the MAC address. Shown by your router as
      `dreame_vacuum_r2492b`, e.g. `aa:bb:cc:dd:ee:ff`.

## Cloud state

The robot is currently enrolled in Dreame's cloud and can pull OTA firmware
unattended. Firmware updates have patched rooting paths before.

- [ ] Note the current firmware version in the app.
- [ ] Do not open the Dreame app again. Decline any firmware prompt.
- [ ] Do the factory reset as late as practical — immediately before Phase 1.
      Hold `reset` beside the Wi-Fi LED until the robot speaks.
- [ ] If you already updated via the app, that is not fatal. The config value
      from `fastboot getvar config` is what selects a firmware build, not your
      firmware version. Just do not update again.

## Hardware function

Before voiding the warranty, confirm the robot is healthy using only the buttons:

- [ ] Start a clean with the physical button.
- [ ] It navigates, does not get stuck.
- [ ] Returns to the dock and charges.
- [ ] Auto-empty and mop-wash run when triggered from the app.
- [ ] No error indicators.

Some units ship with defects. It is much harder to tell a hardware fault from a
rooting mistake if the baseline is not known-good.

## Gear

- [ ] 3.3V USB-TTL adapter (FT232RL with the jumper set to 3.3V, or CP2102).
      **Verify the voltage jumper every time you connect.** 5V can damage the SoC.
- [ ] USB-A → Micro-USB **data** cable, to be sacrificed. Confirm it is a data
      cable, not charge-only.
- [ ] ~7 male dupont jumper wires.
- [ ] Precision tweezers.
- [ ] Pry tool for the top cover.
- [ ] Omarchy MacBook with `sudo pacman -S sunxi-tools android-tools`.

## Backups

Set up **before** any flashing, not after:

- [ ] Somewhere with ~5GB free for `dustx100/101/102.bin` and their zip.
- [ ] An RSA SSH key (`ssh-keygen -t rsa -b 4096`) — dustbuilder wants the `.pub`.
- [ ] A place to store the samples that is **not** the laptop that flashed them.
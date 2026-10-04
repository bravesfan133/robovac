# Wiring the debug connector without the breakout PCB

The official guide recommends [the Dreame Breakout
PCB](https://github.com/Hypfer/valetudo-dreameadapter). It is a passive breakout:
a 2.00mm 2x8 header, two 2.54mm pin headers, a button that pulls `Boot_SEL` to
GND, a micro-USB socket, a USB-A socket and an OTG jumper. Nothing on it is
active. If you cannot source one, wire it by hand.

Someone in `r/valetudorobotusers` rooted an X40 (same hardware family) with no
PCB at all:

> "For that you don't actually need a PCB breakout. I did it by cutting an old
> USB cable and crimping Dupont connectors to the ends. The button can be
> simulated by simply connecting and disconnecting wires. Just make sure that as
> much as possible of the D+ and D- wires are twisted and that they have the same
> length."

## Pin map

Connector is 2x8 at 2.00mm pitch, under the robot's top cover. **Bottom row** is
the one nearer the front of the robot. Read left to right:

| Bottom row | Signal | Needed for FEL? |
|---|---|---|
| 1 | `Boot_SEL` | yes — bridge to GND |
| 2 | `SoC_RX` | no (UART only) |
| 3 | `SoC_TX` | no (UART only) |
| 4 | `USB_ID` | leave unconnected |
| 5 | `D+` | yes |
| 6 | `D-` | yes |

**Top row:** `GND` is third from the left. `VBUS 5V` is at the far right.

> **Do not connect VBUS.** It sits next to the pins you need and it is the one
> pin on this connector that can hurt you. Leave it alone.

Verify against the upstream pinout image before you trust the numbers:
`docs/pages/installation/img/dreame_debug_connector_pinout.png` in
`Hypfer/Valetudo`. Some units have the connector rotated 90° or flipped; the
pinout images `dreame_debug_connector_pinout_90.png` and
`dreame_debug_connector_w10.jpg` cover those variants.

## FEL connection

Four connections total:

```
USB cable D+  ──► pin 5   D+
USB cable D-  ──► pin 6   D-
USB cable GND ──► top row GND
(dupont)      ──► pin 1   Boot_SEL  ──► GND   (held while powering on)
```

Cut the Micro-USB end off the cable, strip the four conductors, crimp dupont
connectors. Leave `VBUS` and `USB_ID` unconnected on both ends.

Signal integrity is the reason upstream pushes the PCB. At 115200 baud a random
untwisted dupont pair is fine; at USB 2.0's 480 Mbps it is marginal and will
fail mid-transfer, which is the worst possible moment. Mitigations:

- **Twist `D+` and `D-` together** for their whole length.
- Keep them the **same length**.
- Keep both leads short.
- Prefer a cable with a real shield if you have one.

## UART tap (do this before your first write)

Over USB, a failed write returns a bare `FAIL`. The real reason is printed only
on the serial console. On one L40 Ultra, `boot1` failed with no explanation over
USB and succeeded on retry with the UART attached; a later `rootfs1` failure was
an eMMC multi-block write timeout (`mmc 2 data timeout`, `cmd 25 STO`) that only
the serial log revealed.

Three more dupont wires to the top three pins of the bottom row:

| Adapter | Breakout pin |
|---|---|
| `TXD` | `SoC_RX` (pin 2) |
| `RXD` | `SoC_TX` (pin 3) |
| `GND` | `GND` |

Crossed, and **no VCC line at all** — the robot powers itself. 3.3V logic only.

Loopback-test the adapter before touching the robot: short `TXD` to `RXD`, run the
capture script, confirm the characters come back. It costs nothing and rules out
the most boring failure mode.

## Entering FEL

1. Robot off, top cover off, USB unplugged.
2. Bridge `Boot_SEL` to `GND` (dupont jumper or tweezers).
3. While holding that, hold the power button for 5 seconds.
4. Release the power button but **keep holding `Boot_SEL`** for 3 more seconds.
5. Button LEDs start pulsing. Plug USB into the computer.

The whole window is tight. The robot's own MCU cuts power roughly 210 seconds
after the button press, leaving 160–180 seconds of usable session. And
`fastboot getvar config` **must be the first command in every session** — the
payload answers anything else with `FAIL you need to run "fastboot getvar config" first`.

## Why this is safe to practise

FEL lives in the SoC's mask ROM and is selected by the button *before* the eMMC is
read. Entering FEL, reading `getvar config` and pulling the 400MB samples all
write nothing to flash. A wrong pin guess costs you time, not the robot.

Only `fastboot oem dust` and everything after it writes. Get `sunxi-fel ver`
answering and the samples downloaded before you go anywhere near that.
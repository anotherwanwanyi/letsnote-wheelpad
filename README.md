# letsnote-wheelpad

> 日本語版は [README.ja.md](README.ja.md) を参照してください。

A userland Linux daemon that reproduces the **Panasonic Let's Note "WheelPad"** circular touchpad scrolling behaviour. Draw a slow circle in the outer ring of your touchpad to scroll vertically — just like on Windows.

Works on Wayland and X11 by reading evdev events directly from the physical Synaptics touchpad and emitting wheel events through a `uinput` virtual device. The physical pad keeps driving the cursor as normal; this daemon contributes scroll only.

## Status

**v0.1.0 — first release.** Ported from reverse-engineered `WheelPad.exe` on Windows. Algorithm verified against the Ghidra-decompiled originals; defaults match the Windows out-of-box behaviour. Tested target: **Panasonic Let's Note CF-SV2 with Synaptics TM3562-3** on Ubuntu 26.04.

## Why this exists

`libinput` rejected adding circular scrolling to the Wayland-era stack (see Peter Hutterer's 2015 reasoning). So if you want your Let's Note's circular scroll to work on Linux, the only path is a userland daemon that reads the touchpad through evdev and emits wheel events through a separate virtual device. That's what this is.

## Install

### Ubuntu / Debian

```sh
sudo dpkg -i letsnote-wheelpad_0.1.0_amd64.deb
systemctl --user enable --now letsnote-wheelpad.service
```

### Fedora / RHEL

```sh
sudo rpm -i letsnote-wheelpad-0.1.0-1.x86_64.rpm
systemctl --user enable --now letsnote-wheelpad.service
```

### Arch

```sh
yay -S letsnote-wheelpad      # AUR
systemctl --user enable --now letsnote-wheelpad.service
```

### From source

```sh
git clone https://github.com/yourname/letsnote-wheelpad
cd letsnote-wheelpad
cargo build --release
sudo install -Dm755 target/release/letsnote-wheelpad /usr/bin/letsnote-wheelpad
sudo install -Dm644 packaging/udev/70-letsnote-wheelpad.rules /etc/udev/rules.d/70-letsnote-wheelpad.rules
sudo install -Dm644 packaging/systemd/letsnote-wheelpad.service /etc/systemd/user/letsnote-wheelpad.service
sudo install -Dm644 packaging/modules-load/letsnote-wheelpad.conf /etc/modules-load.d/letsnote-wheelpad.conf
sudo udevadm control --reload-rules && sudo udevadm trigger
sudo modprobe uinput
systemctl --user daemon-reload
systemctl --user enable --now letsnote-wheelpad.service
```

## Configuration

Configuration lives in `~/.config/letsnote-wheelpad/config.toml`. All keys are optional; defaults match the Windows out-of-box behaviour.

```toml
# Auto-detected by name regex. Override only if you have a non-standard pad.
# device = "/dev/input/event4"
# device_name_regex = "Synaptics.*TM3562"

[scroll]
enable               = true   # master enable
reverse_vertical     = false  # flip vertical scroll direction
horizontal_enable    = false  # enable bottom-edge horizontal-scroll wedge
reverse_horizontal   = false
sensitivity          = 0      # -2..+2 ; lower = less sensitive
detect_area_width    = 0      # 0..10 ; 0 = outer ring only, 10 = whole pad
horizontal_start     = 2      # arc start in π/8 units (2 → 45°)
horizontal_end       = 6      # arc end in π/8 units (6 → 135°)

[log]
level = "info"  # trace | debug | info | warn | error
```

| Key | Default | Range | Notes |
| --- | --- | --- | --- |
| `scroll.enable` | `true` | bool | Disable to keep the daemon alive but suppress all scroll. |
| `scroll.reverse_vertical` | `false` | bool | "Natural" scroll = `true`. |
| `scroll.horizontal_enable` | `false` | bool | Off by default; same as Windows. |
| `scroll.reverse_horizontal` | `false` | bool | |
| `scroll.sensitivity` | `0` | -2..+2 | Indexes the multiplier table `[10, 14, 20, 28, 40]`. |
| `scroll.detect_area_width` | `0` | 0..10 | `0` = require finger near the edge; `10` = whole pad. |
| `scroll.horizontal_start` | `2` | 0..15 | π/8 units. Default 45° → 135° = the bottom edge of the pad. |
| `scroll.horizontal_end` | `6` | 0..15 | |

### View logs

```sh
journalctl --user -u letsnote-wheelpad -f
```

The first second of operation logs the measured packet rate and the chosen history capacity (which scales with rate per [D-021](../DECISIONS.md)). If scrolling feels too fast or too slow, that's the first place to look.

## Known issues / non-goals

- **`WheelUnderCursor` is not configurable.** On Wayland the compositor routes input to the focused surface; there's no userland override.
- **Only the Synaptics TM3562-3 family is tested.** Other touchpads may work with `device_name_regex` overrides, but no compatibility promises.
- **Excel arrow-key fallback is gone.** Modern Excel routes horizontal wheel events natively; we don't need the Windows hack.
- **No coasting/kinetic scrolling.** Matches the Windows WheelPad behaviour; xf86 has it but we don't.

## How it works (one-paragraph version)

The daemon opens `/dev/input/eventN` (the physical touchpad) and watches multi-touch packets. A 6-state FSM (`Idle → Contact → Moving → Scrolling → Debounce`) decides when a finger is drawing a circle in the outer ring. While `Scrolling`, the daemon grabs the physical device with `EVIOCGRAB` (suspending the cursor-driving path so circles don't drag the pointer around) and integrates chord-direction angles into an accumulator. Every time the accumulator crosses ±π, the daemon emits one wheel notch on a separate `uinput` virtual device. On finger lift, the grab is released.

For the full algorithm details — including the reverse-engineering trail that motivated the Linux design — see the analysis docs that ship alongside the source.

## License

MIT. See [LICENSE](LICENSE).

## Acknowledgements

- Panasonic for the original WheelPad design, which this ports.
- The X.Org `xf86-input-synaptics` project for the angle-of-point-about-a-center reference implementation we compared against during reverse engineering.
- Peter Hutterer for the [2015 libinput discussion](https://gitlab.freedesktop.org/libinput/libinput/-/issues/) that explained why this had to be a daemon and not a libinput patch.

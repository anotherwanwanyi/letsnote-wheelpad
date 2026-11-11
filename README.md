# letsnote-wheelpad

> **Personal fork**
>
> This repository is a personal fork of
> [Nerahikada/letsnote-wheelpad](https://github.com/Nerahikada/letsnote-wheelpad),
> shared to make my changes available. It is not actively maintained and does
> not provide official binary packages.

A userland Linux daemon that reproduces the **Panasonic Let's Note "WheelPad"** circular touchpad scrolling behaviour. Draw a slow circle in the outer ring of your touchpad to scroll vertically — just like on Windows.

Works on Wayland and X11 by reading evdev events directly from the physical Synaptics touchpad and exposing two `uinput` devices: a mirrored touchpad for normal pointer and multi-finger input, plus a virtual wheel for circular scrolling.

## Why this exists

`libinput` rejected adding circular scrolling to the Wayland-era stack (see Peter Hutterer's 2015 reasoning). So if you want your Let's Note's circular scroll to work on Linux, the only path is a userland daemon that reads the touchpad through evdev and emits wheel events through a separate virtual device. That's what this is.

## Changes in this fork

- **Smooth, proportional high-resolution scrolling**: Integrates center-relative polar angle directly into ~125Hz `REL_WHEEL_HI_RES` events, removing startup jumps, trailing scroll lag, and low-frequency stepping.
- **Better multi-finger gesture coexistence**: Scales the outer scroll ring to the touchpad's actual size and gives multi-finger gestures a brief preemption window so 2- and 3-finger swipes are not swallowed by circular capture.
- **Stable scroll axis**: Locks vertical vs. horizontal scrolling when a gesture starts so full-circle scrolling never flips direction mid-circle.
- **Event-stream & state-machine fixes**: Fixes stuck virtual contacts after consecutive circles, double cursor defuzzing, watchdog resets while pausing a finger on the pad, and companion evdev node misdetection.

These changes were tested on a Panasonic Let's Note CF-FV1 running Arch Linux. This is provided for reference only and does not imply compatibility with other models.

## Build and install from source

This fork does not publish Debian, RPM, or AUR packages.

```sh
git clone https://github.com/anotherwanwanyi/letsnote-wheelpad.git
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
# Auto-detected by evdev name. See "Using a differently named touchpad"
# below if detection fails.
# device = "/dev/input/event4"
# device_name_regex = "Synaptics.*TM3562"

[scroll]
enable               = true   # master enable
reverse_vertical     = false  # flip vertical scroll direction
horizontal_enable    = false  # enable bottom-edge horizontal-scroll wedge
reverse_horizontal   = false
sensitivity          = 0      # -4..+2 ; lower = less sensitive
detect_area_width    = 0      # 0..10 ; 0 = outer ring only, 10 = whole pad
horizontal_start     = 2      # arc start in π/8 units (2 → 45°)
horizontal_end       = 6      # arc end in π/8 units (6 → 135°)

[log]
level = "info"  # trace | debug | info | warn | error
```

### Using a differently named touchpad

If automatic detection fails, list the evdev device names and paths:

```sh
for event in /sys/class/input/event*; do
    printf '%s: ' "/dev/input/${event##*/}"
    cat "$event/device/name"
done
```

Choose a distinctive part of the physical touchpad name and add a top-level pattern before `[scroll]` in the config file:

```toml
device_name_regex = "UNIQUE PART OF THE TOUCHPAD NAME"

[scroll]
# ...
```

The daemon also needs permission to open the device. Create a local udev rule at `/etc/udev/rules.d/71-letsnote-wheelpad-local.rules`:

```udev
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="*UNIQUE PART OF THE TOUCHPAD NAME*", \
    MODE="0660", GROUP="input", TAG+="uaccess"
```

Then reload the rules and restart the daemon:

```sh
sudo udevadm control --reload-rules
sudo udevadm trigger
systemctl --user restart letsnote-wheelpad.service
```

`device_name_regex` uses regular-expression syntax, while `ATTRS{name}` in the udev rule uses glob syntax. Make the selected name fragment specific enough not to match another input device.

### Configuration reference

| Key | Default | Range | Notes |
| --- | --- | --- | --- |
| `scroll.enable` | `true` | bool | Disable to keep the daemon alive but suppress all scroll. |
| `scroll.reverse_vertical` | `false` | bool | "Natural" scroll = `true`. |
| `scroll.horizontal_enable` | `false` | bool | Off by default; same as Windows. |
| `scroll.reverse_horizontal` | `false` | bool | |
| `scroll.sensitivity` | `0` | -4..+2 | Indexes the multiplier table `[5, 7, 10, 14, 20, 28, 40]`; `-4` and `-3` are extra-low Linux extensions. |
| `scroll.detect_area_width` | `0` | 0..10 | `0` = outer 32% bezel ring (inner 68% radius reserved for pointer/multi-touch); `10` = whole pad. |
| `scroll.horizontal_start` | `2` | 0..15 | π/8 units. Default 45° → 135° = the bottom edge of the pad. |
| `scroll.horizontal_end` | `6` | 0..15 | |

### View logs

```sh
journalctl --user -u letsnote-wheelpad -f
```

If scrolling feels too fast or too slow, adjust `scroll.sensitivity` in the config (-4..+2).

## Known issues / non-goals

- **`WheelUnderCursor` is not configurable.** On Wayland the compositor routes input to the focused surface; there's no userland override.
- **Hardware coverage is limited.** Touchpads whose evdev names do not match the default pattern require the configuration and udev override described above. Compatibility with other hardware is not guaranteed.
- **Excel arrow-key fallback is gone.** Modern Excel routes horizontal wheel events natively; we don't need the Windows hack.
- **No coasting/kinetic scrolling.** Matches the Windows WheelPad behaviour; xf86 has it but we don't.

## License

MIT. See [LICENSE](LICENSE).

## Acknowledgements

- [Nerahikada](https://github.com/Nerahikada) for the original Linux implementation this repository is forked from.
- Panasonic for the original WheelPad design, which this ports.
- The X.Org `xf86-input-synaptics` project for the angle-of-point-about-a-center reference implementation we compared against during reverse engineering.
- Peter Hutterer for the [2015 libinput discussion](https://gitlab.freedesktop.org/libinput/libinput/-/issues/) that explained why this had to be a daemon and not a libinput patch.

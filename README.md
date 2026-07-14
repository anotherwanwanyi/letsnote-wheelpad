# letsnote-wheelpad

> 日本語版は [README.ja.md](README.ja.md) を参照してください。

A userland Linux daemon that reproduces the **Panasonic Let's Note "WheelPad"** circular touchpad scrolling behaviour. Draw a slow circle in the outer ring of your touchpad to scroll vertically — just like on Windows.

Works on Wayland and X11 by reading evdev events directly from the physical Synaptics touchpad and exposing two `uinput` devices: a mirrored touchpad for normal pointer and multi-finger input, plus a virtual wheel for circular scrolling.

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
git clone https://github.com/Nerahikada/letsnote-wheelpad
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

If scrolling feels too fast or too slow, adjust `scroll.sensitivity` in the config (-2..+2). The daemon does not auto-calibrate — history capacity is fixed at 20 slots to match Windows exactly (see DECISIONS.md D-021-followup).

## Known issues / non-goals

- **`WheelUnderCursor` is not configurable.** On Wayland the compositor routes input to the focused surface; there's no userland override.
- **Only the Synaptics TM3562-3 family is tested.** Other touchpads may work with `device_name_regex` overrides, but no compatibility promises.
- **Excel arrow-key fallback is gone.** Modern Excel routes horizontal wheel events natively; we don't need the Windows hack.
- **No coasting/kinetic scrolling.** Matches the Windows WheelPad behaviour; xf86 has it but we don't.

## Multi-finger gesture priority

Circular scrolling is a single-finger gesture. Before it is captured, normal multi-finger gestures have priority:

- While an outer-ring single-finger gesture is still a candidate, its complete `SYN_REPORT` frames are held briefly instead of being forwarded immediately. If a second finger appears, the held frames are replayed unchanged and the daemon enters passthrough-only `MultiTouch` mode. Circular recognition then stays disabled until every finger has lifted; removing only the second finger does not re-arm it halfway through the gesture.
- Capture requires at least three meaningful samples, 7.5° of angular travel, tangential motion that dominates radial motion, and net curvature consistent with the travel direction. Crossing 7.5° without enough evidence remains pending instead of permanently rejecting the gesture; this lets a dense, noisy, or slowly developing arc continue into circular scrolling. Clear radial motion, a sustained straight tangent, or the 20-meaningful-sample observation limit selects ordinary pointer input instead. The held frames are then replayed unchanged and `Passthrough` remains locked in until all-up. A separate 64-raw-frame safety limit bounds buffering when reports contain almost no meaningful movement.
- Once circular scrolling is captured, the held frames are discarded, so libinput never sees the captured contact and there is no initial cursor twitch. Fingers added after capture are also hidden, and the detector remains locked to the original tracking ID. If that original finger lifts first, another finger is never substituted into its circular trajectory.
- Because a captured contact is never started on the virtual touchpad, its final lift can be suppressed without synthesizing slot or touch-key cleanup events and without leaving a stale contact in libinput.

In short: multi-finger input wins before circular capture; an already captured circular gesture keeps ownership until all-up. The next contact sequence starts a fresh arbitration.

## How it works (one-paragraph version)

The daemon takes exclusive ownership of the physical touchpad at startup (`EVIOCGRAB`, held forever) and creates two virtual `uinput` devices that libinput attaches to instead: a touchpad mirror (same capabilities as the physical pad) and a wheel. A 7-state FSM (`Idle`, `Contact`, `Moving`, `MultiTouch`, `Passthrough`, `Scrolling`, `Debounce`) arbitrates input. Ordinary events are forwarded verbatim; an outer-ring single-finger candidate is the exception, with complete frames held while `Moving` classifies its early trajectory. A pointer or multi-finger decision replays those frames in order, while a circular decision discards them and suppresses the rest of that physical contact stream. The circular detector is preheated with the candidate samples, then integrates chord-direction angles and emits one virtual-wheel notch at each ±π crossing. Since a captured touch never reaches the virtual touchpad at all, it produces neither an initial cursor jump nor stale libinput contact state at all-up.

For the full algorithm details and the architectural pivot history — see `DECISIONS.md` (D-022 is the passthrough decision; D-008..D-021 are the algorithm choices) and the analysis docs alongside the source.

## License

MIT. See [LICENSE](LICENSE).

## Acknowledgements

- Panasonic for the original WheelPad design, which this ports.
- The X.Org `xf86-input-synaptics` project for the angle-of-point-about-a-center reference implementation we compared against during reverse engineering.
- Peter Hutterer for the [2015 libinput discussion](https://gitlab.freedesktop.org/libinput/libinput/-/issues/) that explained why this had to be a daemon and not a libinput patch.

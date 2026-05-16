#!/bin/sh
set -e

if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules || true
    udevadm trigger || true
fi

if [ -e /proc/modules ] && ! grep -q '^uinput ' /proc/modules; then
    modprobe uinput || true
fi

if command -v systemctl >/dev/null 2>&1; then
    systemctl --global enable letsnote-wheelpad.service || true
fi

exit 0

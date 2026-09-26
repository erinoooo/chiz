#!/bin/sh
# chiz-setup.sh — one-time Linux setup for AppImage users (spec 10).
# Installs the uinput rule + modules-load entry, then reloads udev.
# Run once with: pkexec chiz-setup.sh  (or: sudo ./chiz-setup.sh)
set -eu
HERE=$(dirname "$0")
install -m 644 "$HERE/60-chiz-uinput.rules" /etc/udev/rules.d/60-chiz-uinput.rules
install -m 644 "$HERE/chiz.conf" /etc/modules-load.d/chiz.conf
modprobe uinput 2>/dev/null || true
udevadm control --reload-rules && udevadm trigger
echo "Chiz uinput setup done. Log out/in if /dev/uinput is still denied."

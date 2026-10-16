#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Keep openRuyi Zero's applet setup, mounts, console and BusyBox init sequence.
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
# The headless Zero package set has no generated glibc locale data. C is
# built in; C.UTF-8 requires locale files that this image does not contain.
export LANG=C
/bin/busybox --install -s /usr/bin
mount -a
exec </dev/console >/dev/console 2>/dev/console
exec busybox init

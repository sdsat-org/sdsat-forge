#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
set -eu

export HOME=/root
# The headless image has no generated glibc locale data.
export LANG=C
exec /init

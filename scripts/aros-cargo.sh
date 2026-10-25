#!/bin/sh
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 John Knipper
# Build with the integrated official compiler and precompiled AROS std.
set -eu
cd "$(dirname "$0")/.."
exec python3 scripts/aros-cargo.py "$@"

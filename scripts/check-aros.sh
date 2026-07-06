#!/bin/sh
# check-aros.sh -- type-check Feraille for AROS (branch aros-port).
# See docs/features/aros-building.md for the full setup this expects.
set -eu
cd "$(dirname "$0")/.."
TARGET_JSON="${AROS_TARGET_JSON:-$(pwd)/../aros-aarch64/hosted/rust/aarch64-unknown-aros.json}"
exec cargo +nightly-2026-06-27 check -p feraille-gpui \
    --target "$TARGET_JSON" \
    -Zjson-target-spec -Zbuild-std=std,panic_abort "$@"

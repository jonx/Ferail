#!/bin/sh
# check-aros.sh -- type-check Ferail for AROS (branch aros-port).
# See docs/features/aros-building.md for the full setup this expects.
set -eu
cd "$(dirname "$0")/.."
TARGET_JSON="${AROS_TARGET_JSON:-$(pwd)/../aros-aarch64/hosted/rust/aarch64-unknown-aros.json}"
exec cargo +nightly-2026-06-27 --config "$(pwd)/packaging/aros/aros-patches.toml" \
    check -p ferail-gpui \
    --target "$TARGET_JSON" \
    -Zjson-target-spec -Zbuild-std=std,panic_abort "$@"

#!/bin/sh
# check-aros.sh -- type-check Feraille for AROS (branch aros-port).
# See docs/features/aros-port.md for the whole picture.
set -eu
cd "$(dirname "$0")/.."
exec cargo +nightly-2026-06-27 check -p feraille-gpui \
    --target /Users/jkn/Source/aros-aarch64/hosted/rust/aarch64-unknown-aros.json \
    -Zjson-target-spec -Zbuild-std=std,panic_abort "$@"

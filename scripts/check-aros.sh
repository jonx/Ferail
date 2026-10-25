#!/bin/sh
# check-aros.sh -- type-check Ferail for native AROS (AArch64).
# See docs/features/aros-building.md for the setup this expects.
set -eu
cd "$(dirname "$0")/.."
exec scripts/aros-cargo.sh check -p ferail-aros-app "$@"

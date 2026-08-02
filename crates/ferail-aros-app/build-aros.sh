#!/bin/bash
# build-aros.sh -- the one-command AROS build: cargo cross-build (with the
# stale-std guard below) + link-aros.sh (link + deploy C:Ferail).
#
#   crates/ferail-aros-app/build-aros.sh              # debug
#   PROFILE=release crates/ferail-aros-app/build-aros.sh
#
# THE STALE-STD GUARD. std for this target is compiled from source
# (-Zbuild-std) out of the rust-src symlink -> ~/Source/rust-aros. Cargo does
# NOT fingerprint those sources: edit the std pal, rebuild, and cargo reuses
# the stale libstd rlib in ~0.5s -- your change silently never ships (cost us
# a whole false "fix failed validation" round on 2026-07-18). So: whenever
# anything under rust-aros/library is newer than the built libstd rlib, clear
# std's fingerprints + rlibs first, forcing a real std rebuild (~5 min).
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"
PROFILE="${PROFILE:-debug}"
TRIPLE=aarch64-unknown-aros
TDIR="$REPO/target/$TRIPLE/$PROFILE"
RUST_SRC="${AROS_RUST_SRC:-$HOME/Source/rust-aros}"
TARGET_JSON="$REPO/../aros-aarch64/hosted/rust/$TRIPLE.json"
TOOLCHAIN="${AROS_RUST_TOOLCHAIN:-nightly-2026-06-27}"

if [ -d "$RUST_SRC/library" ]; then
    rlib="$(ls -t "$TDIR"/deps/libstd-*.rlib 2>/dev/null | head -1 || true)"
    newest_src="$(find "$RUST_SRC/library" -name '*.rs' -newer "${rlib:-/dev/null}" -print -quit 2>/dev/null || true)"
    if [ -z "$rlib" ] || [ -n "$newest_src" ]; then
        echo "[build] std source newer than built rlib (${newest_src:-no rlib}) -- forcing std rebuild"
        rm -rf "$TDIR"/.fingerprint/std-* "$TDIR"/deps/libstd-* "$TDIR"/deps/std-*
    fi
fi

CARGO_PROFILE_FLAG=""
[ "$PROFILE" = "release" ] && CARGO_PROFILE_FLAG="--release"

PATCHES="$REPO/packaging/aros/aros-patches.toml"

# THE INERT-PATCH GUARD. `aros-patches.toml` overrides libc with the vendored
# AROS copy, and cargo only honours a `[patch]` whose version the lock can
# accept -- it will not downgrade to one. Any host `cargo` command that bumps
# libc past the vendored version therefore *silently disables* the patch, and
# the next AROS build dies deep in lzma-sys with
#
#     error[E0432]: unresolved imports `libc::c_char`, `libc::size_t`
#
# which says nothing about the real cause. Re-assert the pin every build rather
# than relying on whoever last touched the lockfile (this bit twice on
# 2026-08-01/02). The version is read from the vendored crate so it cannot go
# stale against it.
LIBC_VENDOR="${AROS_ZED_SRC:-$REPO/../zed-aros}/vendor-aros/libc/Cargo.toml"
LIBC_VER="$(awk -F'"' '/^version = /{print $2; exit}' "$LIBC_VENDOR" 2>/dev/null || true)"
if [ -n "$LIBC_VER" ]; then
    cargo "+$TOOLCHAIN" --config "$PATCHES" \
        update -p libc --precise "$LIBC_VER" >/dev/null 2>&1 || true
fi

# The AROS source overrides ride on --config so they stay off host builds
# ([patch] is global; see the file's header).
cargo "+$TOOLCHAIN" --config "$PATCHES" \
    build $CARGO_PROFILE_FLAG -p ferail-aros-app \
    --target "$TARGET_JSON" \
    -Zjson-target-spec -Zbuild-std=std,panic_abort

PROFILE="$PROFILE" "$DIR/link-aros.sh"

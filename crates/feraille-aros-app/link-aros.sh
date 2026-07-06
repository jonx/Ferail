#!/bin/bash
# link-aros.sh -- link the feraille-aros-app staticlib + C harness into a
# loadable AROS C: command and deploy it. Mirrors hosted/rust/std-build.sh
# (the proven collect-aros recipe); differences: the staticlib bundles the
# gpui_aros C glue (compiled by build.rs against the SDK headers), and the
# patched crosstools lld is taken from the crosstools dir (COMPILER_PATH
# needs a plain `ld`, so a shim dir symlinks ld -> ld.lld).
#
# Stable locations (canonical; the old /tmp copies get eaten by the macOS
# periodic /tmp cleaner): build tree ~/aros-build, toolchain ~/aros-crosstools.
# Override with AROS_BUILD / AROS_CROSSTOOLS.
#
#   crates/feraille-aros-app/link-aros.sh            # link + deploy C:Feraille
#   PROFILE=release crates/feraille-aros-app/link-aros.sh
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"
PROFILE="${PROFILE:-debug}"
RSLIB="$REPO/target/aarch64-unknown-aros/$PROFILE/libferaille_aros_app.a"

T="${AROS_BUILD:-$HOME/aros-build}/bin/darwin-aarch64"
XTOOLS="${AROS_CROSSTOOLS:-$HOME/aros-crosstools}"
GEN="$T/gen"; DEV="$T/AROS/Developer"; LIBDIR="$DEV/lib"
CDIR="$T/AROS/C"; COLLECT="$T/tools/collect-aros"
CC="${AROS_CC:-clang}"
OUT="$DIR/build"; mkdir -p "$OUT"

[ -f "$RSLIB" ] || { echo "FAIL: $RSLIB missing -- cargo build the smoke first" >&2; exit 1; }
[ -x "$COLLECT" ] || { echo "FAIL: no collect-aros at $COLLECT" >&2; exit 2; }

# collect-aros resolves `ld` via COMPILER_PATH; the AROS-patched lld
# (knows -maarch64elf_aros) lives in the crosstools dir.
XTBIN="$OUT/toolshim"; mkdir -p "$XTBIN"
ln -sf "$XTOOLS/bin/ld.lld" "$XTBIN/ld"

CFLAGS=(--target=aarch64-unknown-none-elf -fno-pic -mcmodel=large -ffixed-x18
        -D__arm64__ -O2 -I"$GEN/include" -I"$DEV/include")
AUTOLIB=(-lmui -lamiga -larossupport -lamiga -lcodesets -lkeymap -lexpansion
         -lcommodities -ldiskfont -lasl -lmuimaster -ldatatypes -lcybergraphics
         -lworkbench -licon -lintuition -lgadtools -llayers -laros -lpartition
         -liffparse -lgraphics -llocale -ldos -lutility -loop -llibinit -lautoinit)
STDLIBS=(-lposixc -lstdcio -lstdc -lexec -lpthread)

echo "[link] compile harness feraille_main.c"
"$CC" "${CFLAGS[@]}" -c "$DIR/c/feraille_main.c" -o "$OUT/feraille_main.o"

# The rust-aros std's C shim layer (sys/*/aros.rs call these), same recipe
# as hosted/rust/std-build.sh — fs/sync need the posixc include dir.
RS="${AROS_RS:-/Users/jkn/Source/aros-aarch64/hosted/rust}"
echo "[link] compile rust-aros std glues"
"$CC" "${CFLAGS[@]}" -c "$RS/aros_net_glue.c" -o "$OUT/aros_net_glue.o"
"$CC" "${CFLAGS[@]}" -I"$GEN/include/aros/posixc" -c "$RS/aros_fs_glue.c" -o "$OUT/aros_fs_glue.o"
"$CC" "${CFLAGS[@]}" -c "$RS/aros_process_glue.c" -o "$OUT/aros_process_glue.o"
"$CC" "${CFLAGS[@]}" -c "$RS/aros_thread_glue.c" -o "$OUT/aros_thread_glue.o"
"$CC" "${CFLAGS[@]}" -I"$GEN/include/aros/posixc" -c "$RS/aros_sync_glue.c" -o "$OUT/aros_sync_glue.o"

echo "[link] collect-aros -> ET_REL AROS program ($PROFILE)"
COMPILER_PATH="$XTBIN" "$COLLECT" \
    --eh-frame-hdr --allow-multiple-definition \
    -L"$LIBDIR" -o "$OUT/Feraille" \
    "$LIBDIR/startup.o" "$OUT/feraille_main.o" \
    "$OUT/aros_net_glue.o" "$OUT/aros_fs_glue.o" "$OUT/aros_process_glue.o" \
    "$OUT/aros_thread_glue.o" "$OUT/aros_sync_glue.o" "$RSLIB" \
    -\( "${AUTOLIB[@]}" "${STDLIBS[@]}" -\)
echo "[link] built: $OUT/Feraille ($(stat -f%z "$OUT/Feraille") bytes)"

cp -f "$OUT/Feraille" "$CDIR/Feraille"; chmod +x "$CDIR/Feraille"
echo "[link] deployed -> $CDIR/Feraille"

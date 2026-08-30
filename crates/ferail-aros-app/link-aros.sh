#!/bin/bash
# link-aros.sh -- link the ferail-aros-app staticlib + C harness into a
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
#   crates/ferail-aros-app/link-aros.sh            # link + deploy C:Ferail
#   PROFILE=release crates/ferail-aros-app/link-aros.sh
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$DIR/../.." && pwd)"
PROFILE="${PROFILE:-debug}"
RSLIB="$REPO/target/aarch64-unknown-aros/$PROFILE/libferail_aros_app.a"

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

echo "[link] compile harness ferail_main.c"
"$CC" "${CFLAGS[@]}" -c "$DIR/c/ferail_main.c" -o "$OUT/ferail_main.o"

# The rust-aros std's C shim layer (sys/*/aros.rs call these), same recipe
# as hosted/rust/std-build.sh - fs/sync need the posixc include dir.
RS="${AROS_RS:-/Users/jkn/Source/aros-aarch64/hosted/rust}"
echo "[link] compile rust-aros std glues"
"$CC" "${CFLAGS[@]}" -c "$RS/aros_net_glue.c" -o "$OUT/aros_net_glue.o"
"$CC" "${CFLAGS[@]}" -I"$GEN/include/aros/posixc" -c "$RS/aros_fs_glue.c" -o "$OUT/aros_fs_glue.o"
"$CC" "${CFLAGS[@]}" -c "$RS/aros_process_glue.c" -o "$OUT/aros_process_glue.o"
# aros_proc_glue.c is the newer pipe/spawn/signal layer (aros_pipe_*,
# aros_proc_*, aros_sig_*, aros_task_self) the std's process pal grew for the
# over-pipes shell. Distinct file from aros_process_glue.c above, and not in
# hosted/rust/std-build.sh's list - without it the final link fails with 13
# undefined symbols.
"$CC" "${CFLAGS[@]}" -c "$RS/aros_proc_glue.c" -o "$OUT/aros_proc_glue.o"
"$CC" "${CFLAGS[@]}" -c "$RS/aros_thread_glue.c" -o "$OUT/aros_thread_glue.o"
"$CC" "${CFLAGS[@]}" -I"$GEN/include/aros/posixc" -c "$RS/aros_sync_glue.c" -o "$OUT/aros_sync_glue.o"
"$CC" "${CFLAGS[@]}" -c "$RS/aros_env_glue.c" -o "$OUT/aros_env_glue.o"

echo "[link] collect-aros -> ET_REL AROS program ($PROFILE)"
COMPILER_PATH="$XTBIN" "$COLLECT" \
    --eh-frame-hdr --allow-multiple-definition \
    -L"$LIBDIR" -o "$OUT/Ferail" \
    "$LIBDIR/startup.o" "$OUT/ferail_main.o" \
    "$OUT/aros_net_glue.o" "$OUT/aros_fs_glue.o" "$OUT/aros_process_glue.o" \
    "$OUT/aros_proc_glue.o" \
    "$OUT/aros_thread_glue.o" "$OUT/aros_sync_glue.o" "$OUT/aros_env_glue.o" "$RSLIB" \
    -\( "${AUTOLIB[@]}" "${STDLIBS[@]}" -\)
echo "[link] built: $OUT/Ferail ($(stat -f%z "$OUT/Ferail") bytes)"

# Debug-info ET_RELs take minutes to LoadSeg (every reloc walked); strip
# before deploy (gpui_aros/PORTING.md "LoadSeg scale").
"$XTOOLS/bin/llvm-strip" --strip-debug "$OUT/Ferail"
echo "[link] stripped: $OUT/Ferail ($(stat -f%z "$OUT/Ferail") bytes)"

cp -f "$OUT/Ferail" "$CDIR/Ferail"; chmod +x "$CDIR/Ferail"
echo "[link] deployed -> $CDIR/Ferail"

# AROS port (branch `aros-port`)

Feraille on AROS (the open-source AmigaOS), targeting the user's hosted
`darwin-aarch64` AROS (AROS running as a macOS process — see
`~/Source/aros-aarch64`). Local-only work: absolute paths to sibling
checkouts are by design, mirroring the `[patch]` sections in `Cargo.toml`.

## Status

- **`cargo check -p feraille-gpui --target aarch64-unknown-aros` is GREEN**
  — the whole app type-checks for AROS: gpui via the `gpui_aros` CPU
  backend, gpui-component (full tree-sitter grammar set), bundled sqlite,
  notify, and every feraille crate.
- A GPUI smoke app (`zed-aros/crates/gpui_aros_smoke`) **links** into a real
  AROS `C:GpuiSmoke` command via collect-aros.
- Run-on-AROS is gated on a healthy boot image (see *Boot image damage*).

## The pieces and where they live

| Piece | Where | Branch |
| --- | --- | --- |
| gpui AROS backend (`gpui_aros`: Intuition/CyberGraphics C glue, tiny-skia CPU renderer, std-thread dispatcher, keyboard/wheel input, clipboard.device) | `~/Source/zed-aros` | `aros-platform` |
| Custom Rust std for AROS (posixc-backed fs/thread/net/random) | `~/Source/rust-aros` (symlinked into the nightly's `rust-src`) | — |
| Target spec JSON + C compat shims (`endian.h`, `sys/ioctl.h`) + std C glue | `~/Source/aros-aarch64/hosted/rust/` | — |
| gpui-component with `smol` → `async-channel` (keeps the async-io/rustix reactor out) | `~/Source/gpui-component-aros` (worktree @ pinned rev c112e7b) | `aros-port` |
| AROS OS source (incl. `exec/types.h` storage-class-macro guards) | `~/Source/aros-upstream` | `crash-containment` |
| AROS build tree / SDK / boot image | `/tmp/arosbuild` (canonical), `/tmp/arosbuild2` (scratch rebuilds) | — |
| Patched crosstools (AROS clang/lld, `aarch64elf_aros` emulation, compiler-rt builtins) | `/tmp/aros-crosstools` | — |
| Hosted-AROS run/automation harness | `~/Source/aros-aarch64/graft/` (`aros-ctl`, `run-window.sh`, smokes) | — |

## The check command

```sh
cargo +nightly-2026-06-27 check -p feraille-gpui \
  --target /Users/jkn/Source/aros-aarch64/hosted/rust/aarch64-unknown-aros.json \
  -Zjson-target-spec -Zbuild-std=std,panic_abort
```

(or `scripts/check-aros.sh`). Notes:

- The pinned nightly matters: the custom std rides the toolchain's
  `rust-src` symlink (`$(rustc --print sysroot)/lib/rustlib/src/rust` →
  `~/Source/rust-aros`).
- `-Zjson-target-spec` became required for JSON targets on this nightly.
- `.cargo/config.toml` (force-added on this branch; normally gitignored)
  carries the target rustflags (`getrandom_backend="custom"`) and the
  AROS C recipe for cc-rs (`CC/AR/CFLAGS_aarch64_unknown_aros`).

## C-of-the-graph lessons (all encoded in `.cargo/config.toml`)

- **`-D_GNU_SOURCE`** — AROS posixc guards core-POSIX `fdopen`/`popen`
  behind `_GNU_SOURCE`/`_XOPEN_SOURCE`; tree-sitter et al. expect them
  visible.
- **`-DHAVE_ENDIAN_H` + compat include** — AROS has no `<endian.h>`;
  tree-sitter's `portable/endian.h` honors `HAVE_ENDIAN_H`. Shim at
  `aros-aarch64/hosted/rust/compat/include/endian.h`; the same dir shims
  `sys/ioctl.h` (sqlite includes it unconditionally, calls it only behind
  `__linux__`).
- **sqlite**: `-DSQLITE_OMIT_WAL=1 -DSQLITE_MAX_MMAP_SIZE=0
  -DSQLITE_OMIT_LOAD_EXTENSION=1` — AROS has no mmap/shm-WAL/dlopen; the
  classic rollback-journal configuration is semantically required, not just
  a compile fix.
- **`exec/types.h`** unconditionally `#define GLOBAL extern` and clobbered
  sqlite's function-like `GLOBAL(t,v)` mid-amalgamation (reached via
  `<pthread.h>`); fixed at the source (guarded per-macro, AmigaOS-NDK
  style) in aros-upstream commit `5c1666af`.

## Vendored / patched crates (`[patch]` in `Cargo.toml`)

- `stacker` (zed-aros/vendor-aros) — no-op stack growth on AROS. The lock
  must resolve to the vendored 0.1.23: `cargo update -p stacker --precise
  0.1.23` if a bump reintroduces 0.1.24.
- `filetime` (zed-aros/vendor-aros) — AROS arm reading via std `Metadata`;
  set-times returns `Unsupported` (notify's poll watcher only reads).
- `gpui-component` — the `aros-port` worktree swaps `smol::channel` for
  `async-channel` (identical re-export) so smol's async-io → rustix →
  errno reactor never enters the graph.
- gpui's `test-support` gates `proptest` off AROS (rusty-fork/wait-timeout
  have no arm); `render_to_image` and the rest of test-support survive.

## platform_shell

`feraille-shell-aros` v1 re-exports the `feraille-shell-linux` scaffold
(its `cfg(not(linux))` no-op arms + the pure-Rust `compress_paths`). Real
AROS integrations to grow there: workbench.library reveal, icon.library
icons, clipboard.device file URLs, DefIcons-style thumbnails.

## Running on hosted AROS

```sh
# build + link + deploy the GPUI smoke:
cd ~/Source/zed-aros
cargo +nightly-2026-06-27 build -p gpui_aros_smoke --target ... (as above)
crates/gpui_aros_smoke/link-aros.sh          # -> C:GpuiSmoke in the image

# boot the desktop with the smoke as payload, screenshot:
cd ~/Source/aros-aarch64
AROS_CTL_DESKTOP_EXTRA='Run <NIL: >NIL: QUIET C:GpuiSmoke' \
  AROS_CTL_STARTUP_MODE=desktop graft/aros-ctl run
graft/aros-ctl shot proof.png
```

## Boot image damage (the `/tmp` GC problem)

macOS periodically GCs `/tmp` **by file atime**, so `/tmp/arosbuild` decays
piecemeal — headers, kickstart modules, C: commands and crosstools binaries
vanish independently while directories survive. Encountered so far:

- SDK headers (`gen/include`) — regenerate with the scratch recipe:
  configure a fresh `/tmp/arosbuild2` (thin xtools per
  `graft/build-darwin-aarch64.sh` step 2, retargeted at
  `/tmp/aros-crosstools`'s patched clang) and `make includes`, then rsync
  over the canonical tree.
- `Devs/FileSystem.resource` (kickstart module) — `make kernel-filesystem`
  in the scratch tree, copy over. Needs the crosstools clang (spec flags
  like `-noposixc`), Homebrew clang builtin headers copied into its GC'd
  resource dir, and `lib/generic/libclang_rt.builtins-aarch64.a` symlinked
  into the xtools.
- **`AROS.boot`** — the sneakiest one. dos's `__dos_IsBootable` requires a
  signature file `:AROS.boot` at the boot volume root containing the CPU
  string (`aarch64`). With it GC'd, every mount succeeds but the boot
  volume is deemed "not bootable" (Res2=212) and dos.library's init
  returns NULL — surfacing as the wildly misleading `Could not open
  version 36 or higher of library "dos.library"` alert from dosboot.
  Restore with `echo aarch64 > <bootdir>/AROS.boot`. (Diagnosed by
  transplanting a `-DDEBUG=1` dos.library built via
  `make kernel-dos` in the scratch tree — the `Dos/CliInit:` trace names
  the failing step directly. Keep the debug build out of normal boots:
  it logs every DOS packet and makes big LoadSegs crawl.)
- Single-file transplants from the scratch tree into the old image work
  fine (FileSystem.resource, dos.library round-tripped cleanly);
  `make <metatarget>` in the scratch tree is the recovery tool of choice.
  Avoid plain `make` — the default target tries to fetch and build LLVM.

The durable fix would be moving the canonical build out of `/tmp` (the
tooling honors `AROS_BUILD`/`BUILD`/`AROS_CTL_BOOTD` env for that).

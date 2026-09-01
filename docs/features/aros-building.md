# Building Ferail for AROS - the complete guide

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

How to go from five git checkouts to **Ferail running on AROS**, on an
Apple Silicon Mac. This documents everything "in between": the AROS OS
build, the cross-toolchain, the custom Rust std, the patched GPUI stack,
and the link/deploy/run loop. It is the reproduction recipe for the state
described in [aros-port.md](aros-port.md) (the map of what lives where and
why).

> **Reality check.** This is a hosted-OS research port. The final target is
> AROS (the open-source AmigaOS re-implementation) running *as a macOS
> process*: `darwin-aarch64` hosted AROS. Nothing here targets real Amiga
> hardware, and several trees are moving research code, not releases.

> **Dependency migration checkpoint (2026-08-28).** Desktop Ferail now uses
> Zed `f66ed399` and gpui-component `e8f54eb`. The sibling AROS forks still
> need to be rebased and revalidated before this recipe is current again; see
> [the migration handover](../memos/gpui-migration-2026-08.md#aros). Do not treat a
> host macOS build as proof that the AROS source overrides still interoperate.

---

<!-- toc depth=2 -->

- [0. What you end up with](#0-what-you-end-up-with)
- [1. Host prerequisites](#1-host-prerequisites)
- [2. Checkout layout (sibling paths are load-bearing)](#2-checkout-layout-sibling-paths-are-load-bearing)
- [3. Build hosted AROS + its SDK (the big prerequisite)](#3-build-hosted-aros--its-sdk-the-big-prerequisite)
- [4. The Rust side: custom std + target spec](#4-the-rust-side-custom-std--target-spec)
- [5. The GPUI stack](#5-the-gpui-stack)
- [6. Ferail itself (this repo, branch `main`)](#6-ferail-itself-this-repo-branch-main)
- [7. Build → link → deploy → run](#7-build--link--deploy--run)
- [8. Test suites](#8-test-suites)
- [9. Troubleshooting (the ledger - every one of these actually happened)](#9-troubleshooting-the-ledger---every-one-of-these-actually-happened)
- [10. Known limitations (2026-08-01)](#10-known-limitations-2026-08-01)

<!-- /toc -->

## 0. What you end up with

```
macOS process "Macaros" (AROSBootstrap)
└── booted AROS (Kickstart 51.51, Intuition, CyberGraphics, posixc)
    └── C:Ferail  ← a 70 MB ET_REL AROS command containing:
        Ferail (this repo) → gpui (zed fork) → gpui_aros CPU backend
        → tiny-skia rasterizer → Intuition window → WritePixelArray
```

Ferail's full UI, chrome, sidebar, tabs, list/grid, dark theme, browsing
`SYS:` with working keyboard (typeahead included), mouse, and icons.

## 1. Host prerequisites

- Apple Silicon Mac (the hosted AROS target is `darwin-aarch64`; tested on
  macOS 26 / Xcode clang 21).
- Xcode command-line tools.
- Homebrew packages: `llvm` (clang 20+, provides `llvm-ar`, `llvm-strip`,
  `ld.lld`), plus the AROS build chain: `gawk automake autoconf bison flex
  netpbm libpng gnu-sed` and `python3 -m pip install mako`.
- Rust via rustup with the **pinned nightly**:
  `rustup toolchain install nightly-2026-06-27` (the custom std and the
  `-Zjson-target-spec` / `-Zbuild-std` flags are validated against exactly
  this one).
- ~30 GB free disk. AROS-target build artifacts are large (a debug
  staticlib of the app once hit 600 MB before stripping habits set in).

## 2. Checkout layout (sibling paths are load-bearing)

Ferail's `[patch]` entries and `scripts/check-aros.sh` use **relative
sibling paths**, so all five repos must sit next to each other.

> **Nothing to copy.** The sibling-checkout `[patch]` entries live in
> `packaging/aros/aros-patches.toml` (tracked), which `build-aros.sh` and
> `check-aros.sh` pass to cargo via `--config`. They are deliberately *not* in
> the workspace `Cargo.toml`: a relative sibling path there is load-bearing
> for every platform's build, so a machine without these checkouts could not
> even parse the manifest, and deliberately not in `.cargo/config.toml`
> either, because `[patch]` is global to a build: entries there would feed the
> sibling forks to the macOS and Windows builds on the same machine, release
> packaging included, and would shadow the `vendor/sum-tree` patch that severs
> the GPL-3.0 ztracing edge.
>
> The per-machine `.cargo/config.toml` still carries the AROS `[target]` /
> `[env]` C recipe (see § 6). That half *is* machine-specific.
>
> **2026-08-01:** the hand-copy step this section used to describe is what
> broke the AROS build for a day: the patches were moved out of the manifest
> and the local config never received them, so cargo resolved gpui from
> upstream git and the build died on `errno`. Hence: no copying.

```
~/Source/
├── Ferail/                branch main           (this repo: the AROS work
│                                                 merged; `aros-port` is dead)
├── zed-aros/              branch aros-platform  (zed fork + gpui_aros backend)
├── gpui-component-aros/   branch aros-port      (to recreate at e8f54eb, narrow smol→async-channel delta)
├── rust-aros/                                   (Rust std library fork with the AROS pal)
└── aros-aarch64/                                (the hosted-AROS OS project: graft/, hosted/rust/, aros-ctl)
    └── (built against) ~/Source/aros-upstream   branch crash-containment (AROS OS source)
```

Two non-repo trees, produced by step 3 (env-overridable everywhere via
`AROS_BUILD` / `AROS_CROSSTOOLS`):

```
~/aros-build/         the AROS build output: SDK headers, linklibs, collect-aros, boot image
~/aros-crosstools/    the AROS-patched LLVM (clang/lld with the aarch64elf_aros emulation, compiler-rt builtins)
```

> **Do not keep these under `/tmp`.** macOS's periodic cleaner deletes
> files there piecemeal by atime: half the debugging saga in
> aros-port.md § *Boot image damage* was self-inflicted by a `/tmp` build
> tree slowly evaporating.

## 3. Build hosted AROS + its SDK (the big prerequisite)

This is the [aros-aarch64](../../../aros-aarch64/) project's territory; its
`graft/WORKFLOW.md` and `graft/build-darwin-aarch64.sh` are authoritative.
The condensed shape:

1. Check out `aros-upstream` on the graft branch (carries the
   `darwin-aarch64` configure case, the AArch64 Darwin signal glue, and
   the `exec/types.h` storage-class-macro guards Ferail's sqlite build
   needs: commit `5c1666af`).
2. Build the AROS-patched crosstools into `~/aros-crosstools` (LLVM 20
   with the 4-line lld patch adding the `aarch64elf_aros` emulation, plus
   `lib/generic/libclang_rt.builtins-aarch64.a`).
3. Configure + build AROS into `~/aros-build`
   (`configure --target=darwin-aarch64 --with-toolchain=llvm ...`, then
   the metatargets). What Ferail's build actually consumes:
   - `bin/darwin-aarch64/gen/include/`: the SDK headers (~1900 files;
     regenerate anytime with `make includes` in a configured tree),
   - `bin/darwin-aarch64/AROS/Developer/lib/`: `startup.o` + the
     linklibs (`libposixc.a`, `libintuition.a`, `libpthread.a`, …),
   - `bin/darwin-aarch64/tools/collect-aros`: the ET_REL final-link tool,
   - `bin/darwin-aarch64/AROS/boot/darwin/`: the runnable image
     (`AROSBootstrap`/`Macaros`, kickstart modules, `AROS.boot`).
4. Sanity: `cd ~/Source/aros-aarch64 && graft/aros-ctl run` boots a
   desktop; `graft/rust-smoke` and `graft/bench-run C:RustStd` prove the
   no_std and std Rust milestones on the booted OS.

## 4. The Rust side: custom std + target spec

`aarch64-unknown-aros` is not a rustc target; it exists as:

- **Target spec**: `aros-aarch64/hosted/rust/aarch64-unknown-aros.json`
  (aarch64 ELF, `-mcmodel=large`, x18 reserved, static, panic=abort).
  Passed with `--target <path>.json -Zjson-target-spec`.
- **Custom std**: `~/Source/rust-aros`: a rust `library/` tree with an
  AROS pal (posixc-backed fs/threads/net/random, and the path-join
  translation `SYS:/C` → `SYS:C` at the syscall boundary that makes
  directory listings work at all). Wire it into the pinned toolchain by
  symlinking rust-src:

  ```sh
  SYSROOT=$(rustc +nightly-2026-06-27 --print sysroot)
  cd "$SYSROOT/lib/rustlib/src"
  mv rust rust.orig
  ln -s ~/Source/rust-aros rust
  ```

  `-Zbuild-std=std,panic_abort` then compiles *this* std for the target.

## 5. The GPUI stack

- **`zed-aros`** (branch `aros-platform`): rebase target is the exact rev
  Ferail uses (`f66ed399`) plus the port: `crates/gpui_aros` (Intuition
  C glue, tiny-skia CPU renderer, std-thread dispatcher, rawkey keyboard,
  clipboard.device), small cfg-gated core fixes (proptest off-AROS,
  `Background::as_linear_gradient`), and `vendor-aros/` (stacker and
  filetime with AROS arms). Ferail's root `Cargo.toml` `[patch]`
  redirects `gpui`/`gpui_platform` here.
- **`gpui-component-aros`** (branch `aros-port`): rebase target is
  gpui-component `e8f54eb`, with one conceptual delta: `smol::channel` →
  `async-channel` across the new base/UI split,
  keeping the async-io/rustix reactor (no AROS arm) out of the graph.
- The backend has a **host-runnable porting conformance suite**
  (`cargo test -p gpui_aros`, from `~/Source/zed-aros`): run it on any
  machine, no AROS needed. `crates/gpui_aros/PORTING.md` keeps the
  bug→test ledger and the on-device checklist.

## 6. Ferail itself (this repo, branch `main`)

What the AROS port consists of:

- `crates/ferail-aros-app/`: staticlib + C harness + `link-aros.sh` +
  `ferail.startup`. AROS has C own `main()`; the harness feeds
  argc/argv to the std and calls `ferail_aros_main`, which routes into
  the same `ferail_gpui::boot::run_gui` the desktop binary uses.
- `crates/ferail-shell-aros/`: the `platform_shell` arm (v1: the
  shell-linux stub scaffold re-exported).
- `.cargo/config.toml` (force-added; machine-specific paths): the AROS C
  recipe for every `cc`-built dependency: clang ELF triple, large code
  model, x18 reserved, SDK include dirs, `-D_GNU_SOURCE`,
  `-DHAVE_ENDIAN_H` + the compat shims dir (AROS lacks `<endian.h>` /
  `<sys/ioctl.h>`), and the sqlite no-mmap/no-WAL/no-dlopen flags. Adjust
  the `~/aros-build` paths here if your trees live elsewhere.
- `packaging/aros/aros-patches.toml`: the sibling-checkout `[patch]` entries
  (gpui, gpui_platform, gpui-component[-assets]), passed via `--config` so they
  never touch a host build. **Paths in it resolve relative to `packaging/`, not
  to the file's own directory**: cargo's rule for config files.
- `vendor/tar` and `vendor/filetime`: crates with no upstream AROS arm,
  patched from the **workspace manifest** so every target sees the same copy.
  The older AROS GPUI fork's `stacker` shim is now scoped to the AROS Cargo
  config because standard GPUI no longer enables that edge. The global
  placement of filetime remains load-bearing: a version-differing patch that is
  only visible to AROS invocations goes silently inert as soon as a host
  `cargo` command re-resolves the lock to a newer registry version (cargo does
  not downgrade to a patch, it drops it), and the next AROS build fails with
  the original "no libc for this target" errors. This bit twice on 2026-08-01
  before the crates moved in-repo.

## 7. Build → link → deploy → run

```sh
cd ~/Source/Ferail          # branch aros-port

# 1. Type-check gate (fast; catches graph regressions):
scripts/check-aros.sh

# 2+3. Build + link + deploy in one step (preferred -- it also guards
#      against cargo's stale-std trap, see below):
PROFILE=release crates/ferail-aros-app/build-aros.sh
#    -> ~/aros-build/bin/darwin-aarch64/AROS/C/Ferail  (~70 MB)

# ... or the two raw steps it wraps:
cargo +nightly-2026-06-27 build --release -p ferail-aros-app \
  --target ../aros-aarch64/hosted/rust/aarch64-unknown-aros.json \
  -Zjson-target-spec -Zbuild-std=std,panic_abort
PROFILE=release crates/ferail-aros-app/link-aros.sh

# STALE-STD TRAP: cargo does NOT fingerprint the rust-src symlink's
# sources. After editing anything in ~/Source/rust-aros, a raw cargo
# build reuses the stale libstd rlib in ~0.5s and your change silently
# never ships. build-aros.sh detects source-newer-than-rlib and clears
# target/<triple>/release/{.fingerprint/std-*,deps/libstd-*} first.

# 4. Strip (mandatory - LoadSeg relocates the whole ET_REL; debug info
#    turns seconds into minutes):
llvm-strip --strip-debug crates/ferail-aros-app/build/Ferail
cp crates/ferail-aros-app/build/Ferail \
   ~/aros-build/bin/darwin-aarch64/AROS/C/Ferail

# 5. Boot AROS with the Ferail startup and watch it come up:
cd ~/Source/aros-aarch64
AROS_CTL_STARTUP_FILE=~/Source/Ferail/crates/ferail-aros-app/ferail.startup \
  graft/aros-ctl run
graft/aros-ctl shot proof.png        # screenshot the framebuffer
graft/aros-ctl stop
```

`ferail.startup` is three lines of load-bearing:

```
Stack 16000000                  ← REQUIRED. AROS shells give commands ~40 KB;
                                  gpui recursion overflows that, and in a
                                  single-address-space OS the overflow corrupts
                                  *other* tasks (crashes point at emul-handler /
                                  graphics.library, never at the app).
SetEnv HOME "SYS:"              ← Ferail opens $HOME at boot; unset, it lands
                                  on "/" which AROS can't enumerate.
C:Ferail --theme dark --width 780 --height 560
```

Driving it under automation: `graft/aros-ctl type/click/key/shot` (input
travels the same HIDD path as real events). Crash forensics: `obs.rs`'s
panic hook appends the crash report (message, location, breadcrumbs,
backtrace) to `MacRW:ferail-panic.txt` (≈ `~/AROS/Shared/` on the host)
with panic=abort the abort cascades into a deadend + ColdReboot whose
bootstrap re-exec truncates the host log, so stderr never survives. The
`log`-crate bridge similarly lands in `MacRW:ferail-log.txt`. When
chasing a reboot that leaves neither, capture the host log with a
truncation-proof reader (`tail -n +1 -F /tmp/aros-window.log > sidecar`):
exec's Alert() and ShutdownA() leave breadcrumbs there.

## 8. Test suites

| Suite | Where | Command |
| --- | --- | --- |
| Porting conformance (renderer/atlas/input) | zed-aros | `cargo test -p gpui_aros` |
| Ferail host regression | this repo | `cargo test -p ferail-gpui` |
| AROS type-check gate | this repo | `scripts/check-aros.sh` |
| On-device smokes | aros-aarch64 | `graft/rust-smoke`, `graft/bench-run C:RustStd`, `C:GpuiSmoke` |

## 9. Troubleshooting (the ledger - every one of these actually happened)

| Symptom | Cause / fix |
| --- | --- |
| `could not find specification for target` | Missing `-Zjson-target-spec`, or the target JSON path is wrong. |
| std compiles like stock (your fs fixes missing) | The rust-src symlink (step 4) isn't in place, or cargo cached std: nuke `target/<triple>/release/.fingerprint/std-*` + `deps/libstd-*`. |
| C dep fails: `fdopen` undeclared | `-D_GNU_SOURCE` missing from `CFLAGS_aarch64_unknown_aros` (AROS overguards core POSIX). |
| tree-sitter: `platform not supported` (endian.h) | Compat include dir + `-DHAVE_ENDIAN_H` missing. |
| sqlite: `expected expression` at `GLOBAL(...)` | aros-upstream too old: needs the `exec/types.h` macro guards (`5c1666af`). |
| sqlite: `sys/ioctl.h not found` | Compat include dir missing. |
| `errno`/`rustix`/`polling` fail to build ("target OS is not yet supported") | The async-io reactor is back in the graph. Ferail never calls it: gpui takes `http_client` with `default-features = false`, which drops `github_download` and util's smol half. Check that zed-aros still has that (root manifest note on `http_client`) and that no new crate pulls `smol`/`async-std` directly. Do **not** fix this by mirroring zed's ~35 vendored reactor crates. |
| `[patch]` not applied: gpui resolves from github | `--config packaging/aros/aros-patches.toml` missing from the cargo invocation, or a sibling checkout is missing / on the wrong branch. Verify with `cargo tree -i gpui`: it must print a path, not a git URL. |
| stacker `libc::mmap` / filetime `os::unix` errors | The AROS patch is not being used. With the AROS `--config`, `cargo tree -i stacker` must resolve to `zed-aros/vendor-aros/stacker` while the old fork still needs it; `filetime` must resolve to Ferail's `vendor/filetime`. Keep lock versions at stacker 0.1.23 / filetime 0.2.26 until the rebase proves otherwise. |
| `tar` fails with 15 errors in `header.rs` | Same thing for `vendor/tar`: `cargo tree -i tar` must print the vendor path. See [vendor/tar/README.md](../../vendor/tar/README.md). |
| lzma-sys: `pthread_sigmask` undeclared | The AROS build tree predates the `pthread_sigmask` implementation. It was a stub: unimplemented, absent from `compiler/pthread/mmakefile.src`, undeclared in `pthread.h`: so liblzma could not compile. Rebuild the OS side: `make linklibs-pthread` in the configured tree. |
| lzma-sys: `unresolved imports \`libc::c_char\`` | The vendored libc patch is inert. `cargo tree -i libc` (with the AROS `--config`) must print `zed-aros/vendor-aros/libc`; if it prints a registry version, re-pin: `cargo update -p libc --precise 0.2.186`. |
| Link fails with undefined `aros_pipe_*` / `aros_proc_*` / `aros_sig_*` | `link-aros.sh` is missing a glue file from `aros-aarch64/hosted/rust`. That set grows as the std pal does: `aros_proc_glue.c` was added on 2026-08-01. Compare against the `.c` files in that directory. |
| Boot: `Failed to open file FileSystem.resource` | Boot image lost a kickstart module: rebuild just it (`make kernel-filesystem` in a configured tree) and copy in. |
| Boot: `Could not open version 36 ... dos.library` | Wildly misleading. Real cause: the `AROS.boot` signature file is missing from the boot volume root: `echo aarch64 > <bootdir>/AROS.boot`. |
| App loads forever, ~0 CPU | Debug binary (strip it) or a `-DDEBUG` dos.library in the image (logs every packet). |
| App silently wedges before its first output | A disk library it auto-opens is missing from `AROS/Libs` (e.g. partition.library): rebuild + copy in. |
| Random tasks (emul-handler, graphics) crash after input | You launched without `Stack 16000000`. |
| Every folder empty | The rust-aros path-join fix is missing (std symlink again). |
| emul-handler `DoExamineNext` requester under browsing | Known OS-side frontier (its handler stack; UPSTREAM-NOTES item 35). Ferail's recursive folder-size walker is gated off on AROS for this reason; crash containment lets you Suspend and keep the session. |

## 10. Known limitations (2026-08-01)

- Folder sizes read `--` on AROS (walker gated; see above).
- Real platform icons / thumbnails pending (`ferail-shell-aros` is a
  stub scaffold: Lucide glyph fallbacks render instead; icon.library /
  DefIcons integration is the designed next step).
- Dead-key composition disabled.
- **`.tar.xz` decode is unverified on device.** It builds, links and boots
  since `pthread_sigmask` landed (see below), but the archive workbench could
  not be driven through injected input to confirm a real decode: double-click
  does not register even when the four events are written straight to the
  control FIFO (the guest polls on a 33 ms frame), plain right-click raises no
  context menu, and the native Intuition menu arms but will not drop under the
  synthetic hold gesture. Verify by hand, or teach `aros-ctl` a real
  double-click.
- The metadata DB (sqlite) compiles and links; its runtime behavior on
  AROS paths has not been audited.
- `parking_lot_core` is in the graph unpatched. Zed vendors an AROS thread
  parker for it because the upstream generic parker spins in `park()`, and
  AROS round-robins equal-priority tasks, so a parked thread can starve the
  UI. Ferail has not needed it, but it is the first place to look if the UI
  stutters while background work runs.

### Fixed since the 2026-07-06 list

- **`.tar.xz` works.** It used to be gated off: liblzma's `mythread.h` calls
  `pthread_sigmask`, which AROS declared nowhere and never built. That has
  been implemented upstream-side (`compiler/pthread/pthread_sigmask.c`
  forwards to `sigprocmask`, whose mask is already per-task) and added to the
  pthread linklib and header. Every archive format Ferail supports now works
  on AROS. **Runtime decode is not yet confirmed on device**: the build,
  link and boot are, but driving the archive workbench through injected input
  did not work (see the note on double-click below).

- **Window resize works.** The size gadget drag-resizes and the layout
  reflows; verified on device 2026-08-01 (`screenshots/aros-resize-*.png`).
  The earlier "unverified" note predated zed-aros `85b08a3446`, which added
  the `WA_MinWidth`/`MaxWidth` tags Intuition needs before the gadget can
  move at all.
- **No duplicate window controls.** gpui-component's client-side
  minimise/maximise/close are suppressed on AROS (they were inert there
  anyway) so only Intuition's own gadgets show.
- Scroll wheel works end-to-end (the kickstart carries the hidd).

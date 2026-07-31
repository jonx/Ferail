# Building Ferail for AROS — the complete guide

How to go from five git checkouts to **Ferail running on AROS**, on an
Apple Silicon Mac. This documents everything "in between": the AROS OS
build, the cross-toolchain, the custom Rust std, the patched GPUI stack,
and the link/deploy/run loop. It is the reproduction recipe for the state
described in [aros-port.md](aros-port.md) (the map of what lives where and
why).

> **Reality check.** This is a hosted-OS research port. The final target is
> AROS (the open-source AmigaOS re-implementation) running *as a macOS
> process* — `darwin-aarch64` hosted AROS. Nothing here targets real Amiga
> hardware, and several trees are moving research code, not releases.

---

## 0. What you end up with

```
macOS process "Macaros" (AROSBootstrap)
└── booted AROS (Kickstart 51.51, Intuition, CyberGraphics, posixc)
    └── C:Ferail  ← a 70 MB ET_REL AROS command containing:
        Ferail (this repo) → gpui (zed fork) → gpui_aros CPU backend
        → tiny-skia rasterizer → Intuition window → WritePixelArray
```

Ferail's full UI — chrome, sidebar, tabs, list/grid, dark theme — browsing
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

> **First, install the patch config.** The `[patch]` entries do *not* live in
> the workspace `Cargo.toml` — a relative sibling path there is load-bearing for
> every platform's build, so a machine without these checkouts cannot even parse
> the manifest. Copy the template into the gitignored per-machine config
> instead, once per checkout:
>
> ```sh
> cp packaging/aros/cargo-config-aros.toml .cargo/config.toml
> ```
>
> Without this the AROS build resolves gpui/gpui-component from upstream git and
> fails as described in [§ Troubleshooting](#troubleshooting).

```
~/Source/
├── Ferail/              branch aros-port      (this repo)
├── zed-aros/              branch aros-platform  (zed fork + gpui_aros backend)
├── gpui-component-aros/   branch aros-port      (gpui-component @ pinned rev c112e7b, smol→async-channel)
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
> files there piecemeal by atime — half the debugging saga in
> aros-port.md § *Boot image damage* was self-inflicted by a `/tmp` build
> tree slowly evaporating.

## 3. Build hosted AROS + its SDK (the big prerequisite)

This is the [aros-aarch64](../../../aros-aarch64/) project's territory; its
`graft/WORKFLOW.md` and `graft/build-darwin-aarch64.sh` are authoritative.
The condensed shape:

1. Check out `aros-upstream` on the graft branch (carries the
   `darwin-aarch64` configure case, the AArch64 Darwin signal glue, and
   the `exec/types.h` storage-class-macro guards Ferail's sqlite build
   needs — commit `5c1666af`).
2. Build the AROS-patched crosstools into `~/aros-crosstools` (LLVM 20
   with the 4-line lld patch adding the `aarch64elf_aros` emulation, plus
   `lib/generic/libclang_rt.builtins-aarch64.a`).
3. Configure + build AROS into `~/aros-build`
   (`configure --target=darwin-aarch64 --with-toolchain=llvm ...`, then
   the metatargets). What Ferail's build actually consumes:
   - `bin/darwin-aarch64/gen/include/` — the SDK headers (~1900 files;
     regenerate anytime with `make includes` in a configured tree),
   - `bin/darwin-aarch64/AROS/Developer/lib/` — `startup.o` + the
     linklibs (`libposixc.a`, `libintuition.a`, `libpthread.a`, …),
   - `bin/darwin-aarch64/tools/collect-aros` — the ET_REL final-link tool,
   - `bin/darwin-aarch64/AROS/boot/darwin/` — the runnable image
     (`AROSBootstrap`/`Macaros`, kickstart modules, `AROS.boot`).
4. Sanity: `cd ~/Source/aros-aarch64 && graft/aros-ctl run` boots a
   desktop; `graft/rust-smoke` and `graft/bench-run C:RustStd` prove the
   no_std and std Rust milestones on the booted OS.

## 4. The Rust side: custom std + target spec

`aarch64-unknown-aros` is not a rustc target; it exists as:

- **Target spec**: `aros-aarch64/hosted/rust/aarch64-unknown-aros.json`
  (aarch64 ELF, `-mcmodel=large`, x18 reserved, static, panic=abort).
  Passed with `--target <path>.json -Zjson-target-spec`.
- **Custom std**: `~/Source/rust-aros` — a rust `library/` tree with an
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

- **`zed-aros`** (branch `aros-platform`): zed pinned at the exact rev
  Ferail uses (`1d217ee`) plus the port: `crates/gpui_aros` (Intuition
  C glue, tiny-skia CPU renderer, std-thread dispatcher, rawkey keyboard,
  clipboard.device), small cfg-gated core fixes (proptest off-AROS,
  `Background::as_linear_gradient`), and `vendor-aros/` (stacker and
  filetime with AROS arms). Ferail's root `Cargo.toml` `[patch]`
  redirects `gpui`/`gpui_platform` here.
- **`gpui-component-aros`** (branch `aros-port`): the pinned
  gpui-component rev with one delta — `smol::channel` → `async-channel` —
  keeping the async-io/rustix reactor (no AROS arm) out of the graph.
- The backend has a **host-runnable porting conformance suite**
  (`cargo test -p gpui_aros`, from `~/Source/zed-aros`) — run it on any
  machine, no AROS needed. `crates/gpui_aros/PORTING.md` keeps the
  bug→test ledger and the on-device checklist.

## 6. Ferail itself (this repo, branch `aros-port`)

What the branch adds over `main`:

- `crates/ferail-aros-app/` — staticlib + C harness + `link-aros.sh` +
  `ferail.startup`. AROS has C own `main()`; the harness feeds
  argc/argv to the std and calls `ferail_aros_main`, which routes into
  the same `ferail_gpui::boot::run_gui` the desktop binary uses.
- `crates/ferail-shell-aros/` — the `platform_shell` arm (v1: the
  shell-linux stub scaffold re-exported).
- `.cargo/config.toml` (force-added; machine-specific paths) — the AROS C
  recipe for every `cc`-built dependency: clang ELF triple, large code
  model, x18 reserved, SDK include dirs, `-D_GNU_SOURCE`,
  `-DHAVE_ENDIAN_H` + the compat shims dir (AROS lacks `<endian.h>` /
  `<sys/ioctl.h>`), and the sqlite no-mmap/no-WAL/no-dlopen flags. Adjust
  the `~/aros-build` paths here if your trees live elsewhere.
- `[patch]` entries for the sibling checkouts + vendored crates. One lock
  quirk: the vendored stacker is 0.1.23 — if the lockfile ever drifts to
  0.1.24, re-pin with `cargo update -p stacker --precise 0.1.23`.

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

# 4. Strip (mandatory — LoadSeg relocates the whole ET_REL; debug info
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
— with panic=abort the abort cascades into a deadend + ColdReboot whose
bootstrap re-exec truncates the host log, so stderr never survives. The
`log`-crate bridge similarly lands in `MacRW:ferail-log.txt`. When
chasing a reboot that leaves neither, capture the host log with a
truncation-proof reader (`tail -n +1 -F /tmp/aros-window.log > sidecar`) —
exec's Alert() and ShutdownA() leave breadcrumbs there.

## 8. Test suites

| Suite | Where | Command |
| --- | --- | --- |
| Porting conformance (renderer/atlas/input) | zed-aros | `cargo test -p gpui_aros` |
| Ferail host regression | this repo | `cargo test -p ferail-gpui` |
| AROS type-check gate | this repo | `scripts/check-aros.sh` |
| On-device smokes | aros-aarch64 | `graft/rust-smoke`, `graft/bench-run C:RustStd`, `C:GpuiSmoke` |

## 9. Troubleshooting (the ledger — every one of these actually happened)

| Symptom | Cause / fix |
| --- | --- |
| `could not find specification for target` | Missing `-Zjson-target-spec`, or the target JSON path is wrong. |
| std compiles like stock (your fs fixes missing) | The rust-src symlink (step 4) isn't in place, or cargo cached std — nuke `target/<triple>/release/.fingerprint/std-*` + `deps/libstd-*`. |
| C dep fails: `fdopen` undeclared | `-D_GNU_SOURCE` missing from `CFLAGS_aarch64_unknown_aros` (AROS overguards core POSIX). |
| tree-sitter: `platform not supported` (endian.h) | Compat include dir + `-DHAVE_ENDIAN_H` missing. |
| sqlite: `expected expression` at `GLOBAL(...)` | aros-upstream too old — needs the `exec/types.h` macro guards (`5c1666af`). |
| sqlite: `sys/ioctl.h not found` | Compat include dir missing. |
| `errno`/`rustix`/`wait-timeout` fail to build | The `[patch]` graph isn't applied — `.cargo/config.toml` missing (copy `packaging/aros/cargo-config-aros.toml`), or the gpui-component-aros / zed-aros checkouts are missing or on wrong branches. |
| stacker `libc::mmap` errors | Lock resolved 0.1.24 from crates.io — `cargo update -p stacker --precise 0.1.23`. |
| Boot: `Failed to open file FileSystem.resource` | Boot image lost a kickstart module — rebuild just it (`make kernel-filesystem` in a configured tree) and copy in. |
| Boot: `Could not open version 36 ... dos.library` | Wildly misleading. Real cause: the `AROS.boot` signature file is missing from the boot volume root — `echo aarch64 > <bootdir>/AROS.boot`. |
| App loads forever, ~0 CPU | Debug binary (strip it) or a `-DDEBUG` dos.library in the image (logs every packet). |
| App silently wedges before its first output | A disk library it auto-opens is missing from `AROS/Libs` (e.g. partition.library) — rebuild + copy in. |
| Random tasks (emul-handler, graphics) crash after input | You launched without `Stack 16000000`. |
| Every folder empty | The rust-aros path-join fix is missing (std symlink again). |
| emul-handler `DoExamineNext` requester under browsing | Known OS-side frontier (its handler stack; UPSTREAM-NOTES item 35). Ferail's recursive folder-size walker is gated off on AROS for this reason; crash containment lets you Suspend and keep the session. |

## 10. Known limitations (2026-07-06)

- Folder sizes read `--` on AROS (walker gated; see above).
- Real platform icons / thumbnails pending (`ferail-shell-aros` is a
  stub scaffold — Lucide glyph fallbacks render instead; icon.library /
  DefIcons integration is the designed next step).
- Scroll wheel untested end-to-end (the cocoametal control protocol has
  no wheel injection event yet); dead-key composition disabled.
- Window resize via the size gadget unverified for app windows under
  automation.
- The metadata DB (sqlite) compiles and links; its runtime behavior on
  AROS paths has not been audited.

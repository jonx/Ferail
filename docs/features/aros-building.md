# Building Ferail for AROS

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

The recipe for the native AROS build described in
[aros-port.md](aros-port.md): the checkouts, the one script, and what each
stage proves.

<!-- toc depth=2 -->

- [Checkout layout](#checkout-layout)
- [Prerequisites](#prerequisites)
- [The script](#the-script)
- [Stages](#stages)
- [Pins and overrides](#pins-and-overrides)
- [Running it](#running-it)
- [Functional contracts](#functional-contracts)

<!-- /toc -->

## Checkout layout

`packaging/aros/aros-patches.toml` reaches the AROS forks by relative path,
so the layout is load-bearing (the paths resolve relative to `packaging/`):

```
~/Source/
├── Ferail/                         this repository
└── aros-project/
    ├── gpui-aros/                  github.com/pomaros/gpui-aros, main
    ├── gpui-component-aros-0.7.1/  github.com/pomaros/gpui-kit, branch aros-port-0.7.1
    └── aros-crosstools/            the AROS cross C toolchain: clang, ld.lld, llvm-ar
```

The script selects an installed Rust SDK with `AROS_RUST_SDK`. Its manifest
identifies the exact native std, C SDK, compiler tools and linker; these inputs
are verified before and after Cargo. `AROS_CRATE_STORE` names the patched
crate store. `AROS_GPUI_ROOT` selects another GPUI backend checkout, for a
build against a frozen revision while another checkout is being edited.
`AROS_PROJECT` selects the project root.

## Prerequisites

- An AROS build for AArch64: its C SDK (`Developer`) and `collect-aros`.
- The Rust SDK from [pomaros/rust-aros](https://github.com/pomaros/rust-aros):
  `toolchain/integrate.py` installs the official `nightly-2026-08-04` tools,
  the patched std and the native C adapters into a prefix, given the C SDK,
  the cross C toolchain and `collect-aros`. rustup is not a build dependency.
  The std must build native DOS paths: an older std joins `Work:` and a name
  into `Work:/name`, which DOS reads as the parent of the volume root, and
  every folder lists empty.
- The patched crate store: `crates/store.py --output DIR` in the same
  repository fetches the pinned crates.io packages, checks their checksums
  and applies their AROS patches.
- Python 3.11 or newer for the Cargo wrapper.

## The script

```sh
scripts/check-aros.sh                                   # type-check the AROS command
scripts/aros-cargo.sh build --release -p ferail-aros-app  # link C:Ferail
scripts/aros-cargo.sh tree -p ferail-aros-app -i <crate>  # who pulls a crate on AROS
```

`scripts/aros-cargo.sh` runs the installed Cargo with the SDK's target,
linker and precompiled std configuration. It writes `target/aros/toolchain.toml`
with application source overrides, the identified C compiler and `AROS_SDK`,
and resolves the AROS graph into its own lockfile, `packaging/aros/Cargo.lock`:
the patched crates resolve to store paths there, and the root `Cargo.lock`
stays the other platforms' resolution. The target libraries are
consumed without `-Zbuild-std`; application libc and std's locked libc each
use declarations for one native C provider. The `Ferail` command lands in
`target/aarch64-unknown-aros/release/Ferail`.

## Stages

| Stage | Command | Verdict |
| --- | --- | --- |
| Compilation | `scripts/check-aros.sh` | exit 0: every crate of the AROS graph type-checks, the three C glue files compile against the SDK |
| Linking | `scripts/aros-cargo.sh build --release -p ferail-aros-app` | an AROS `ET_REL` AArch64 command; `llvm-nm --undefined-only` prints nothing |
| Execution | the command started from a Shell on a running AROS system with its launch script | the `T:` diagnostic files and the process list |

A stage's verdict does not imply the next one.

## Pins and overrides

- Application libc is rust-aros's native 0.2.190 profile, selected independently
  from std's locked 0.2.189. Both declare native adapter functions; neither
  exports duplicate C providers or weak ENOSYS bodies.
- `packaging/aros/Cargo.lock` must hold libc 0.2.190, memmap2 0.9.10, ctor 1.0.12,
  inventory 0.3.24, parking_lot_core 0.9.12, getrandom 0.4.3,
  backtrace 0.3.76 and libsqlite3-sys 0.28.0. The script re-pins patched
  versions before a run because Cargo otherwise leaves an override unused.
- `packaging/aros/include-override/endian.h` supplies byte-order conversions
  for vendored C. The C flags spell GNU `asm` as `__asm__` for strict C modes.
  Storage-class guards and `pthread_sigmask` come from the C SDK.
- The AROS command is built without the `screenshot-harness` feature.
- Independent `AROS_LINK_DIRS` overrides are rejected: a library correction
  belongs in the SDK generation, so application and native std use the same
  runtime contracts.

## Running it

`crates/ferail-aros-app/ferail.startup` is the launch script: `Stack
16000000`, then `C:Ferail --theme dark --width 780 --height 560`.
Copy `Ferail` to the system's `C:` and run the script from a shell. The
diagnostic files appear in `T:`
(see [aros-port.md § Diagnostics](aros-port.md#diagnostics)).

## Functional contracts

The `aros-functional` example exercises Ferail's production `NativeFs`, file
transfer engine and archive codecs with a dedicated fixture tree. It checks
contents, collision policies, cancellation, enumeration and errors. Archive
coverage includes ZIP passwords, ZIP/TAR and gzip/bzip2/xz round trips, safe
conversion publication, traversal/volume guards, and 7z/LHA extraction. The case
manifest distinguishes the UI's shared rename/delete filesystem primitives
from application APIs; this gate does not exercise GPUI events or native
requesters.

```sh
scripts/aros-cargo.sh --config profile.release.lto=false --config profile.release.strip=false \
  build --release -p ferail-fs-native --features aros-functional --example aros-functional -j2
```

On AROS, `aros-functional NEW_ROOT [SECOND_NEW_ROOT]` runs every case in a
fixture tree under `NEW_ROOT`, which must not exist (a second root on another
volume adds the cross-volume transfers). It prints one
`FERAIL_FUNCTIONAL case=NAME result=PASS|FAIL` line per case of
`crates/ferail-fs-native/examples/aros-functional-cases.json`, and
`FERAIL_CAPABILITY` lines for facilities the system lacks; fixture files stay
in place for inspection. A complete run reports every declared case.
Physical filesystem, UI interaction, persistence and responsiveness need
separate runs with their own fixtures.

Validate the complete output with the bounded guest run's verdict:

```sh
python3 scripts/aros-functional-result.py --log RUN/functional.log \
  --runtime-verdict RUN/runtime-verdict.json --output RUN/functional-result.json
python3 scripts/test-aros-functional-result.py
```

The checker requires every declared case exactly once and one consistent final
summary. Missing/duplicate cases, failures, refused roots and a failed runtime
gate reject the result. Capability records remain explicit in the receipt.
Use a second fixture root on another disposable volume to cover the complete
manifest; a single-volume run does not qualify the cross-volume case. This
check covers filesystem/archive APIs, separately from GUI interaction.

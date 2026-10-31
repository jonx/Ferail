# AROS port

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

> Building it? **[aros-building.md](aros-building.md)** is the recipe. This
> document is the map: what the native AROS port is made of, where each
> piece lives, and which capabilities AROS does and does not provide.

Ferail on native AROS (AArch64): the `Ferail` command, a Rust program linked against the AROS SDK,
with its own GPUI platform backend over Intuition and a shell crate over the
AROS system libraries. AROS is a platform in its own right here: where it
has a facility, Ferail uses it; where it has none, the feature reports
itself unavailable instead of imitating another desktop.

<!-- toc depth=2 -->

- [The pieces and where they live](#the-pieces-and-where-they-live)
- [The toolchain](#the-toolchain)
- [The GPUI backend](#the-gpui-backend)
- [The shell crate](#the-shell-crate)
- [Filesystem and process](#filesystem-and-process)
- [Allocation](#allocation)
- [Profiling](#profiling)
- [Capabilities](#capabilities)
- [Diagnostics](#diagnostics)
- [Validation](#validation)

<!-- /toc -->

## The pieces and where they live

| Piece | Where | Branch |
| --- | --- | --- |
| Ferail with the AROS crates (`ferail-aros-app`, `ferail-shell-aros`, the AROS arms of `ferail-fs-native` and `ferail-gpui`) | this repository | `main` |
| GPUI backend `gpui_aros` and the `gpui-pre-platform` fork that selects it | [pomaros/gpui-aros](https://github.com/pomaros/gpui-aros), checked out at `../aros-project/gpui-aros` | `main` |
| gpui-kit v0.7.1 with the AROS delta (`async-channel`, no client-side window controls) | [pomaros/gpui-kit](https://github.com/pomaros/gpui-kit), checked out at `../aros-project/gpui-component-aros-0.7.1` | `aros-port-0.7.1` |
| Rust for native AROS: the official nightly, the target and std patch, installed as an SDK prefix (`AROS_RUST_SDK`) | [pomaros/rust-aros](https://github.com/pomaros/rust-aros) `toolchain/` | `main` |
| Patched crates (libc, rustix, errno, mio, tempfile, serde, getrandom, parking_lot_core, memmap2, ctor, inventory, sqlite), built into a store (`AROS_CRATE_STORE`) by `crates/store.py` | [pomaros/rust-aros](https://github.com/pomaros/rust-aros) `crates/` | `main` |
| Native C SDK and tools (`Developer`, `collect-aros`) | an AROS build for AArch64 | — |

GPUI itself is the `gpui-pre` 0.3.8 crates.io snapshot and gpui-component
is 0.7.1, the same pins as the desktop builds. The AROS forks are swapped in
only by the AROS scripts, through `packaging/aros/aros-patches.toml`
(`[patch.crates-io]`); the shared manifest carries no AROS path.

## The toolchain

`scripts/aros-cargo.sh` is the single entry point (`check`, `build`, `tree`,
any cargo verb). It runs the installed official pinned nightly with the
precompiled native AROS std, the native target JSON (`aarch64-unknown-aros`, Unix
family, large code model, no PIC, ordinary x18), the patch set, and a
generated configuration with the machine paths. C in the graph (sqlite,
tree-sitter, liblzma, the glue files) is compiled by the crosstools clang as
an AROS driver: `--target=aarch64-unknown-aros --sysroot=<Developer>` gives it
the AROS include order and, at link time, collect-aros with `startup.o`, the
autolibs and compiler-rt. A Rust `bin` links the same way, so the AROS C
start-up hands `argc`/`argv` to std and runs the `.init_array` constructors
GPUI's registries rely on.

The identified SDK provides the storage-class macro guards and native
`pthread_sigmask`. `packaging/aros/include-override/endian.h` adds byte-order
conversions expected by vendored C, and GNU `asm` is spelled `__asm__` for
strict C modes. Application libc 0.2.190 uses the core's native profile and
shares one C adapter provider with std's locked libc; no Rust crate exports
weak replacement C bodies. The wrapper verifies the installed toolchain and
its external inputs, and re-pins patched crates before each run.

## The GPUI backend

`gpui_aros` implements GPUI's `Platform`, `PlatformWindow`, `PlatformAtlas`
and dispatcher: Intuition windows and IDCMP input, a tiny-skia CPU
rasterizer blitted with `WritePixelArray` (one device pixel per point, dirty
rectangle per frame), cosmic-text with the bundled IBM Plex Sans and Lilex,
GadTools menus, ASL file requesters, Intuition pointers and
`clipboard.device` text. Its README states the platform contract and what it
reports as unavailable (idle-sleep inhibition, sleep and wake notifications,
window visibility beyond open or closed). The portable half (renderer,
atlas, damage, input mapping) runs its conformance tests on any host.

The optional `aros-native-gpu` command feature selects WGPU/GLES hardware
rendering through native Intuition handles and `egl.library`. Ordinary logical
windows present the rendered image through AROS's native bitmap composition,
which controls window overlap, scaling and the software pointer. This path
copies GPU results to the window bitmap on the CPU. Accelerated desktop scene
composition is a separate platform capability. The default tiny-skia path is
retained for VM use. See [the GPU build command](aros-building.md#building).

Keyboard shortcuts use Ctrl: GPUI's `secondary` token is Cmd only on macOS,
and the backend maps the Amiga keys to GPUI's `platform` modifier.

## The shell crate

`ferail-shell-aros` exposes the `platform_shell` surface the other shells
do, over the AROS libraries (`c/shell_glue.c`):

| Need | AROS facility |
| --- | --- |
| open a file with its application, reveal a path | `workbench.library` `OpenWorkbenchObject`: the default tool, or the drawer window |
| open a URL | `openurl.library`: the configured browser or mailer |
| open a shell in a directory | DOS `SystemTags("NewShell")` with that directory current, or the custom program from the terminal preference |
| choose a folder | `asl.library` drawer requester |
| copy text | `clipboard.device` unit 0, IFF FTXT (ISO-8859-1) through `iffparse.library` |
| duplicate, link, zip | `std::fs`; DOS soft links through `symlink`; the shared zip writer |

## Filesystem and process

- **Volumes.** `ferail-fs-native` lists the volumes DOS has mounted from the
  DOS list, with size, free space, write protection and filesystem type from
  `Info()` (`c/aros_volumes.c`). A volume whose medium is out is skipped, so
  no "please insert volume" requester can appear; the process also runs with
  `pr_WindowPtr = -1`, so DOS answers probes of missing devices with an
  error. Per-volume `Trashcan` drawers are the trash.
- **Locations.** The sidebar lists `SYS:`, `RAM:` and the standard `SYS:`
  drawers that exist.
- **Name encoding.** AROS file names are ISO-8859-1 bytes, as the Shell,
  Wanderer, the keymaps and the fonts read them. `paths::os_to_display`
  decodes a name for display (every byte is a character, so no name is
  skipped) and `paths::display_to_os` encodes a typed name back; a name
  with a character outside ISO-8859-1, a colon or a slash is refused,
  since DOS reads `:` as a volume and `/` as the parent.
- **Home.** `home_dir()` is `HOME:` resolved to the drawer it names: std
  joins a child of a bare assign as `HOME:/name`, which DOS reads as the
  parent of the volume.
- **Per-user files.** Preferences are `HOME:Settings/Ferail/` and the
  metadata database `HOME:Data/Ferail/metadata.db`, under AROS's per-user
  `HOME:` assign (the user's home on the data volume, never `SYS:`, which
  a system update replaces as a whole). The assign is the whole contract:
  no environment variable is read and there is no fallback to `SYS:` or
  `RAM:`; without `HOME:` nothing is persisted and the database is in
  memory, both reported. The bundled SQLite carries two AROS changes
  (`crates/libsqlite3-sys-0.28.0-aros.diff` in pomaros/rust-aros): a
  path with a device or assign name is absolute to its VFS instead of
  being prefixed with the current directory, and a read beyond the end of
  the file returns nothing without seeking, because the C library's
  `lseek` past the end extends the file with zeros at once. The database
  opens through the `unix-none` VFS: the C library has no POSIX advisory
  locks, and one process owns the database.
- **Paths.** AROS paths are byte strings; a `PathBuf::join` of a device
  root yields `DEV:/x`, which DOS reads as the parent of the root, so every
  path handed to DOS drops the slash after the colon.
- **Stack.** GPUI needs megabytes of stack. The command re-runs its own
  segment list on a 16 MB stack through `RunCommand` when started with less;
  `ferail.startup` sets `Stack 16000000` up front.
- **Metadata.** Timestamps are not editable (no `utimes` in the AROS C
  library); there is no group database, so a group shows as its number.
- **Process exit.** The process ends when `main` returns: the AROS C
  start-up cancels the helper threads, waits a bounded time, and parks any
  thread that never reaches a cancellation point (one looping over
  `thread::sleep`, which is a `DoIO` on `timer.device`), leaking its stack
  on purpose rather than holding the exit. A helper thread cannot end the
  process: `exit` from a thread jumps to that thread's own exit point and
  ends the thread only. The shutdown watchdog therefore writes its report
  and stops there on AROS instead of forcing an exit; the freeze watchdog
  ends by itself once shutdown is flagged. `crates/aros-exit-probe`
  (`ExitProbe plain|parked|sleeper|thread-exit`) is the conformance probe
  for this contract: each mode prints `main done` and the caller observes
  whether the process ended.

## Allocation

The C library's `malloc` is exec's pooled allocator behind a semaphore and
Forbid/Permit, and on AArch64 AROS every Permit runs a supervisor check
and a scheduler pass; GPUI's layout makes thousands of small allocations per
frame, and a sampling profile of the UI thread put two thirds of its busy
time there. `crates/aros-alloc` is the command's global allocator: requests
up to 2 KB with alignment up to 16 come from per-class free lists refilled
from 256 KB slabs, freed blocks return to their class, and larger or more
aligned requests go to the C library. A long-lived process keeps its
working set; nothing is returned to the system.

## Profiling

`GPUI_AROS_PROFILE=<file>` makes the backend sample the UI thread's program
counter every millisecond from its exec register frame, with the
frame-pointer chain and whether the task was waiting, and write the samples
with a module map at exit and every thirty seconds;
`scripts/aros-profile-symbolize.py PROFILE BINARY --stacks` names them
against the unstripped command and, with `AROS_MODULE_ROOT` naming the
AROS build the system ran, the ROM module ELFs. The
backend also logs frame statistics every two seconds (GPUI draw, raster and
blit time, damaged pixels, time per primitive kind, scene size, text calls).
The sluggishness of the first native runs was measured and removed in that
order: the damage diff, the renderer's clipping and glyph tinting, then
the allocator ([aros-building.md](aros-building.md) for the recipe).

## Capabilities

Unavailable on AROS and reported as such, never as a silent success: the
update check (no HTTP client in the AROS graph; no menu entry, no Settings
toggle), Open With candidates (no application registry), thumbnails and type
icons (type glyphs instead; `icon.library` and datatypes are the candidate
providers), file-URL clipboard, Finder tags, Quick Look, idle-sleep
inhibition, system theme and power observers, volume observers (the volume
list is rescanned on demand), native video overlay, process lock
diagnostics and elevation, window docking, Windows and WSL roots, headless
screenshots (the `--screenshot` harness is macOS and Windows; AROS is
observed on the running system), and the forced
exit of the shutdown watchdog (a helper thread cannot end the process).

Video posters come from `C:FFThumb` when that command is installed; the
viewer shows "No preview available" otherwise.

## Diagnostics

The log, panic and hang files go to the `T:` assign (`ferail-log.txt`,
`ferail-panic.txt`, `ferail-hang.txt`), or to the directory named by
`FERAIL_LOG_DIR` for a run whose log must outlive a reboot. The `log` crate
facade is bridged into the same sink at Info level; `FERAIL_LOG_TRACE=1`
raises it to Trace. Nothing is written to the console the command was
started from: on AROS that console covers the application's screen.

## Validation

Three stages, each with its own verdict: compilation
(`scripts/check-aros.sh`), linking (`scripts/aros-cargo.sh build --release -p
ferail-aros-app`, an AROS `ET_REL` command with no undefined symbol), and
execution on a running AROS system, started from a Shell with the launch
script. A run is judged from the `T:` diagnostic files and the process list,
never from the screen alone. The GPUI smoke command (`gpui_aros_smoke` in
pomaros/gpui-aros) is the minimal consumer for the execution stage: one
window, every renderer primitive, keyboard echo; `ExitProbe` is the
process-exit probe.

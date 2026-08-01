# Vendored `tar` — AROS platform arms

This is [`tar` 0.4.46](https://crates.io/crates/tar) (MIT/Apache-2.0), copied
verbatim from the crates.io registry, plus **AROS arms for the handful of
functions upstream gates on `cfg(unix)` / `cfg(windows)` / `wasm32`**. The
crate's own README is preserved as `README.upstream.md`.

Patched in from the workspace manifest:

```toml
[patch.crates-io]
tar = { path = "vendor/tar" }
```

## Why this exists

`aarch64-unknown-aros` is none of `unix`, `windows` or `wasm32`, and every
platform helper in `tar` is written as an exhaustive match over exactly those
three. On AROS no arm applies, so the crate does not merely lose a feature — it
fails to compile, 15 errors deep:

```text
error[E0432]: unresolved import `crate::header::path2bytes`
error[E0425]: cannot find function `bytes2path` in this scope
error[E0425]: cannot find function `symlink` in this scope
error[E0599]: no method named `fill_platform_from` found for `&mut Header`
...
```

Nothing about tar's *format* handling is platform-specific — this is entirely
`OsStr`↔bytes conversion and the metadata setters. Hence arms rather than a
reimplementation: the block, checksum and pax code that actually moves user
data stays byte-identical to upstream.

## The delta, in full

Every change is additive and behind `target_os = "aros"`. **No existing arm is
modified**, so behaviour on macOS, Windows, Linux and wasm is unchanged.

| File | Change |
|---|---|
| `Cargo.toml` | `[lints.rust.unexpected_cfgs]` teaching rustc that `target_os = "aros"` is a real value — it is a custom-JSON target, so check-cfg would otherwise warn on every host build |
| `src/header.rs` | `use std::os::aros::prelude::*` alongside the unix/windows imports |
| `src/header.rs` | `DETERMINISTIC_TIMESTAMP` cfg widened to include AROS |
| `src/header.rs` | new `fill_platform_from` arm (see below) |
| `src/header.rs` | `ends_with_slash`, `path2bytes`, `bytes2path` cfgs widened to include AROS |
| `src/entry.rs` | `symlink`, `_set_ownerships`, `_set_perms`, `set_xattrs` — AROS added to the **existing** no-op / unsupported fallback arms |

`src/builder.rs` needed nothing: its `cfg(unix)` blocks already have
`cfg(not(unix))` fallbacks, which AROS takes.

### Why the path helpers can share the unix arms

rust-aros exposes `std::os::aros::ffi::{OsStrExt, OsStringExt}` — literally the
unix implementation, `#[path]`-included from the same source file — so
`as_bytes()` / `from_vec()` behave identically and `path2bytes` / `bytes2path`
are the same one-liners. AROS paths are bytes, like unix paths.

### Why `fill_platform_from` needed its own arm

Two reasons the unix arm cannot be shared:

- **No `uid`/`gid`.** AROS is single-user and its `MetadataExt` exposes neither,
  so ownership is recorded as `root:root` (0/0).
- **No libc constants.** The unix arm classifies entries by matching
  `meta.mode() & libc::S_IFMT` against `S_IFREG` / `S_IFLNK` / …, and AROS is
  not `cfg(unix)`, so `libc` is an empty module there. The AROS arm reads
  `meta.file_type()` through std instead — available, and independent of how
  the AROS pal happens to encode mode bits.

`mtime` and `mode` do come from `std::os::aros::fs::MetadataExt`, which has
both.

### Unsupported operations

`symlink`, ownership, permission and xattr setting are no-ops or return
"Not implemented", reusing upstream's own wasm/windows fallbacks. AROS has no
POSIX symlinks, uids or xattrs reachable through std; extraction succeeds and
simply does not carry metadata the OS cannot represent.

## Re-syncing on a tar bump

1. Copy the new version over this directory from
   `~/.cargo/registry/src/*/tar-<version>/`, then restore this README and
   `README.upstream.md`.
2. Re-apply the table above — `grep -n 'target_os = "aros"' src/*.rs` on the old
   copy lists every insertion point.
3. Bump the `tar` requirement in `crates/ferail-fs-native/Cargo.toml` and check
   the patch still applies: `cargo tree -i tar` must show this path, not the
   registry. A version mismatch makes cargo silently ignore the patch and the
   AROS build fails exactly as above.
4. Rebuild for AROS: `PROFILE=release crates/ferail-aros-app/build-aros.sh`.

## Licence

Unchanged from upstream: MIT OR Apache-2.0. `LICENSE-MIT` and `LICENSE-APACHE`
are the files as shipped by the crate.

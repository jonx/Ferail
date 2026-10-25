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

AROS is a Unix-family target (the native toolchain's std is the shared Unix
implementation with AROS arms), so the crate's `cfg(unix)` arms serve it:
path/bytes conversion, symlinks, permissions and the header fill. Two unix
arms call into C the AROS library does not have and are excluded there, with
the existing portable stub taking over:

| File | Change |
| --- | --- |
| `src/entry.rs` | `_set_ownerships`: the unix arm (`fchown`/`lchown`) is `not(target_os = "aros")`; ownership is not restored on the single-user AROS (the stub returns `Ok`). |
| `src/entry.rs` | `set_xattrs`: the unix arm (the `xattr` crate) is `not(target_os = "aros")`; the stub returns `Ok`. |

Everything else is byte-identical to upstream 0.4.46. Behaviour on macOS,
Windows, Linux and wasm is unchanged.

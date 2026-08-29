# Vendored `block` 0.1.6

This is upstream [`SSheldon/rust-block`](https://github.com/SSheldon/rust-block)
0.1.6 with one compatibility correction in `src/lib.rs`.

Upstream declares `_NSConcreteStackBlock` using an empty Rust enum. Empty
enums are uninhabited, and Rust's `uninhabited_static` future-incompatibility
lint is becoming a hard error. The vendored copy models the opaque runtime
class with the same 32-pointer layout used by `block2` 0.6 instead. Public API
and block behavior are unchanged.

Remove this patch once the GPUI macOS dependency graph has migrated its
remaining `block` users to `block2`/`objc2`.

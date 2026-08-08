//! No-op `#[instrument]` for the `ztracing` clean-room stub.
//!
//! Accepts any attribute arguments and emits the item unchanged — the same
//! observable behaviour upstream has outside Zed's profiling builds.

use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn instrument(_args: TokenStream, item: TokenStream) -> TokenStream {
    item
}

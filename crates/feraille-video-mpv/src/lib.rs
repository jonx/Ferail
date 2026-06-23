//! mpv (libmpv) video provider for Feraille's viewer.
//!
//! A sibling of `feraille-video-vlc` implementing the same
//! [`feraille_core::video`] seam, but built on libmpv's **software render
//! API**: rather than libvlc's push callbacks, frames are *pulled* by calling
//! `mpv_render_context_render` into a BGRA buffer we own — the exact
//! windowless model the viewer wants (`copy_frame → (w, h, BGRA)`).
//!
//! Why mpv over VLC (see docs/features/VIDEO-MPV.md): libmpv's filter chain is
//! settable **live** (`vf` property), so colour grade, enhancement, and the
//! transparent-colour key all compose into one runtime filtergraph with no
//! stream re-open — which is also what a *live* chroma-key picker needs. The
//! Phase 0 spike (`spikes/mpv-probe/`) verified SW render emits a real alpha
//! channel from a `colorkey` filter, so keying lives in mpv's chain.

mod imp;

pub use imp::backend;

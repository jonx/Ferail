//! Clean-room, no-op stand-in for zed's `ztracing` crate.
//!
//! Upstream `ztracing` (GPL-3.0-or-later) is a thin switch: with Zed's
//! `--cfg ztracing` profiling flag it forwards to `tracing` + a Tracy
//! subscriber; without it (every Ferail build) it exposes the same names as
//! no-ops. This crate reproduces only that public no-op surface, written
//! from the API contract, so linking it introduces no GPL-derived code.
//!
//! Surface consumers (gpui, sum_tree) can use:
//! - `#[ztracing::instrument]` attribute (any args) — emits the item as-is.
//! - `trace_span! / debug_span! / info_span! / warn_span! / error_span! /
//!   span! / event!` — swallow their arguments, yield a [`Span`].
//! - [`Span`] with `current()`, `enter()`, `record()` — all inert.
//! - `Level` / `field` — re-exported from `tracing` (MIT), as upstream does.
//! - [`init()`] — does nothing.

pub use tracing::{Level, field};

pub use ztracing_stub_macros::instrument;

/// Inert span handle. Upstream's non-profiling build exposes the same
/// unit-like type; nothing observes it.
pub struct Span;

impl Span {
    pub fn current() -> Self {
        Span
    }

    pub fn enter(&self) {}

    pub fn record<K, V>(&self, _key: K, _value: V) {}
}

/// Swallows any macro arguments and yields a [`Span`]. Each of the span /
/// event macro names below is an alias of this.
#[macro_export]
macro_rules! __noop_span {
    ($($args:tt)*) => {
        $crate::Span
    };
}

pub use __noop_span as debug_span;
pub use __noop_span as error_span;
pub use __noop_span as event;
pub use __noop_span as info_span;
pub use __noop_span as span;
pub use __noop_span as trace_span;
pub use __noop_span as warn_span;

/// Profiling-subscriber setup in upstream; nothing to set up here.
pub fn init() {}

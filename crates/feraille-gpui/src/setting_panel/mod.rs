//! Local fork of `gpui-component`'s `setting` module.
//!
//! Provenance: vendored from gpui-component rev `c112e7b`
//! (`crates/ui/src/setting/`), Apache-2.0 licensed.
//!
//! Forked for one reason: upstream's [`Settings`] splits sidebar|content
//! with its `resizable` panel group, which rescales panels
//! proportionally one frame *after* a window resize — right-anchored
//! controls jumped left↔right and wrapped text reflowed every frame
//! during a live resize (the same upstream bug documented on
//! [`crate::splitter`]). A settings sidebar doesn't need user resizing
//! at all, so this fork replaces the resizable split with a fixed-width
//! sidebar + flex content row: the layout engine absorbs the window
//! delta in the same frame, with no correction pass.
//!
//! Two other deviations from upstream, both cosmetic:
//!
//! - `rust_i18n::t!("Settings.…")` labels are replaced with their
//!   English literals so Feraille doesn't take the `rust_i18n`
//!   dependency.
//! - `use crate::{…}` imports become `use gpui_component::{…}` since
//!   the code now lives outside that crate.
//!
//! The API mirrors upstream gpui-component's `setting` module — same
//! types, same public surface (`Settings`, `SettingPage`,
//! `SettingGroup`, `SettingItem`, `SettingField`, `SelectIndex`,
//! `RenderOptions`, …) — so swapping between the two is import-only.

// Lint policy: this module is a pinned fork — keeping the diff
// against upstream gpui-component reviewable beats lint cleanliness,
// so style lints that would rewrite inherited code are allowed here.
// Correctness lints stay on. Don't add these allows elsewhere.
#![allow(
    clippy::new_without_default,
    clippy::type_complexity,
    clippy::question_mark,
    clippy::needless_borrow,
    clippy::unused_enumerate_index,
    clippy::len_zero,
    clippy::let_and_return,
    clippy::unwrap_or_default,
    clippy::redundant_static_lifetimes,
    clippy::ptr_arg,
    clippy::needless_return,
    clippy::single_match,
    clippy::unnecessary_map_or
)]

mod fields;
mod group;
mod item;
mod page;
mod settings;

pub use fields::*;
pub use group::*;
pub use item::*;
pub use page::*;
pub use settings::*;

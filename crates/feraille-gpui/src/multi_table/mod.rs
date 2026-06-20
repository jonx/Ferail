//! Local fork of `gpui-component`'s table/virtual-list.
//!
//! Forked so Feraille controls row-event behavior directly.
//! Key difference from upstream: `TableEvent::RowClicked` carries the
//! original `Modifiers` from the click event (Cmd, Shift, etc.) and
//! `TableEvent::LeadMoved` carries `window.modifiers()` at dispatch
//! time, letting the Shell drive modifier-aware multi-select without
//! reading `window.modifiers()` at subscription time.
//!
//! ## Future evolutions this fork enables
//!
//! - **Drag-select rubber-banding** — `on_mouse_down` + `on_mouse_move`
//!   to sweep-select a row range by dragging, emitting new events as the
//!   mouse moves.
//! - **Press-on-selected drag delay** — spec §3.3 requires mouse-down on
//!   a selected row to wait for a drag threshold before collapsing
//!   selection, which upstream's instant `SelectRow` cannot express.
//! - **Custom drag image / payload** — attach a multi-row drag badge
//!   with a count chip; upstream only supports single-row `on_drag`.
//! - **Empty-area click** — left-click on blank space below the last
//!   row to clear selection (spec §2.4); requires a hit-test gap the
//!   upstream table doesn't expose.
//! - **Per-cell click intercept** — modifier-aware cell clicks for
//!   future inline rename or cell-level context menus.

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
    clippy::redundant_static_lifetimes
)]

use gpui::App;

mod actions;
mod column;
mod data_table;
mod delegate;
mod loading;
mod state;
mod table;
mod virtual_list;

pub use column::*;
pub use data_table::*;
pub use delegate::*;
pub use state::*;
pub use table::*;
pub(crate) use virtual_list::{VirtualListScrollHandle, v_virtual_list};

pub(crate) fn init(cx: &mut App) {
    data_table::init(cx);
}

fn measure_enable() -> bool {
    false
}

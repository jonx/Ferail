//! Context menu (NSMenu) integration. Synchronously shows a menu at
//! the cursor and returns the user's pick as a [`CommandId`].
//!
//! Mirrors the menu-bar plumbing in [`crate::app_menu`]: each item
//! carries a tag, the tag indexes a thread-local table populated for
//! the duration of the show, and an Objective-C `MenuTarget`'s
//! `itemClicked:` selector resolves the tag back to the command.
//!
//! Stage A: only `Action` and `Separator` items. Submenus and custom
//! views (Open With submenu, Tags row) land in their respective
//! later stages without changing this module's call shape.

use std::cell::{Cell, RefCell};

use feraille_core::commands::{CommandId, CommandPayload};
use objc2::declare_class;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::{Allocated, Retained};
use objc2::sel;
use objc2::{ClassType, DeclaredClass};
use objc2_app_kit::{NSMenu, NSMenuItem, NSView};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Single item in a context-menu plan.
#[derive(Clone, Debug)]
pub enum MenuPlanItem {
    /// A clickable row that fires `command` when picked.
    Action {
        command: CommandId,
        /// Display title. May differ from `CommandSpec.title` so call
        /// sites can render dynamic counts ("Reveal 5 in Finder").
        title: String,
        /// Greyed-out items still display but can't be picked.
        enabled: bool,
        /// Show a leading checkmark. Used by tag rows to indicate
        /// "this colour is currently set on the selection".
        checked: bool,
        /// Optional payload threaded back to the host alongside the
        /// `CommandId` (e.g. which tag colour was picked).
        payload: Option<CommandPayload>,
    },
    Separator,
    /// Nested submenu (Open With, future async-populated lists).
    /// The submenu's items can themselves be `Action`s — when one
    /// is picked, the resulting [`MenuPick`] threads through with
    /// that action's command + payload.
    Submenu {
        title: String,
        items: Vec<MenuPlanItem>,
    },
    /// AppKit-owned Services / Quick Actions submenu. At render
    /// time we attach `NSApp.servicesMenu` directly so AppKit's
    /// auto-population covers Quick Actions registered via
    /// Automator workflows. No-op if [`crate::services::install`]
    /// hasn't run yet.
    ServicesSubmenu { title: String },
}

impl MenuPlanItem {
    pub fn action(command: CommandId, title: impl Into<String>) -> Self {
        MenuPlanItem::Action {
            command,
            title: title.into(),
            enabled: true,
            checked: false,
            payload: None,
        }
    }

    /// Action carrying a [`CommandPayload`]. The payload comes back
    /// to the call-site via [`MenuPick::payload`] when this item is
    /// chosen.
    pub fn action_with_payload(
        command: CommandId,
        title: impl Into<String>,
        payload: CommandPayload,
    ) -> Self {
        MenuPlanItem::Action {
            command,
            title: title.into(),
            enabled: true,
            checked: false,
            payload: Some(payload),
        }
    }

    /// Builder-style toggle for the leading checkmark.
    pub fn checked(mut self, on: bool) -> Self {
        if let MenuPlanItem::Action {
            ref mut checked, ..
        } = self
        {
            *checked = on;
        }
        self
    }

    pub fn separator() -> Self {
        MenuPlanItem::Separator
    }

    pub fn submenu(title: impl Into<String>, items: Vec<MenuPlanItem>) -> Self {
        MenuPlanItem::Submenu {
            title: title.into(),
            items,
        }
    }

    pub fn services_submenu(title: impl Into<String>) -> Self {
        MenuPlanItem::ServicesSubmenu {
            title: title.into(),
        }
    }
}

/// Ordered list of items to show. Build one per right-click; pass
/// to [`show_context_menu`] which renders an `NSMenu` and blocks
/// until the user picks or dismisses.
#[derive(Clone, Debug, Default)]
pub struct MenuPlan {
    pub items: Vec<MenuPlanItem>,
}

impl MenuPlan {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: MenuPlanItem) -> &mut Self {
        self.items.push(item);
        self
    }
}

/// What the user picked. `command` is the catalogue id; `payload`
/// is whatever the call-site attached at plan-build time (None for
/// plain actions, `Some(CommandPayload::Tag(_))` for tag rows, …).
#[derive(Clone, Debug)]
pub struct MenuPick {
    pub command: CommandId,
    pub payload: Option<CommandPayload>,
}

thread_local! {
    /// Tag of the most-recently-clicked menu item. `-1` = user
    /// dismissed (Esc, click outside, no pick). Reset before every
    /// `show_context_menu` and read after.
    static CLICKED_TAG: Cell<i32> = const { Cell::new(-1) };

    /// Maps menu-item tag → `(CommandId, Option<CommandPayload>)`
    /// for the duration of one show. Repopulated each call so we
    /// don't grow unbounded across the app's lifetime.
    static TAG_TO_PICK: RefCell<Vec<(CommandId, Option<CommandPayload>)>> =
        const { RefCell::new(Vec::new()) };
}

declare_class!(
    pub struct MenuTarget;

    unsafe impl ClassType for MenuTarget {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FeraMenuTarget";
    }

    impl DeclaredClass for MenuTarget {
        type Ivars = ();
    }

    unsafe impl MenuTarget {
        #[method(itemClicked:)]
        fn item_clicked(&self, sender: &NSMenuItem) {
            let tag = unsafe { sender.tag() };
            CLICKED_TAG.with(|t| t.set(tag as i32));
        }
    }
);

unsafe impl NSObjectProtocol for MenuTarget {}

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let alloc: Allocated<Self> = mtm.alloc();
        unsafe { msg_send_id![alloc, init] }
    }
}

/// Show a context menu at the mouse cursor and return the picked
/// command (with any attached payload). Blocks the calling thread
/// while the menu is open. Returns `None` if the user dismissed
/// without choosing.
pub fn show_context_menu(
    window: &winit::window::Window,
    plan: MenuPlan,
    cursor_dips: (f32, f32),
) -> Option<MenuPick> {
    let mtm = MainThreadMarker::new()?;
    let Ok(handle) = window.window_handle() else {
        return None;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return None;
    };
    let ns_view_ptr = h.ns_view.as_ptr();
    if ns_view_ptr.is_null() {
        return None;
    }

    CLICKED_TAG.with(|t| t.set(-1));
    TAG_TO_PICK.with(|cell| cell.borrow_mut().clear());

    let target = MenuTarget::new(mtm);

    unsafe {
        let ns_view: &NSView = &*(ns_view_ptr as *const NSView);
        let menu = build_ns_menu(mtm, &target, plan.items);

        // winit's NSView is flipped (top-left origin), so the dips
        // are already the right coordinate space.
        let location = NSPoint::new(cursor_dips.0 as f64, cursor_dips.1 as f64);
        let _: bool = objc2::msg_send![
            &*menu,
            popUpMenuPositioningItem: std::ptr::null::<NSMenuItem>(),
            atLocation: location,
            inView: ns_view,
        ];
    }

    let tag = CLICKED_TAG.with(|t| t.get());
    if tag < 0 {
        return None;
    }
    TAG_TO_PICK.with(|cell| {
        cell.borrow()
            .get(tag as usize)
            .map(|(c, p)| MenuPick {
                command: *c,
                payload: p.clone(),
            })
    })
}

/// Recursively turn a list of [`MenuPlanItem`]s into an `NSMenu`.
/// Submenus recurse into themselves; tags grow monotonically across
/// the whole tree so each picked item resolves uniquely.
unsafe fn build_ns_menu(
    mtm: MainThreadMarker,
    target: &Retained<MenuTarget>,
    items: Vec<MenuPlanItem>,
) -> Retained<NSMenu> {
    use objc2_app_kit::{NSControlStateValueOff, NSControlStateValueOn};

    let menu = NSMenu::new(mtm);
    // Disable autoenabling so disabled `Action`s actually grey out
    // even when the menu is opened modally; AppKit's default tries
    // to second-guess us via responder chain.
    menu.setAutoenablesItems(false);
    let empty_key = NSString::from_str("");

    for item in items {
        match item {
            MenuPlanItem::Separator => {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
            }
            MenuPlanItem::Action {
                command,
                title,
                enabled,
                checked,
                payload,
            } => {
                let tag = TAG_TO_PICK.with(|cell| {
                    let mut v = cell.borrow_mut();
                    let idx = v.len();
                    v.push((command, payload));
                    idx
                });
                let ns_title = NSString::from_str(&title);
                let action = if enabled {
                    Some(sel!(itemClicked:))
                } else {
                    None
                };
                let ns_item: Retained<NSMenuItem> = msg_send_id![
                    mtm.alloc::<NSMenuItem>(),
                    initWithTitle: &*ns_title,
                    action: action,
                    keyEquivalent: &*empty_key,
                ];
                ns_item.setTag(tag as isize);
                if enabled {
                    ns_item.setTarget(Some(&**target));
                }
                ns_item.setEnabled(enabled);
                let state = if checked {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                };
                ns_item.setState(state);
                menu.addItem(&ns_item);
            }
            MenuPlanItem::Submenu { title, items } => {
                let ns_title = NSString::from_str(&title);
                let header_item: Retained<NSMenuItem> = msg_send_id![
                    mtm.alloc::<NSMenuItem>(),
                    initWithTitle: &*ns_title,
                    action: None::<objc2::runtime::Sel>,
                    keyEquivalent: &*empty_key,
                ];
                let submenu = build_ns_menu(mtm, target, items);
                submenu.setTitle(&ns_title);
                header_item.setSubmenu(Some(&submenu));
                menu.addItem(&header_item);
            }
            MenuPlanItem::ServicesSubmenu { title } => {
                // Allocate a fresh empty NSMenu and re-publish it
                // as `NSApp.servicesMenu`. AppKit's "this menu's
                // already been populated" gate keys off the menu
                // identity, so a fresh instance forces a complete
                // responder-chain walk against the *current*
                // selection rather than reusing whatever stale
                // layout state lingered from a prior show.
                let Some(services_menu) = crate::services::refresh_services_menu() else {
                    // Anchor not installed yet — render an inert
                    // header item so the right-click site doesn't
                    // need to know about install order.
                    let ns_title = NSString::from_str(&title);
                    let stub: Retained<NSMenuItem> = msg_send_id![
                        mtm.alloc::<NSMenuItem>(),
                        initWithTitle: &*ns_title,
                        action: None::<objc2::runtime::Sel>,
                        keyEquivalent: &*empty_key,
                    ];
                    stub.setEnabled(false);
                    menu.addItem(&stub);
                    continue;
                };
                let ns_title = NSString::from_str(&title);
                let header_item: Retained<NSMenuItem> = msg_send_id![
                    mtm.alloc::<NSMenuItem>(),
                    initWithTitle: &*ns_title,
                    action: None::<objc2::runtime::Sel>,
                    keyEquivalent: &*empty_key,
                ];
                services_menu.setTitle(&ns_title);
                // Belt and braces: even with a fresh menu, force
                // synchronous validation before AppKit measures
                // the submenu's frame.
                let _: () = objc2::msg_send![&*services_menu, update];
                header_item.setSubmenu(Some(&services_menu));
                menu.addItem(&header_item);
            }
        }
    }
    menu
}

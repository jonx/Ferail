//! Context menu (NSMenu) integration. Synchronously shows a menu at
//! the cursor and returns the user's selection by integer tag.
//!
//! The menu items themselves are owned by the host (Feraille's app
//! layer); this module just renders the picker. iter-4.5 may switch
//! the source to `IContextMenu`-style shell items so third-party
//! verbs (Finder extensions, "Open With…") show up — for now it's a
//! fixed app-defined list.

use std::cell::Cell;

use objc2::declare_class;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::{Allocated, Retained};
use objc2::sel;
use objc2::{ClassType, DeclaredClass};
use objc2_app_kit::{NSMenu, NSMenuItem, NSView};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

thread_local! {
    /// Tag of the most recently clicked menu item. `-1` = user dismissed
    /// the menu (Esc, click outside) without choosing an item. Read +
    /// reset on every `show_context_menu` call.
    static CLICKED_TAG: Cell<i32> = const { Cell::new(-1) };
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

/// Show a context menu at the mouse cursor with the given titles.
/// Blocks the calling thread while the menu is open. Returns the
/// 0-based index of the selected item, or `None` if the user dismissed
/// the menu without choosing one.
///
/// Separators in the source are signaled by an empty string title.
pub fn show_context_menu(
    window: &winit::window::Window,
    titles: &[&str],
    cursor_dips: (f32, f32),
) -> Option<usize> {
    let Some(mtm) = MainThreadMarker::new() else { return None };
    let Ok(handle) = window.window_handle() else { return None };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    let ns_view_ptr = h.ns_view.as_ptr();
    if ns_view_ptr.is_null() {
        return None;
    }

    CLICKED_TAG.with(|t| t.set(-1));

    let target = MenuTarget::new(mtm);

    unsafe {
        let ns_view: &NSView = &*(ns_view_ptr as *const NSView);
        let menu = NSMenu::new(mtm);
        let empty_key = NSString::from_str("");

        for (idx, title) in titles.iter().enumerate() {
            if title.is_empty() {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
                continue;
            }
            let ns_title = NSString::from_str(title);
            let item: Retained<NSMenuItem> = msg_send_id![
                mtm.alloc::<NSMenuItem>(),
                initWithTitle: &*ns_title,
                action: Some(sel!(itemClicked:)),
                keyEquivalent: &*empty_key,
            ];
            item.setTag(idx as isize);
            item.setTarget(Some(&*target));
            menu.addItem(&item);
        }

        // Convert cursor DIPs to view-local NSPoint. The view's coordinate
        // system has origin at the lower-left, but `popUpMenuPositioningItem`
        // accepts whatever the view considers "in its coordinate space",
        // which on a flipped (top-left origin) view is what we already
        // have. winit's NSView is flipped, so DIPs work directly.
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
        None
    } else {
        Some(tag as usize)
    }
}

//! Services / Quick Actions menu wiring.
//!
//! macOS auto-populates the Services submenu (which also surfaces
//! Automator-registered Quick Actions) by walking the responder
//! chain and asking each responder, "would you vend an `NSURL` for
//! the user-selected items?" via
//! `validRequestorForSendType:returnType:`. If the answer is yes,
//! AppKit lights up every service that consumes that type.
//!
//! winit owns the window's content view, so we can't subclass it.
//! Instead we declare a tiny `NSResponder` subclass and splice it
//! into the chain *between* winit's view and the `NSWindow`, by
//! mutating `setNextResponder:` at install time. The original
//! `NSWindow` next-link is preserved on our anchor so AppKit's
//! normal walk still terminates correctly.
//!
//! Selection state is kept in a thread-local so the anchor
//! doesn't need to know about `App`. The right-click handler
//! pushes the resolved selection just before showing the context
//! menu; the anchor reads it when AppKit asks.

use std::cell::RefCell;
use std::path::PathBuf;

use objc2::declare_class;
use objc2::msg_send;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSMenu, NSPasteboard, NSPasteboardWriting, NSResponder, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSObjectProtocol, NSString, NSURL,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

thread_local! {
    /// Paths the anchor will hand to AppKit when asked. Pushed by
    /// the right-click handler just before the context menu opens;
    /// stays sticky between menu sessions but only ever read when
    /// AppKit is actively populating Services for that menu, so
    /// the staleness window is harmless.
    static CURRENT_SELECTION: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };

    /// Owned reference to the installed anchor — `setNextResponder:`
    /// only takes a weak pointer, so we keep the strong ref here so
    /// the anchor outlives the window that points at it.
    static ANCHOR: RefCell<Option<Retained<ServicesAnchor>>> = const { RefCell::new(None) };

    /// The empty `NSMenu` we hand to `NSApp.servicesMenu` once at
    /// install time and reuse as the submenu of every right-click
    /// "Services" item. AppKit re-populates it on every show.
    static SERVICES_MENU: RefCell<Option<Retained<NSMenu>>> = const { RefCell::new(None) };
}

declare_class!(
    /// Responder splinter that lives between winit's content view
    /// and `NSWindow`. Exists only to advertise our app as a valid
    /// services requestor for file URLs.
    pub struct ServicesAnchor;

    unsafe impl ClassType for ServicesAnchor {
        type Super = NSResponder;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FeraServicesAnchor";
    }

    impl DeclaredClass for ServicesAnchor {
        type Ivars = ();
    }

    unsafe impl ServicesAnchor {
        /// Tell AppKit "yes, I can produce file URLs" when the
        /// pasteboard send type is `public.file-url` and a
        /// non-empty selection is available. Otherwise return nil
        /// so AppKit walks further up the responder chain.
        ///
        /// Returns a raw `*mut AnyObject` (Apple's `id`) — the
        /// autoreleased convention applies, so handing back `self`
        /// without retaining is correct.
        #[method(validRequestorForSendType:returnType:)]
        fn valid_requestor(
            &self,
            send_type: *const NSString,
            return_type: *const NSString,
        ) -> *mut AnyObject {
            let want_file_url = if send_type.is_null() {
                false
            } else {
                let s = unsafe { (&*send_type).to_string() };
                s == "public.file-url" || s == "NSFilenamesPboardType"
            };
            let has_selection = CURRENT_SELECTION
                .with(|cell| !cell.borrow().is_empty());
            if want_file_url && has_selection {
                let me: *const ServicesAnchor = self;
                return me as *mut AnyObject;
            }
            // Defer to the rest of the chain.
            unsafe {
                msg_send![
                    super(self),
                    validRequestorForSendType: send_type,
                    returnType: return_type,
                ]
            }
        }

        /// AppKit wants us to push the current selection onto
        /// `pasteboard` ahead of invoking the chosen service.
        /// Returns `YES` when at least one URL was written.
        #[method(writeSelectionToPasteboard:types:)]
        fn write_selection(
            &self,
            pasteboard: *mut NSPasteboard,
            _types: *mut AnyObject,
        ) -> objc2::runtime::Bool {
            let paths = CURRENT_SELECTION.with(|cell| cell.borrow().clone());
            if paths.is_empty() || pasteboard.is_null() {
                return objc2::runtime::Bool::NO;
            }
            unsafe {
                let pb: &NSPasteboard = &*pasteboard;
                pb.clearContents();
                let mut urls: Vec<Retained<NSURL>> = Vec::with_capacity(paths.len());
                for p in &paths {
                    let Some(s) = p.to_str() else { continue };
                    let ns_path = NSString::from_str(s);
                    let url = NSURL::fileURLWithPath_isDirectory(&ns_path, p.is_dir());
                    urls.push(url);
                }
                if urls.is_empty() {
                    return objc2::runtime::Bool::NO;
                }
                // NSPasteboard.writeObjects: takes
                // [id<NSPasteboardWriting>]. NSURL conforms;
                // `ProtocolObject::from_id` consumes the owned
                // Retained<NSURL> and re-types it.
                let writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = urls
                    .into_iter()
                    .map(ProtocolObject::from_id)
                    .collect();
                let array: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
                    NSArray::from_vec(writers);
                let ok: bool = msg_send![pb, writeObjects: &*array];
                objc2::runtime::Bool::new(ok)
            }
        }
    }
);

unsafe impl NSObjectProtocol for ServicesAnchor {}

impl ServicesAnchor {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let alloc: Allocated<Self> = mtm.alloc();
        unsafe { msg_send_id![alloc, init] }
    }
}

/// Install the Services anchor on `window`'s responder chain and
/// publish an empty `NSApp.servicesMenu` AppKit can populate on
/// demand. Idempotent — calling twice replaces the prior anchor.
/// No-op outside macOS.
pub fn install(window: &winit::window::Window) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let ns_view_ptr = h.ns_view.as_ptr();
    if ns_view_ptr.is_null() {
        return;
    }

    let anchor = ServicesAnchor::new(mtm);

    unsafe {
        let ns_view: &NSView = &*(ns_view_ptr as *const NSView);

        // Splice the anchor into the responder chain:
        //   winit_view -> anchor -> (winit_view's old next: usually NSWindow)
        let original_next: Option<Retained<NSResponder>> = ns_view.nextResponder();
        let anchor_responder: &NSResponder = anchor.as_ref();
        if let Some(orig) = original_next.as_ref() {
            anchor_responder.setNextResponder(Some(orig));
        }
        ns_view.setNextResponder(Some(anchor_responder));

        // Tell AppKit which pasteboard types we can produce so it
        // can filter the Services menu down to relevant entries.
        let app = NSApplication::sharedApplication(mtm);
        let send_types: Vec<Retained<NSString>> =
            vec![NSString::from_str("public.file-url")];
        let send_refs: Vec<&NSString> = send_types.iter().map(|s| s.as_ref()).collect();
        let send_array: Retained<NSArray<NSString>> =
            NSArray::from_vec(send_types.clone());
        let _ = send_refs;
        let return_array: Retained<NSArray<NSString>> = NSArray::new();
        let _: () = msg_send![
            &*app,
            registerServicesMenuSendTypes: &*send_array,
            returnTypes: &*return_array,
        ];

        // Build the empty menu AppKit will populate on demand.
        // We pre-warm by pushing a dummy non-empty selection just
        // long enough for `update` to walk the responder chain
        // and trigger AppKit's first-time async population — that
        // way the *user's* first right-click hits a warm cache
        // instead of a transient empty render. We restore an
        // empty selection right after.
        let menu = NSMenu::new(mtm);
        menu.setTitle(&NSString::from_str("Services"));
        app.setServicesMenu(Some(&menu));
        let dummy = std::env::temp_dir();
        CURRENT_SELECTION.with(|cell| *cell.borrow_mut() = vec![dummy]);
        let _: () = objc2::msg_send![&*menu, update];
        CURRENT_SELECTION.with(|cell| cell.borrow_mut().clear());
        SERVICES_MENU.with(|cell| *cell.borrow_mut() = Some(menu));
    }

    ANCHOR.with(|cell| *cell.borrow_mut() = Some(anchor));
}

/// Push the right-clicked selection so the anchor has something to
/// vend when AppKit asks. Call from the right-click handler just
/// before [`crate::show_context_menu`]. Stale entries are harmless
/// because AppKit only consults the anchor while a Services-aware
/// menu is open.
pub fn set_current_selection(paths: Vec<PathBuf>) {
    CURRENT_SELECTION.with(|cell| *cell.borrow_mut() = paths);
}

/// Build a fresh empty `NSMenu`, install it as `NSApp.servicesMenu`,
/// and stash the strong reference. The right-click builder calls
/// this each time it attaches the Services submenu so AppKit's
/// "was this menu populated already?" gate flips off and forces a
/// new responder-chain walk against the *current* selection.
///
/// Returns `None` until [`install`] has run.
pub fn refresh_services_menu() -> Option<Retained<NSMenu>> {
    let mtm = MainThreadMarker::new()?;
    if ANCHOR.with(|cell| cell.borrow().is_none()) {
        return None;
    }
    unsafe {
        let menu = NSMenu::new(mtm);
        menu.setTitle(&NSString::from_str("Services"));
        let app = NSApplication::sharedApplication(mtm);
        app.setServicesMenu(Some(&menu));
        SERVICES_MENU.with(|cell| *cell.borrow_mut() = Some(menu.clone()));
        Some(menu)
    }
}

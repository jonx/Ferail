//! Drag-out: kick off a Cocoa drag session for a list of file paths so
//! the user can drop them onto Finder, Mail, the Dock — anything that
//! accepts `kUTTypeFileURL`.
//!
//! The session is started by `view.beginDraggingSessionWithItems:event:source:`
//! so it composes correctly with modern macOS behavior (drag preview,
//! spring-load targets, etc.). The source object is a minimal class
//! declared via `declare_class!` that returns `Copy | Generic` for any
//! drag context — we don't yet support move/link semantics.

use std::path::Path;

use objc2::declare_class;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::ProtocolObject;
use objc2::{ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource,
    NSDragOperation, NSPasteboardWriting, NSView,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSString, NSURL};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

declare_class!(
    /// Minimal `NSDraggingSource` implementation. Reports a copy/generic
    /// operation mask for any drag context. Holds no state.
    pub struct DragSource;

    unsafe impl ClassType for DragSource {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FeraDragSource";
    }

    impl DeclaredClass for DragSource {
        type Ivars = ();
    }

    unsafe impl DragSource {
        #[method(draggingSession:sourceOperationMaskForDraggingContext:)]
        fn dragging_session_op_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            // Copy = 1, Generic = 4 — flags from <AppKit/NSDragging.h>
            NSDragOperation::Copy | NSDragOperation::Generic
        }
    }
);

unsafe impl NSObjectProtocol for DragSource {}
unsafe impl NSDraggingSource for DragSource {}

impl DragSource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let alloc: Allocated<Self> = mtm.alloc();
        unsafe { msg_send_id![alloc, init] }
    }
}

/// Begin a drag session for `paths`. Returns `true` if the drag was
/// kicked off; `false` if any prerequisite (window handle, NSEvent,
/// NSURL conversion) failed.
pub fn begin_drag(window: &winit::window::Window, paths: &[&Path]) -> bool {
    if paths.is_empty() {
        return false;
    }
    let Ok(handle) = window.window_handle() else { return false };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
    let ns_view_ptr = h.ns_view.as_ptr();
    if ns_view_ptr.is_null() {
        return false;
    }

    let Some(mtm) = MainThreadMarker::new() else { return false };

    unsafe {
        let ns_view: &NSView = &*(ns_view_ptr as *const NSView);
        let app = NSApplication::sharedApplication(mtm);
        let Some(event) = app.currentEvent() else { return false };

        // Build [NSDraggingItem] with NSURL pasteboard writers.
        let mut items: Vec<Retained<NSDraggingItem>> = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(s) = path.to_str() else { continue };
            let ns_path = NSString::from_str(s);
            let url = NSURL::fileURLWithPath(&ns_path);
            let writer: &ProtocolObject<dyn NSPasteboardWriting> =
                ProtocolObject::from_ref(&*url);
            let alloc: Allocated<NSDraggingItem> = NSDraggingItem::alloc();
            let item = NSDraggingItem::initWithPasteboardWriter(alloc, writer);
            items.push(item);
        }
        if items.is_empty() {
            return false;
        }
        let items_array = NSArray::from_vec(items);

        let source = DragSource::new(mtm);
        let source_proto: &ProtocolObject<dyn NSDraggingSource> =
            ProtocolObject::from_ref(&*source);

        let _session = ns_view.beginDraggingSessionWithItems_event_source(
            &items_array,
            &event,
            source_proto,
        );
        // The system retains both `items` and `source` for the duration
        // of the session; our local Retained handles drop here safely.
        true
    }
}

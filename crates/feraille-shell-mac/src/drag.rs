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
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEventType, NSPasteboardWriting, NSView,
};
use objc2_foundation::{
    CGPoint, CGRect, CGSize, MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSString, NSURL,
};
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
///
/// AppKit is strict about two things here:
/// - the event must be a mouse-down or mouse-dragged event
/// - every NSDraggingItem must have a non-empty dragging frame
///
/// We pre-validate the event and set a tiny frame around the current
/// cursor point before calling into AppKit. Do not wrap this in
/// `objc2::exception::catch`: on stable Rust, an Objective-C exception
/// thrown through that closure can become `panic in a function that
/// cannot unwind`, which hides the real AppKit complaint.
pub fn begin_drag(window: &winit::window::Window, paths: &[&Path]) -> bool {
    drag_log("begin drag request");
    drag_log(format!("    count : {}", paths.len()));
    for (idx, path) in paths.iter().enumerate() {
        drag_log(format!("    path[{idx}]: {}", path.display()));
    }
    if paths.is_empty() {
        return false;
    }
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let ns_view_ptr = h.ns_view.as_ptr();
    if ns_view_ptr.is_null() {
        drag_log("abort: null ns_view");
        return false;
    }

    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };

    unsafe {
        let ns_view: &NSView = &*(ns_view_ptr as *const NSView);
        let app = NSApplication::sharedApplication(mtm);
        let Some(event) = app.currentEvent() else {
            drag_log("abort: NSApp currentEvent = nil");
            return false;
        };

        // beginDraggingSession requires a mouse-down or mouse-dragged
        // event. `currentEvent` can return other types (key, mouse-up,
        // app-defined) depending on what NSApp last dispatched; passing
        // those raises NSInvalidArgumentException. Bail cleanly when
        // the type is wrong instead of relying on the exception
        // backstop.
        let ty = event.r#type();
        drag_log("current event");
        drag_log(format!("    type  : {} ({ty:?})", event_type_name(ty)));
        drag_log(format!("    nsView: {:p}", ns_view_ptr));
        if !matches!(
            ty,
            NSEventType::LeftMouseDown
                | NSEventType::LeftMouseDragged
                | NSEventType::RightMouseDown
                | NSEventType::RightMouseDragged
                | NSEventType::OtherMouseDown
                | NSEventType::OtherMouseDragged
        ) {
            drag_log(format!(
                "abort: unsupported event type {} ({ty:?})",
                event_type_name(ty),
            ));
            return false;
        }
        let view_point = ns_view.convertPoint_fromView(event.locationInWindow(), None);
        let drag_frame = CGRect::new(
            CGPoint::new(view_point.x - 16.0, view_point.y - 16.0),
            CGSize::new(32.0, 32.0),
        );
        drag_log(format!(
            "    frame : x={:.1} y={:.1} w={:.1} h={:.1}",
            drag_frame.origin.x, drag_frame.origin.y, drag_frame.size.width, drag_frame.size.height,
        ));

        // Build [NSDraggingItem] with NSURL pasteboard writers.
        let mut items: Vec<Retained<NSDraggingItem>> = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(s) = path.to_str() else { continue };
            let ns_path = NSString::from_str(s);
            let url = NSURL::fileURLWithPath(&ns_path);
            let writer: &ProtocolObject<dyn NSPasteboardWriting> = ProtocolObject::from_ref(&*url);
            let alloc: Allocated<NSDraggingItem> = NSDraggingItem::alloc();
            let item = NSDraggingItem::initWithPasteboardWriter(alloc, writer);
            item.setDraggingFrame(drag_frame);
            items.push(item);
        }
        if items.is_empty() {
            drag_log("abort: no valid NSURL items built");
            return false;
        }
        let items_array = NSArray::from_vec(items);

        let source = DragSource::new(mtm);
        let source_proto: &ProtocolObject<dyn NSDraggingSource> =
            ProtocolObject::from_ref(&*source);

        drag_log("beginDraggingSession");
        drag_log(format!("    items : {}", items_array.len()));
        drag_log(format!("    event : {} ({ty:?})", event_type_name(ty)));
        let _session =
            ns_view.beginDraggingSessionWithItems_event_source(&items_array, &event, source_proto);
        drag_log("beginDraggingSession returned successfully");
        // The system retains both `items` and `source` for the duration
        // of the session; our local Retained handles drop here safely.
        true
    }
}

fn drag_log(message: impl std::fmt::Display) {
    eprintln!("[shell-mac][drag] {message}");
}

fn event_type_name(ty: NSEventType) -> &'static str {
    match ty {
        NSEventType::LeftMouseDown => "LeftMouseDown",
        NSEventType::LeftMouseDragged => "LeftMouseDragged",
        NSEventType::RightMouseDown => "RightMouseDown",
        NSEventType::RightMouseDragged => "RightMouseDragged",
        NSEventType::OtherMouseDown => "OtherMouseDown",
        NSEventType::OtherMouseDragged => "OtherMouseDragged",
        _ => "Other",
    }
}

//! Share menu via `NSSharingServicePicker`. We don't enumerate
//! services ourselves — picking "Share" pops Apple's native picker
//! anchored to the window so the user gets the familiar Mail /
//! Messages / AirDrop / etc. list with whatever extensions they
//! have installed.
//!
//! Synchronous on the calling thread; the picker itself is
//! non-modal, so this returns once the picker is on screen.

use std::path::Path;

#[cfg(target_os = "macos")]
pub fn show_picker(window: &winit::window::Window, paths: &[&Path]) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSView;
    use objc2_foundation::{
        MainThreadMarker, NSArray, NSPoint, NSRect, NSSize, NSString, NSURL,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    if paths.is_empty() {
        return Err("share called with no paths".into());
    }
    let Some(_mtm) = MainThreadMarker::new() else {
        return Err("share must be called on the main thread".into());
    };
    let handle = window
        .window_handle()
        .map_err(|e| format!("no window handle: {e}"))?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return Err("share is macOS-only".into());
    };
    let ns_view_ptr = h.ns_view.as_ptr();
    if ns_view_ptr.is_null() {
        return Err("null NSView".into());
    }

    unsafe {
        let ns_view: &NSView = &*(ns_view_ptr as *const NSView);

        // Build an NSArray<NSURL>* from the path slice.
        let url_objs: Vec<Retained<NSURL>> = paths
            .iter()
            .map(|p| {
                let path_ns = NSString::from_str(&p.to_string_lossy());
                NSURL::fileURLWithPath_isDirectory(&path_ns, p.is_dir())
            })
            .collect();
        let array: Retained<NSArray<NSURL>> = NSArray::from_vec(url_objs);

        // NSSharingServicePicker isn't bound by objc2-app-kit's
        // feature set, so we drive alloc + init through raw
        // `msg_send` and balance the +1 init retain via
        // `Retained::from_raw`.
        let cls = objc2::runtime::AnyClass::get("NSSharingServicePicker")
            .ok_or("NSSharingServicePicker class missing")?;
        let alloc_ptr: *mut AnyObject = msg_send![cls, alloc];
        if alloc_ptr.is_null() {
            return Err("NSSharingServicePicker alloc returned nil".into());
        }
        let init_ptr: *mut AnyObject = msg_send![
            alloc_ptr,
            initWithItems: &*array,
        ];
        let picker = Retained::from_raw(init_ptr)
            .ok_or("NSSharingServicePicker init returned nil")?;

        // Anchor: a 1pt rect at the view's top-left. Apple's docs
        // say AppKit picks a sensible position when the rect is a
        // small square; the picker arrow lands near it.
        let bounds: NSRect = ns_view.bounds();
        let rect = NSRect {
            origin: NSPoint {
                x: bounds.origin.x,
                y: bounds.origin.y,
            },
            size: NSSize {
                width: 1.0,
                height: 1.0,
            },
        };

        // -[NSSharingServicePicker showRelativeToRect:ofView:preferredEdge:]
        // preferredEdge = NSRectEdgeMinY (1) shows below the rect.
        let _: () = msg_send![
            &*picker,
            showRelativeToRect: rect,
            ofView: ns_view,
            preferredEdge: 1u64,
        ];
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn show_picker(_window: &winit::window::Window, _paths: &[&Path]) -> Result<(), String> {
    Err("share is macOS-only".into())
}

//! Native outbound file promises for archive members.
//!
//! An archive member has no file URL until the user chooses a drop
//! destination. `NSFilePromiseProvider` is AppKit's exact abstraction for
//! that case: starting the drag is UI-only and immediate; AppKit invokes the
//! writer on the delegate's background operation queue after a real external
//! drop. No archive decoding or destination I/O ever runs on the GUI thread.
//!
//! The same gesture must also land in Ferail's own windows. AppKit only sends
//! drag-destination callbacks to a window registered (`registerForDraggedTypes:`)
//! for a type that is actually on the session pasteboard. gpui registers its
//! windows for `NSFilenamesPboardType` alone, and a promised file never carries
//! that type — no path exists yet — so without help AppKit would never call
//! `draggingEntered:` on a gpui window: the drop would silently do nothing
//! while Finder (which registers for promise types) keeps working. Two pieces
//! make Ferail windows valid destinations without materializing anything:
//!
//! * [`ArchivePromiseProvider`] subclasses `NSFilePromiseProvider` to also
//!   declare a private, data-free marker type on the session pasteboard —
//!   the documented way to add types to a promised item. Finder keeps
//!   reading the standard promise types and ignores the marker.
//! * [`register_archive_promise_destinations`] registers every gpui window
//!   for that marker (plus the legacy filename type gpui already uses, since
//!   `registerForDraggedTypes:` accumulates) before the session starts, so
//!   AppKit routes the gesture to the `draggingEntered:` shim in `lib.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use objc2::declare_class;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2::{msg_send, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSDraggingItem, NSDraggingSession, NSFilePromiseProvider,
    NSFilePromiseProviderDelegate, NSFilenamesPboardType, NSPasteboard, NSPasteboardType,
    NSPasteboardWriting, NSPasteboardWritingOptions, NSView, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSError, NSObject, NSObjectProtocol, NSOperationQueue, NSPoint,
    NSRect, NSSize, NSString, NSURL,
};

type PromiseWriter = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync + 'static>;

/// Private marker understood by the GPUI-window drag-enter shim in `lib.rs`.
/// It carries no path or archive data; the real payload stays in-process.
/// Declared by every [`ArchivePromiseProvider`], and the type Ferail windows
/// register for so AppKit delivers the promised-file session to them at all.
pub(super) const ARCHIVE_PROMISE_PASTEBOARD_TYPE: &str = "com.ferail.archive-entry-file-promise";

fn is_marker_type(pasteboard_type: &NSPasteboardType) -> bool {
    pasteboard_type.to_string() == ARCHIVE_PROMISE_PASTEBOARD_TYPE
}

/// One item offered to Finder by an outbound promised-file drag.
pub struct FilePromise {
    name: String,
    is_directory: bool,
    writer: PromiseWriter,
}

impl FilePromise {
    pub fn new(
        name: impl Into<String>,
        is_directory: bool,
        writer: impl Fn(&Path) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            is_directory,
            writer: Arc::new(writer),
        }
    }
}

struct PromiseIvars {
    name: String,
    writer: PromiseWriter,
    queue: Retained<NSOperationQueue>,
}

declare_class!(
    struct PromiseDelegate;

    unsafe impl ClassType for PromiseDelegate {
        type Super = NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "FeraArchiveFilePromiseDelegate";
    }

    impl DeclaredClass for PromiseDelegate {
        type Ivars = PromiseIvars;
    }

    unsafe impl NSObjectProtocol for PromiseDelegate {}

    unsafe impl NSFilePromiseProviderDelegate for PromiseDelegate {
        #[method_id(filePromiseProvider:fileNameForType:)]
        fn promised_name(
            &self,
            _provider: &NSFilePromiseProvider,
            _file_type: &NSString,
        ) -> Retained<NSString> {
            NSString::from_str(&self.ivars().name)
        }

        #[method(filePromiseProvider:writePromiseToURL:completionHandler:)]
        fn write_promise(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion: &block2::Block<dyn Fn(*mut NSError)>,
        ) {
            let target = unsafe { url.path() }
                .map(|path| PathBuf::from(path.to_string()));
            let result = target
                .ok_or_else(|| "Finder supplied an invalid destination URL".to_string())
                .and_then(|target| (self.ivars().writer)(&target));
            match result {
                Ok(()) => completion.call((std::ptr::null_mut(),)),
                Err(message) => {
                    log::warn!("archive file promise failed: {message}");
                    let domain = NSString::from_str("com.ferail.archive-file-promise");
                    let error = unsafe {
                        NSError::errorWithDomain_code_userInfo(&domain, 1, None)
                    };
                    completion.call((Retained::as_ptr(&error).cast_mut(),));
                }
            }
        }

        #[method_id(operationQueueForFilePromiseProvider:)]
        fn operation_queue(
            &self,
            _provider: &NSFilePromiseProvider,
        ) -> Retained<NSOperationQueue> {
            self.ivars().queue.clone()
        }
    }
);

impl PromiseDelegate {
    fn new(mtm: MainThreadMarker, name: String, writer: PromiseWriter) -> Retained<Self> {
        let queue = unsafe { NSOperationQueue::new() };
        unsafe {
            queue.setMaxConcurrentOperationCount(1);
            queue.setName(Some(&NSString::from_str("Ferail archive extraction")));
        }
        let alloc = mtm.alloc::<Self>().set_ivars(PromiseIvars {
            name,
            writer,
            queue,
        });
        unsafe { msg_send_id![super(alloc), init] }
    }
}

declare_class!(
    /// `NSFilePromiseProvider` that additionally declares
    /// [`ARCHIVE_PROMISE_PASTEBOARD_TYPE`] on the session pasteboard. Apple
    /// documents subclassing the provider to add pasteboard types; the
    /// standard promise types and their lazy writer are inherited untouched.
    struct ArchivePromiseProvider;

    unsafe impl ClassType for ArchivePromiseProvider {
        type Super = NSFilePromiseProvider;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "FeraArchiveFilePromiseProvider";
    }

    impl DeclaredClass for ArchivePromiseProvider {}

    unsafe impl NSObjectProtocol for ArchivePromiseProvider {}

    unsafe impl ArchivePromiseProvider {
        #[method_id(writableTypesForPasteboard:)]
        fn writable_types(
            &self,
            pasteboard: &NSPasteboard,
        ) -> Retained<NSArray<NSPasteboardType>> {
            let inherited: Retained<NSArray<NSPasteboardType>> =
                unsafe { msg_send_id![super(self), writableTypesForPasteboard: pasteboard] };
            let mut types = inherited.to_vec_retained();
            types.push(NSString::from_str(ARCHIVE_PROMISE_PASTEBOARD_TYPE));
            NSArray::from_vec(types)
        }

        #[method(writingOptionsForType:pasteboard:)]
        fn writing_options(
            &self,
            pasteboard_type: &NSPasteboardType,
            pasteboard: &NSPasteboard,
        ) -> NSPasteboardWritingOptions {
            if is_marker_type(pasteboard_type) {
                // Written eagerly, not promised: a destination's
                // `draggingEntered:` must be able to read it immediately.
                NSPasteboardWritingOptions::empty()
            } else {
                unsafe {
                    msg_send![
                        super(self),
                        writingOptionsForType: pasteboard_type,
                        pasteboard: pasteboard
                    ]
                }
            }
        }

        #[method_id(pasteboardPropertyListForType:)]
        fn property_list(
            &self,
            pasteboard_type: &NSPasteboardType,
        ) -> Option<Retained<AnyObject>> {
            if is_marker_type(pasteboard_type) {
                let marker: Retained<NSObject> = Retained::into_super(NSString::from_str("1"));
                Some(Retained::into_super(marker))
            } else {
                unsafe { msg_send_id![super(self), pasteboardPropertyListForType: pasteboard_type] }
            }
        }
    }
);

impl ArchivePromiseProvider {
    fn new(
        file_type: &NSString,
        delegate: &ProtocolObject<dyn NSFilePromiseProviderDelegate>,
    ) -> Retained<Self> {
        unsafe { msg_send_id![Self::alloc(), initWithFileType: file_type, delegate: delegate] }
    }
}

/// Register every gpui window as a drag destination for archive promises.
///
/// gpui's own registration covers `NSFilenamesPboardType` only, which a
/// promised file never carries; without the marker type AppKit skips every
/// Ferail window and the drop lands nowhere. `registerForDraggedTypes:`
/// accumulates, but the legacy filename type is passed again regardless so
/// Finder→Ferail file drops can never lose their registration. Only gpui's
/// window classes are touched — other AppKit windows keep their own types.
/// Called on the main thread at drag start, so windows opened later are
/// covered by the next drag. Returns how many windows were registered.
fn register_archive_promise_destinations(app: &NSApplication) -> usize {
    let gpui_classes: Vec<&'static AnyClass> = ["GPUIWindow", "GPUIPanel"]
        .iter()
        .filter_map(|name| AnyClass::get(name))
        .collect();
    if gpui_classes.is_empty() {
        return 0;
    }
    // Re-stringified: `NSArray::from_vec` wants owned `NSString`s and the
    // AppKit constant is a borrowed static.
    let legacy_filenames = NSString::from_str(&unsafe { NSFilenamesPboardType }.to_string());
    let marker = NSString::from_str(ARCHIVE_PROMISE_PASTEBOARD_TYPE);
    let types = NSArray::from_vec(vec![legacy_filenames, marker]);
    let mut registered = 0;
    for window in app.windows().iter() {
        if gpui_classes.iter().any(|class| window.isKindOfClass(class)) {
            window.registerForDraggedTypes(&types);
            registered += 1;
        }
    }
    registered
}

/// Start a native promised-file drag from the current AppKit mouse gesture.
///
/// This function itself is main-thread-only and performs no filesystem work.
/// Each promise's writer is dispatched by AppKit on the delegate's private
/// operation queue only after an external destination accepts the drop.
pub fn start(ns_view: *mut std::ffi::c_void, promises: Vec<FilePromise>) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    if ns_view.is_null() || promises.is_empty() {
        return false;
    }

    let view: &NSView = unsafe { &*(ns_view as *const NSView) };
    let app = NSApplication::sharedApplication(mtm);
    let Some(event) = app.currentEvent() else {
        return false;
    };
    let window: Option<Retained<AnyObject>> = unsafe { msg_send_id![view, window] };
    let Some(window) = window else { return false };
    let location = unsafe { event.locationInWindow() };
    let frame = NSRect::new(
        NSPoint::new(location.x - 16.0, location.y - 16.0),
        NSSize::new(32.0, 32.0),
    );
    let workspace = unsafe { NSWorkspace::sharedWorkspace() };
    let mut items = Vec::with_capacity(promises.len());

    for promise in promises {
        let file_type = if promise.is_directory {
            "public.folder"
        } else {
            "public.data"
        };
        let delegate = PromiseDelegate::new(mtm, promise.name, promise.writer);
        let delegate_protocol = ProtocolObject::from_ref(&*delegate);
        let provider: Retained<NSFilePromiseProvider> = Retained::into_super(
            ArchivePromiseProvider::new(&NSString::from_str(file_type), delegate_protocol),
        );
        // NSFilePromiseProvider's delegate is non-owning. Keeping the delegate
        // in retained `userInfo` ties its lifetime to the provider/session.
        let delegate_object: &AnyObject = &delegate;
        unsafe { provider.setUserInfo(Some(delegate_object)) };
        let writer = ProtocolObject::<dyn NSPasteboardWriting>::from_ref(&*provider);
        let item =
            unsafe { NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer) };
        #[allow(deprecated)]
        let icon = unsafe { workspace.iconForFileType(&NSString::from_str(file_type)) };
        unsafe { item.setDraggingFrame_contents(frame, Some(&icon)) };
        items.push(item);
    }

    // Ferail windows must be registered for the marker type before AppKit
    // starts destination tracking — when the popped-out workbench overlaps
    // its parent, the pointer may already be over the underlying window as
    // the session begins.
    let registered = register_archive_promise_destinations(&app);
    if registered == 0 {
        log::warn!(
            "no gpui window registered for archive promise drags; \
             dropping into Ferail windows will not work this session"
        );
    } else {
        log::info!("archive promise drag: {registered} gpui window(s) registered as destinations");
    }

    let items = NSArray::from_vec(items);
    // Raised before the session starts so a destination's `draggingEntered:`
    // — possibly delivered before `beginDraggingSession` returns — already
    // sees the in-process flag; the pasteboard marker declared by every
    // provider is the fallback. `session_ended` in `lib.rs` lowers it.
    super::native_drag::ARCHIVE_PROMISE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    let session: Option<Retained<NSDraggingSession>> = unsafe {
        msg_send_id![
            view,
            beginDraggingSessionWithItems: &*items,
            event: &*event,
            source: &*window
        ]
    };
    if session.is_none() {
        super::native_drag::ARCHIVE_PROMISE_ACTIVE
            .store(false, std::sync::atomic::Ordering::SeqCst);
        log::warn!("archive promise drag: beginDraggingSession returned nil");
        return false;
    }
    log::info!("archive promise drag: native session started");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> Retained<NSFilePromiseProvider> {
        // The provider, its delegate, and a private pasteboard are plain
        // model objects; AppKit does not require the main thread to build
        // them, which is what lets this run under the test harness.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let writer: PromiseWriter = Arc::new(|_: &Path| Ok(()));
        let delegate = PromiseDelegate::new(mtm, "member.txt".into(), writer);
        let provider = ArchivePromiseProvider::new(
            &NSString::from_str("public.data"),
            ProtocolObject::from_ref(&*delegate),
        );
        Retained::into_super(provider)
    }

    #[test]
    fn archive_promise_declares_marker_but_never_legacy_filenames() {
        let provider = test_provider();
        let pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
        unsafe { pasteboard.clearContents() };
        let object: Retained<AnyObject> = Retained::into_super(Retained::into_super(provider));
        let objects = NSArray::from_vec(vec![object]);
        let written: bool = unsafe { msg_send![&*pasteboard, writeObjects: &*objects] };
        assert!(written, "promise provider must be writable to a pasteboard");

        // The marker Ferail windows register for is present and readable
        // immediately (eager, not promised)...
        let marker = NSString::from_str(ARCHIVE_PROMISE_PASTEBOARD_TYPE);
        let value = unsafe { pasteboard.stringForType(&marker) }.map(|s| s.to_string());
        assert_eq!(value.as_deref(), Some("1"));

        // ...the legacy filename list gpui parses is never advertised, so
        // its own `draggingEntered:` path cannot see a bogus path...
        assert!(unsafe { pasteboard.propertyListForType(NSFilenamesPboardType) }.is_none());

        // ...and Finder's promise receiver still sees the standard types.
        let types = unsafe { pasteboard.types() }.expect("pasteboard types");
        let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
        assert!(
            types
                .iter()
                .any(|t| t == "com.apple.pasteboard.promised-file-content-type"),
            "standard promise types missing: {types:?}"
        );
    }

    #[test]
    fn destination_registration_is_harmless_without_gpui_windows() {
        // The test binary has no gpui window classes; registration must
        // report zero rather than touch arbitrary AppKit windows.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let app = NSApplication::sharedApplication(mtm);
        assert_eq!(register_archive_promise_destinations(&app), 0);
    }
}

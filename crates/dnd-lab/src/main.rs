//! dnd-lab — a two-window gpui harness for native promised-file drags.
//!
//! Ferail's archive workbench drags entries out as `NSFilePromiseProvider`
//! items (no file exists until a destination accepts the drop). The same
//! gesture must also land in Ferail's *own* gpui windows, which means AppKit
//! has to route a pathless promise drag to a gpui window and gpui's
//! `draggingUpdated:` / `performDragOperation:` have to turn it into
//! MouseMove/MouseUp for an in-process payload. Getting that right blind,
//! inside the full app, proved slow — so this lab reproduces the exact
//! mechanism with the exact `ferail-shell-mac` code (path dependency, not a
//! copy) and logs every callback AppKit and gpui make.
//!
//! Run: `cargo run -p dnd-lab` (macOS only; other hosts print a note).
//!
//! Windows:
//! - **Source** (left): rows that start a promise drag when dragged out of
//!   the window, plus its own drop zone (the "docked archive" case).
//! - **Target** (right): one big drop zone (the "other Ferail window" case).
//!
//! Scenarios to exercise: Source → Finder (a real file must appear),
//! Source → Target, Source → Source (leave and re-enter), overlapping
//! windows at promotion, Esc during the drag.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("dnd-lab only runs on macOS");
}

#[cfg(target_os = "macos")]
fn main() {
    lab::main();
}

#[cfg(target_os = "macos")]
mod lab {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicPtr, Ordering::SeqCst};
    use std::time::Instant;

    use gpui::prelude::*;
    use gpui::{
        div, px, rgb, size, App, Bounds, Context, DragMoveEvent, ExternalPaths, KeyDownEvent,
        MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Point, Render, SharedString,
        TitlebarOptions, Window, WindowBounds, WindowOptions,
    };
    use objc2::runtime::{AnyClass, AnyObject, Sel};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // ------------------------------------------------------------------
    // Logging: everything goes to stderr with elapsed seconds.
    // ------------------------------------------------------------------

    struct StderrLogger {
        start: Instant,
    }

    impl log::Log for StderrLogger {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            eprintln!(
                "[+{:7.3}s][{}] {}",
                self.start.elapsed().as_secs_f64(),
                record.level().as_str().to_lowercase(),
                record.args()
            );
        }
        fn flush(&self) {}
    }

    fn install_logger() {
        let logger: &'static StderrLogger = Box::leak(Box::new(StderrLogger {
            start: Instant::now(),
        }));
        let _ = log::set_logger(logger);
        log::set_max_level(log::LevelFilter::Trace);
    }

    // ------------------------------------------------------------------
    // In-process payload: mirrors Ferail's ArchiveEntryDrag + NATIVE_ARCHIVE_DRAG.
    // ------------------------------------------------------------------

    #[derive(Clone, Debug)]
    struct LabDrag {
        entries: Vec<String>,
        directories: Vec<bool>,
    }

    thread_local! {
        static NATIVE_DRAG: RefCell<Option<LabDrag>> = const { RefCell::new(None) };
    }

    fn native_drag_active() -> bool {
        NATIVE_DRAG.with(|d| d.borrow().is_some())
    }
    fn take_native_drag() -> Option<LabDrag> {
        NATIVE_DRAG.with(|d| d.borrow_mut().take())
    }
    fn set_native_drag(drag: Option<LabDrag>) {
        NATIVE_DRAG.with(|d| *d.borrow_mut() = drag);
    }

    // ------------------------------------------------------------------
    // Drag ghost
    // ------------------------------------------------------------------

    struct Ghost {
        label: SharedString,
    }

    impl Render for Ghost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .px_2()
                .py_1()
                .bg(rgb(0x3b82f6))
                .text_color(rgb(0xffffff))
                .rounded_md()
                .child(self.label.clone())
        }
    }

    // ------------------------------------------------------------------
    // Shared drop-zone element builder (used by both windows)
    // ------------------------------------------------------------------

    fn drop_zone<V: 'static>(
        name: &'static str,
        cx: &mut Context<V>,
        on_native_drop: impl Fn(&mut V, LabDrag, &mut Window, &mut Context<V>) + 'static + Clone,
    ) -> gpui::Stateful<gpui::Div> {
        let native = native_drag_active();
        let on_native_drop_up = on_native_drop.clone();
        div()
            .id(name)
            .flex_1()
            .w_full()
            .m_2()
            .p_2()
            .border_2()
            .border_color(rgb(0x9ca3af))
            .rounded_md()
            .bg(rgb(0xf3f4f6))
            .text_color(rgb(0x111827))
            .child(format!("{name} drop zone (native drag active: {native})"))
            // In-app typed drag (never crosses windows in gpui today).
            .drag_over::<LabDrag>(|style, _, _, _| {
                style.border_color(rgb(0x16a34a)).bg(rgb(0xdcfce7))
            })
            .on_drag_move(cx.listener(move |_, e: &DragMoveEvent<LabDrag>, _, _| {
                if e.bounds.contains(&e.event.position) {
                    log::info!("{name}: gpui DragMoveEvent<LabDrag> at {:?}", e.event.position);
                }
            }))
            .on_drop(cx.listener(move |this, drag: &LabDrag, window, cx| {
                log::info!("{name}: gpui on_drop::<LabDrag> {:?}", drag.entries);
                on_native_drop(this, drag.clone(), window, cx);
            }))
            // Real files from Finder / other apps.
            .drag_over::<ExternalPaths>(|style, _, _, _| {
                style.border_color(rgb(0x2563eb)).bg(rgb(0xdbeafe))
            })
            .on_drop(cx.listener(move |_, paths: &ExternalPaths, _, _| {
                log::info!("{name}: gpui on_drop::<ExternalPaths> {:?}", paths.paths());
            }))
            // Native promise session fallback: gpui has no typed drag, AppKit's
            // draggingUpdated/performDragOperation arrive as MouseMove/MouseUp.
            .on_mouse_move(cx.listener(move |_, e: &MouseMoveEvent, _, cx| {
                if native_drag_active() {
                    log::debug!(
                        "{name}: MouseMove during native drag at {:?} pressed={:?}",
                        e.position,
                        e.pressed_button
                    );
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(move |_, e: &MouseDownEvent, _, _| {
                log::debug!("{name}: MouseDown at {:?}", e.position);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseUpEvent, window, cx| {
                    log::info!(
                        "{name}: MouseUp at {:?} has_active_drag={} native={}",
                        e.position,
                        cx.has_active_drag(),
                        native_drag_active()
                    );
                    if cx.has_active_drag() {
                        return;
                    }
                    let Some(drag) = take_native_drag() else {
                        return;
                    };
                    cx.stop_propagation();
                    log::info!("{name}: >>> NATIVE DROP ACCEPTED {:?}", drag.entries);
                    on_native_drop_up(this, drag, window, cx);
                }),
            )
            .when(native, |d| {
                d.hover(|style| style.border_color(rgb(0xf59e0b)).bg(rgb(0xfef3c7)))
            })
    }

    // ------------------------------------------------------------------
    // Source window
    // ------------------------------------------------------------------

    struct SourceView {
        focus: gpui::FocusHandle,
        rows: Vec<(String, bool)>,
        status: SharedString,
    }

    impl Render for SourceView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let rows = self.rows.clone();
            if !self.focus.is_focused(window) {
                window.focus(&self.focus, cx);
            }
            div()
                .id("source-root")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, _| {
                    if e.keystroke.key == "escape" {
                        log::info!("Esc → cancel_native_drag()");
                        ferail_shell_mac::cancel_native_drag();
                    }
                }))
                .size_full()
                .flex()
                .flex_col()
                .bg(rgb(0xffffff))
                .text_color(rgb(0x111827))
                .on_mouse_down(MouseButton::Left, cx.listener(|_, e: &MouseDownEvent, _, _| {
                    log::debug!("SOURCE root: MouseDown at {:?}", e.position);
                }))
                // Fixed-height layout so synthetic-event drivers can compute
                // row centres: header 40, status 24, rows 40 each, zone rest.
                .child(
                    div()
                        .h(px(40.))
                        .px_2()
                        .flex()
                        .items_center()
                        .child("SOURCE — drag a row out of this window (to Target, Finder, or back here)"),
                )
                .child(div().h(px(24.)).px_2().text_sm().child(self.status.clone()))
                .children(rows.into_iter().enumerate().map(|(ix, (name, is_dir))| {
                    let drag = LabDrag {
                        entries: vec![name.clone()],
                        directories: vec![is_dir],
                    };
                    let label: SharedString = name.clone().into();
                    let ghost_label = label.clone();
                    div()
                        .id(("row", ix))
                        .h(px(40.))
                        .px_2()
                        .flex()
                        .items_center()
                        .border_1()
                        .border_color(rgb(0xd1d5db))
                        .hover(|s| s.bg(rgb(0xe5e7eb)))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |_, e: &MouseDownEvent, _, _| {
                            log::info!("SOURCE row {ix}: MouseDown at {:?}", e.position);
                        }))
                        .child(format!("{}{}", if is_dir { "📁 " } else { "📄 " }, label))
                        .on_drag(drag, move |_d, _offset, _window, cx| {
                            cx.new(|_| Ghost {
                                label: ghost_label.clone(),
                            })
                        })
                        .external_drag_payload::<LabDrag>(|drag, window, cx| {
                            promote_to_native(drag, window, cx);
                            None
                        })
                }))
                .child(drop_zone("Source", cx, |this, drag, _, cx| {
                    this.status = format!("Source zone received native drop: {:?}", drag.entries).into();
                    cx.notify();
                }))
                .child(
                    div()
                        .p_2()
                        .text_xs()
                        .child("Esc cancels a native drag. Window ids are logged at startup."),
                )
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                }))
        }
    }

    /// Exactly Ferail's promotion recipe (file_list.rs), minus archive I/O:
    /// build promises, park the in-process payload, start the native session,
    /// retire gpui's typed drag, and clear the payload when AppKit ends.
    fn promote_to_native(drag: &LabDrag, window: &mut Window, cx: &mut App) {
        let source_window = gpui::Window::window_handle(window);
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            log::error!("promote: no raw window handle");
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            log::error!("promote: not an AppKit handle");
            return;
        };
        let promises: Vec<ferail_shell_mac::FilePromise> = drag
            .entries
            .iter()
            .zip(drag.directories.iter().copied())
            .map(|(entry, is_dir)| {
                let entry = entry.clone();
                ferail_shell_mac::FilePromise::new(
                    entry.clone(),
                    is_dir,
                    move |target: &std::path::Path| {
                        log::info!("promise writer: materialize {entry} → {}", target.display());
                        if is_dir {
                            std::fs::create_dir_all(target).map_err(|e| e.to_string())
                        } else {
                            std::fs::write(target, format!("lab entry {entry}\n"))
                                .map_err(|e| e.to_string())
                        }
                    },
                )
            })
            .collect();
        set_native_drag(Some(drag.clone()));
        log::info!(
            "promote: starting native promise session for {:?} from ns_view={:p}",
            drag.entries,
            handle.ns_view.as_ptr()
        );
        if ferail_shell_mac::start_file_promise_drag(handle.ns_view.as_ptr(), promises) {
            log::info!("promote: native session started; retiring gpui typed drag");
            cx.stop_active_drag(window);
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                while ferail_shell_mac::native_drag_session_active() {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(16))
                        .await;
                }
                log::info!("watcher: native session ended; clearing in-process payload (was_present={})", native_drag_active());
                let _ = cx.update_window(source_window, |_, window, cx| {
                    cx.stop_active_drag(window);
                    set_native_drag(None);
                });
                cx.refresh();
            })
            .detach();
        } else {
            log::error!("promote: native session FAILED to start");
            set_native_drag(None);
        }
    }

    // ------------------------------------------------------------------
    // Target window
    // ------------------------------------------------------------------

    struct TargetView {
        status: SharedString,
    }

    impl Render for TargetView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(rgb(0xffffff))
                .text_color(rgb(0x111827))
                .on_mouse_down(MouseButton::Left, cx.listener(|_, e: &MouseDownEvent, _, _| {
                    log::debug!("TARGET root: MouseDown at {:?}", e.position);
                }))
                .child(div().h(px(40.)).px_2().flex().items_center().child("TARGET — drop here"))
                .child(div().h(px(24.)).px_2().text_sm().child(self.status.clone()))
                .child(drop_zone("Target", cx, |this, drag, _, cx| {
                    this.status = format!("Target received native drop: {:?}", drag.entries).into();
                    cx.notify();
                }))
        }
    }

    // ------------------------------------------------------------------
    // Destination-callback probe: logs what AppKit sends gpui's window
    // classes, chaining to whatever is installed (shell-mac shim → gpui).
    // Installed AFTER ferail_shell_mac::install_native_drag_operations().
    // ------------------------------------------------------------------

    static ORIG_ENTERED: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    static ORIG_UPDATED: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    static ORIG_EXITED: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    static ORIG_PERFORM: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    static ORIG_CONCLUDE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    static ORIG_ENDED: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

    fn describe(obj: *mut AnyObject) -> String {
        if obj.is_null() {
            return "nil".into();
        }
        unsafe {
            let desc: *mut AnyObject = objc2::msg_send![obj, description];
            if desc.is_null() {
                return "?".into();
            }
            let s: *const std::ffi::c_char = objc2::msg_send![desc, UTF8String];
            if s.is_null() {
                return "?".into();
            }
            std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned()
        }
    }

    fn pasteboard_types(info: *mut AnyObject) -> String {
        if info.is_null() {
            return "nil info".into();
        }
        unsafe {
            let pb: *mut AnyObject = objc2::msg_send![info, draggingPasteboard];
            if pb.is_null() {
                return "nil pasteboard".into();
            }
            let types: *mut AnyObject = objc2::msg_send![pb, types];
            describe(types).replace('\n', " ")
        }
    }

    fn source_of(info: *mut AnyObject) -> *mut AnyObject {
        if info.is_null() {
            return std::ptr::null_mut();
        }
        unsafe { objc2::msg_send![info, draggingSource] }
    }

    type EnteredFn = extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize;
    type VoidInfoFn = extern "C" fn(*mut AnyObject, Sel, *mut AnyObject);
    /// Objective-C `BOOL`. On ARM64 (and modern arm64e) it is C `bool`, but on
    /// Intel macOS it is a signed `char` — so the probe uses `i8` and compares
    /// against 0 rather than Rust's `bool`, which would misread the return on
    /// x86_64. (Production never replaces `performDragOperation:`; this is the
    /// lab's diagnostic chain only.)
    type ObjcBool = i8;
    type PerformFn = extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> ObjcBool;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPointRaw {
        x: f64,
        y: f64,
    }
    type EndedFn = extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, CGPointRaw, usize);

    extern "C" fn probe_entered(this: *mut AnyObject, sel: Sel, info: *mut AnyObject) -> usize {
        log::info!(
            "AppKit draggingEntered: window={this:p} source={:p} types=[{}]",
            source_of(info),
            pasteboard_types(info)
        );
        let orig = ORIG_ENTERED.load(SeqCst);
        let r = if orig.is_null() {
            0
        } else {
            let f: EnteredFn = unsafe { std::mem::transmute(orig) };
            f(this, sel, info)
        };
        log::info!("AppKit draggingEntered → operation mask {r}");
        r
    }

    extern "C" fn probe_updated(this: *mut AnyObject, sel: Sel, info: *mut AnyObject) -> usize {
        let orig = ORIG_UPDATED.load(SeqCst);
        let r = if orig.is_null() {
            0
        } else {
            let f: EnteredFn = unsafe { std::mem::transmute(orig) };
            f(this, sel, info)
        };
        log::debug!("AppKit draggingUpdated: window={this:p} → {r}");
        r
    }

    extern "C" fn probe_exited(this: *mut AnyObject, sel: Sel, info: *mut AnyObject) {
        log::info!("AppKit draggingExited: window={this:p}");
        let orig = ORIG_EXITED.load(SeqCst);
        if !orig.is_null() {
            let f: VoidInfoFn = unsafe { std::mem::transmute(orig) };
            f(this, sel, info);
        }
    }

    extern "C" fn probe_perform(this: *mut AnyObject, sel: Sel, info: *mut AnyObject) -> ObjcBool {
        log::info!("AppKit performDragOperation: window={this:p}");
        let orig = ORIG_PERFORM.load(SeqCst);
        let r = if orig.is_null() {
            0
        } else {
            let f: PerformFn = unsafe { std::mem::transmute(orig) };
            f(this, sel, info)
        };
        log::info!("AppKit performDragOperation → {}", r != 0);
        r
    }

    extern "C" fn probe_conclude(this: *mut AnyObject, sel: Sel, info: *mut AnyObject) {
        log::info!("AppKit concludeDragOperation: window={this:p}");
        let orig = ORIG_CONCLUDE.load(SeqCst);
        if !orig.is_null() {
            let f: VoidInfoFn = unsafe { std::mem::transmute(orig) };
            f(this, sel, info);
        }
    }

    extern "C" fn probe_ended(
        this: *mut AnyObject,
        sel: Sel,
        session: *mut AnyObject,
        point: CGPointRaw,
        operation: usize,
    ) {
        log::info!(
            "AppKit draggingSession:endedAtPoint:operation: window={this:p} op={operation} at ({}, {})",
            point.x,
            point.y
        );
        let orig = ORIG_ENDED.load(SeqCst);
        if !orig.is_null() {
            let f: EndedFn = unsafe { std::mem::transmute(orig) };
            f(this, sel, session, point, operation);
        }
    }

    fn install_probe() {
        unsafe fn replace(
            class: *mut objc2::ffi::objc_class,
            sel: Sel,
            imp: *mut std::ffi::c_void,
            types: &std::ffi::CStr,
            slot: &AtomicPtr<std::ffi::c_void>,
        ) {
            let prev = unsafe {
                objc2::ffi::class_replaceMethod(
                    class,
                    sel.as_ptr(),
                    std::mem::transmute::<*mut std::ffi::c_void, objc2::ffi::IMP>(imp),
                    types.as_ptr(),
                )
            };
            if let Some(prev) = prev {
                let prev = prev as *mut std::ffi::c_void;
                if prev != imp {
                    slot.store(prev, SeqCst);
                }
            }
        }
        for name in ["GPUIWindow", "GPUIPanel"] {
            let Some(class) = AnyClass::get(name) else {
                log::warn!("probe: class {name} not found");
                continue;
            };
            let class = class as *const AnyClass as *mut objc2::ffi::objc_class;
            unsafe {
                replace(class, objc2::sel!(draggingEntered:), probe_entered as EnteredFn as *mut _, c"Q@:@", &ORIG_ENTERED);
                replace(class, objc2::sel!(draggingUpdated:), probe_updated as EnteredFn as *mut _, c"Q@:@", &ORIG_UPDATED);
                replace(class, objc2::sel!(draggingExited:), probe_exited as VoidInfoFn as *mut _, c"v@:@", &ORIG_EXITED);
                // Type encoding: "B" (C99 _Bool) on arm64, "c" (signed char)
                // on x86_64 — the same split as `ObjcBool` above.
                #[cfg(target_arch = "aarch64")]
                let bool_encoding = c"B@:@";
                #[cfg(not(target_arch = "aarch64"))]
                let bool_encoding = c"c@:@";
                replace(class, objc2::sel!(performDragOperation:), probe_perform as PerformFn as *mut _, bool_encoding, &ORIG_PERFORM);
                replace(class, objc2::sel!(concludeDragOperation:), probe_conclude as VoidInfoFn as *mut _, c"v@:@", &ORIG_CONCLUDE);
                replace(class, objc2::sel!(draggingSession:endedAtPoint:operation:), probe_ended as EndedFn as *mut _, c"v@:@{CGPoint=dd}Q", &ORIG_ENDED);
            }
            log::info!("probe: installed destination/session logging on {name}");
        }
    }

    fn log_window_identity(label: &str, window: &mut Window) {
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                let view = h.ns_view.as_ptr() as *mut AnyObject;
                let ns_window: *mut AnyObject = unsafe { objc2::msg_send![view, window] };
                let class = if ns_window.is_null() {
                    "nil".to_string()
                } else {
                    unsafe { (*ns_window).class().name().to_string() }
                };
                log::info!("{label}: ns_view={view:p} ns_window={ns_window:p} class={class}");
            }
        }
    }

    // ------------------------------------------------------------------
    // main
    // ------------------------------------------------------------------

    pub fn main() {
        install_logger();
        log::info!("dnd-lab starting (pid {})", std::process::id());
        gpui_platform::application().run(|cx: &mut App| {
            let source = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::new(px(120.), px(120.)),
                    size: size(px(460.), px(420.)),
                })),
                titlebar: Some(TitlebarOptions {
                    title: Some("dnd-lab SOURCE".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let target = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::new(px(700.), px(120.)),
                    size: size(px(460.), px(420.)),
                })),
                titlebar: Some(TitlebarOptions {
                    title: Some("dnd-lab TARGET".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let src = cx.open_window(source, |window, cx| {
                log_window_identity("SOURCE", window);
                cx.new(|cx| SourceView {
                    focus: cx.focus_handle(),
                    rows: vec![
                        ("alpha.txt".to_string(), false),
                        ("beta.txt".to_string(), false),
                        ("sub".to_string(), true),
                    ],
                    status: "no drop yet".into(),
                })
            });
            let tgt = cx.open_window(target, |window, _cx| {
                log_window_identity("TARGET", window);
                _cx.new(|_| TargetView {
                    status: "no drop yet".into(),
                })
            });
            log::info!("windows opened: source={:?} target={:?}", src.is_ok(), tgt.is_ok());
            let installed = ferail_shell_mac::install_native_drag_operations();
            log::info!("ferail_shell_mac::install_native_drag_operations() = {installed}");
            install_probe();
            let out = std::env::temp_dir().join("dnd-lab-drops");
            let _ = std::fs::create_dir_all(&out);
            log::info!("a Finder drop writes promised files wherever you drop; scratch dir: {}", out.display());
            let _ = PathBuf::new();
        });
    }
}

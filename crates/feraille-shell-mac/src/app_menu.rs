//! Application menu bar (`NSApp.mainMenu`) and the standard About panel.
//!
//! Built once at startup via [`install_app_menu`]. About panel content
//! is supplied via `orderFrontStandardAboutPanelWithOptions:` so the
//! unbundled cargo-run binary still renders a Finder-style About panel
//! with our app name, tagline, version, and copyright — same look as
//! `NSApp.orderFrontStandardAboutPanel:` would give a properly bundled
//! `.app`.
//!
//! The first pass deliberately stays small: App / Edit / Window
//! submenus, with custom selectors only for About and Settings. Hide /
//! Quit / Cut / Copy / Paste / Minimize / Zoom / Close use built-in
//! AppKit selectors that ride the responder chain — no plumbing
//! needed. File / View / Go menus belong to the Feraille app layer
//! (they need to call into `App` state) and land in a follow-up.

use std::cell::RefCell;

use objc2::declare_class;
use objc2::msg_send;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::Retained;
use objc2::sel;
use objc2::{ClassType, DeclaredClass};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSObject, NSObjectProtocol, NSString};

thread_local! {
    /// Pre-built dictionary passed to `orderFrontStandardAboutPanelWithOptions:`
    /// when the user clicks About. Built once in [`install_app_menu`] and
    /// looked up from the menu-action selector below.
    static ABOUT_OPTIONS: RefCell<Option<Retained<NSDictionary<NSString, NSString>>>> =
        const { RefCell::new(None) };
}

declare_class!(
    /// Tiny Objective-C target subclass that backs the About and Settings
    /// menu items. Selectors land here, then bridge back into AppKit
    /// (About) or no-op (Settings, until a real Settings window exists).
    pub struct AppMenuTarget;

    unsafe impl ClassType for AppMenuTarget {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FeraAppMenuTarget";
    }

    impl DeclaredClass for AppMenuTarget {
        type Ivars = ();
    }

    unsafe impl AppMenuTarget {
        #[method(showAbout:)]
        fn show_about(&self, _sender: &NSMenuItem) {
            let mtm = MainThreadMarker::from(self);
            let app = NSApplication::sharedApplication(mtm);
            ABOUT_OPTIONS.with(|cell| {
                if let Some(opts) = cell.borrow().as_ref() {
                    // Bypass the typed binding (which wants
                    // NSDictionary<NSAboutPanelOptionKey, AnyObject>):
                    // our dict is NSDictionary<NSString, NSString>, and
                    // NSAboutPanelOptionKey is a typedef for NSString so
                    // it's interchangeable at the obj-c runtime level.
                    let dict_ref: &NSDictionary<NSString, NSString> = opts.as_ref();
                    unsafe {
                        let _: () = msg_send![
                            &*app,
                            orderFrontStandardAboutPanelWithOptions: dict_ref,
                        ];
                    }
                } else {
                    // Defensive: if install_app_menu wasn't called, fall
                    // back to the bare About panel. Better than nothing.
                    unsafe { app.orderFrontStandardAboutPanel(None) };
                }
            });
        }

        #[method(showSettings:)]
        fn show_settings(&self, _sender: &NSMenuItem) {
            // Iter-5.7+ will open a real Settings window. The menu entry
            // exists today so the Cmd+, shortcut and the menu structure
            // are in place; clicking is a no-op for now.
        }
    }
);

unsafe impl NSObjectProtocol for AppMenuTarget {}

impl AppMenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let alloc = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send_id![super(alloc), init] }
    }
}

/// Install the application menu bar. Call once at startup, after
/// [`crate::set_app_icon_from_png_bytes`] has set the icon (so the
/// About panel inherits it via `NSImage(named: "NSApplicationIcon")`).
///
/// `tagline` is the short subtitle line under the app name in the
/// About panel (Finder uses "The Macintosh Desktop Experience"). The
/// panel also shows "{name} version {version}" automatically when
/// `version` is supplied.
pub fn install_app_menu(app_name: &str, tagline: &str, version: &str, copyright: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };

    // Build the About options dictionary once and stash it where the
    // menu-action selector can find it.
    let about_options = build_about_options(app_name, tagline, version, copyright);
    ABOUT_OPTIONS.with(|cell| *cell.borrow_mut() = Some(about_options));

    let target = AppMenuTarget::new(mtm);

    let main_menu = NSMenu::new(mtm);
    main_menu.addItem(&build_app_submenu(mtm, &target, app_name));
    main_menu.addItem(&build_edit_submenu(mtm));
    main_menu.addItem(&build_window_submenu(mtm));

    let app = NSApplication::sharedApplication(mtm);
    app.setMainMenu(Some(&main_menu));
}

fn build_about_options(
    app_name: &str,
    tagline: &str,
    version: &str,
    copyright: &str,
) -> Retained<NSDictionary<NSString, NSString>> {
    // Standard keys consumed by `orderFrontStandardAboutPanelWithOptions:`.
    // Layout in the resulting About panel:
    //   ApplicationIcon  ← inherited from NSImage("NSApplicationIcon")
    //   ApplicationName       (bold, large)
    //   ApplicationVersion    (subtitle line — we use it for the tagline)
    //   "{ApplicationName} version {Version}"
    //   Copyright             (smaller footer line)
    let key_name = NSString::from_str("ApplicationName");
    let key_app_version = NSString::from_str("ApplicationVersion");
    let key_version = NSString::from_str("Version");
    let key_copyright = NSString::from_str("Copyright");

    let val_name = NSString::from_str(app_name);
    let val_app_version = NSString::from_str(tagline);
    let val_version = NSString::from_str(version);
    let val_copyright = NSString::from_str(copyright);

    let keys: [&NSString; 4] = [&key_name, &key_app_version, &key_version, &key_copyright];
    // `from_slice` would require `IsRetainable`, which NSString doesn't
    // satisfy because it has a mutable subclass (NSMutableString).
    // `from_vec` only needs `Message`, so move ownership in.
    let values: Vec<Retained<NSString>> =
        vec![val_name, val_app_version, val_version, val_copyright];

    NSDictionary::from_vec(&keys, values)
}

fn build_app_submenu(
    mtm: MainThreadMarker,
    target: &AppMenuTarget,
    app_name: &str,
) -> Retained<NSMenuItem> {
    unsafe {
        let item = NSMenuItem::new(mtm);
        let submenu = NSMenu::new(mtm);
        // The submenu's title is conventionally the app name; on the
        // menu bar this is the bold first entry.
        submenu.setTitle(&NSString::from_str(app_name));

        // About Feraille
        let about_title = format!("About {app_name}");
        let about = make_item(mtm, &about_title, sel!(showAbout:), "");
        about.setTarget(Some(target));
        submenu.addItem(&about);

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        // Settings… (Cmd+,)
        let settings = make_item(mtm, "Settings…", sel!(showSettings:), ",");
        settings.setTarget(Some(target));
        submenu.addItem(&settings);

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        // Hide Feraille (Cmd+H) — first responder via NSApplication.
        let hide_title = format!("Hide {app_name}");
        submenu.addItem(&make_item(mtm, &hide_title, sel!(hide:), "h"));

        // Hide Others (Cmd+Option+H)
        let hide_others = make_item(mtm, "Hide Others", sel!(hideOtherApplications:), "h");
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::NSEventModifierFlagCommand
                | NSEventModifierFlags::NSEventModifierFlagOption,
        );
        submenu.addItem(&hide_others);

        // Show All
        submenu.addItem(&make_item(
            mtm,
            "Show All",
            sel!(unhideAllApplications:),
            "",
        ));

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        // Quit Feraille (Cmd+Q)
        let quit_title = format!("Quit {app_name}");
        submenu.addItem(&make_item(mtm, &quit_title, sel!(terminate:), "q"));

        item.setSubmenu(Some(&submenu));
        item
    }
}

fn build_edit_submenu(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    unsafe {
        let item = NSMenuItem::new(mtm);
        let submenu = NSMenu::new(mtm);
        submenu.setTitle(&NSString::from_str("Edit"));

        // All entries here ride the first-responder chain so they "just
        // work" for our text inputs and any future content that wires
        // standard editing selectors.
        submenu.addItem(&make_item(mtm, "Undo", sel!(undo:), "z"));
        let redo = make_item(mtm, "Redo", sel!(redo:), "z");
        redo.setKeyEquivalentModifierMask(
            NSEventModifierFlags::NSEventModifierFlagCommand
                | NSEventModifierFlags::NSEventModifierFlagShift,
        );
        submenu.addItem(&redo);

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        submenu.addItem(&make_item(mtm, "Cut", sel!(cut:), "x"));
        submenu.addItem(&make_item(mtm, "Copy", sel!(copy:), "c"));
        submenu.addItem(&make_item(mtm, "Paste", sel!(paste:), "v"));

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        submenu.addItem(&make_item(mtm, "Select All", sel!(selectAll:), "a"));

        item.setSubmenu(Some(&submenu));
        item
    }
}

fn build_window_submenu(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    unsafe {
        let item = NSMenuItem::new(mtm);
        let submenu = NSMenu::new(mtm);
        submenu.setTitle(&NSString::from_str("Window"));

        // Minimize (Cmd+M), Zoom — performMiniaturize:/performZoom: are
        // NSWindow responder methods.
        submenu.addItem(&make_item(mtm, "Minimize", sel!(performMiniaturize:), "m"));
        submenu.addItem(&make_item(mtm, "Zoom", sel!(performZoom:), ""));

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        // Close Window (Cmd+W)
        submenu.addItem(&make_item(mtm, "Close Window", sel!(performClose:), "w"));

        item.setSubmenu(Some(&submenu));
        item
    }
}

/// Build a basic NSMenuItem with default modifier mask (Command only).
/// Caller can override the mask after construction if needed.
unsafe fn make_item(
    mtm: MainThreadMarker,
    title: &str,
    action: objc2::runtime::Sel,
    key: &str,
) -> Retained<NSMenuItem> {
    let title_ns = NSString::from_str(title);
    let key_ns = NSString::from_str(key);
    let item: Retained<NSMenuItem> = msg_send_id![
        mtm.alloc::<NSMenuItem>(),
        initWithTitle: &*title_ns,
        action: Some(action),
        keyEquivalent: &*key_ns,
    ];
    item
}

//! Application menu bar (`NSApp.mainMenu`) and the standard About panel.
//!
//! Built once at startup via [`install_app_menu`]. Menu items for
//! Feraille-owned actions are emitted from
//! [`feraille_core::commands::all_commands`] — the menu is a *view*
//! over the command catalogue. Picking an item fires
//! [`register_command_callback`]'s registered closure with the
//! corresponding [`CommandId`]; the host app routes from there.
//!
//! Built-in AppKit actions (Hide / Quit / Cut / Copy / Minimize /
//! Zoom / Close) ride the responder chain unchanged — they are
//! deliberately NOT in the catalogue and are emitted here with their
//! AppKit selectors hard-coded.

use std::cell::RefCell;

use feraille_core::commands::{
    all_commands, Category, CommandId, CommandSpec, Shortcut,
};
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
    /// Pre-built dictionary passed to `orderFrontStandardAboutPanelWithOptions:`.
    /// Built once in [`install_app_menu`].
    static ABOUT_OPTIONS: RefCell<Option<Retained<NSDictionary<NSString, NSString>>>> =
        const { RefCell::new(None) };

    /// Owned reference to the menu's action target. NSMenuItem holds
    /// targets weakly; without this our `Retained` would drop and the
    /// items would go disabled.
    static APP_MENU_TARGET: RefCell<Option<Retained<AppMenuTarget>>> =
        const { RefCell::new(None) };

    /// Index into [`all_commands()`] for each NSMenuItem we emit. The
    /// item carries `setTag(idx)`; the dispatch selector uses the tag
    /// to look up the command's [`CommandId`] here, then fires the
    /// registered callback. Built once at install time so the lookup
    /// doesn't depend on ordering of `all_commands()` at runtime.
    static TAG_TO_COMMAND: RefCell<Vec<CommandId>> = const { RefCell::new(Vec::new()) };

    /// Callback registered by the host app. `Box<dyn Fn>` so the host
    /// can close over an `EventLoopProxy` or whatever it needs to
    /// reach its main state.
    static COMMAND_CALLBACK: RefCell<Option<Box<dyn Fn(CommandId) + 'static>>> =
        const { RefCell::new(None) };
}

declare_class!(
    /// Tiny Objective-C target subclass behind the app menu. One
    /// dispatch selector for every Feraille-owned command (looked up
    /// by tag); a separate selector for About because that one stays
    /// purely AppKit-local.
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
        /// Dispatch a Feraille-owned command. The sender's tag is the
        /// index into `TAG_TO_COMMAND`; we hand the resolved id to
        /// whatever closure the host registered.
        #[method(actCommand:)]
        fn act_command(&self, sender: &NSMenuItem) {
            let tag = unsafe { sender.tag() } as usize;
            let id = TAG_TO_COMMAND.with(|t| t.borrow().get(tag).copied());
            let Some(id) = id else { return };
            COMMAND_CALLBACK.with(|cell| {
                if let Some(cb) = cell.borrow().as_ref() {
                    cb(id);
                }
            });
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

/// Replace the host app's command callback. Pass `None` to clear.
/// Fired on the main thread when the user picks a menu item that
/// maps to a Feraille-owned [`CommandId`].
pub fn register_command_callback(cb: Option<Box<dyn Fn(CommandId) + 'static>>) {
    COMMAND_CALLBACK.with(|cell| *cell.borrow_mut() = cb);
}

/// Show the standard About panel using the dictionary configured by
/// [`install_app_menu`]. Falls back to the bare panel if
/// `install_app_menu` hasn't run.
pub fn show_about_panel() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    ABOUT_OPTIONS.with(|cell| {
        if let Some(opts) = cell.borrow().as_ref() {
            let dict_ref: &NSDictionary<NSString, NSString> = opts.as_ref();
            unsafe {
                let _: () = msg_send![
                    &*app,
                    orderFrontStandardAboutPanelWithOptions: dict_ref,
                ];
            }
        } else {
            unsafe { app.orderFrontStandardAboutPanel(None) };
        }
    });
}

/// Install the application menu bar. Call once at startup, on the
/// main thread, after [`crate::set_app_icon_from_png_bytes`].
pub fn install_app_menu(app_name: &str, tagline: &str, version: &str, copyright: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };

    ABOUT_OPTIONS.with(|cell| {
        *cell.borrow_mut() = Some(build_about_options(app_name, tagline, version, copyright));
    });

    let target = AppMenuTarget::new(mtm);
    APP_MENU_TARGET.with(|cell| *cell.borrow_mut() = Some(target.clone()));

    // Reset & rebuild the tag map alongside the menu so tags and
    // command lookups stay in sync.
    TAG_TO_COMMAND.with(|cell| cell.borrow_mut().clear());

    let main_menu = NSMenu::new(mtm);

    // App submenu — App-category commands, then standard AppKit items.
    main_menu.addItem(&build_app_submenu(mtm, &target, app_name));
    // File / View / Go / Edit / Window — pulled from the catalogue
    // (or hard-coded for built-in selectors).
    if let Some(item) = build_category_submenu(mtm, &target, Category::File, "File") {
        main_menu.addItem(&item);
    }
    main_menu.addItem(&build_edit_submenu(mtm));
    if let Some(item) = build_category_submenu(mtm, &target, Category::View, "View") {
        main_menu.addItem(&item);
    }
    if let Some(item) = build_category_submenu(mtm, &target, Category::Go, "Go") {
        main_menu.addItem(&item);
    }
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
    let key_name = NSString::from_str("ApplicationName");
    let key_app_version = NSString::from_str("ApplicationVersion");
    let key_version = NSString::from_str("Version");
    let key_copyright = NSString::from_str("Copyright");

    let val_name = NSString::from_str(app_name);
    let val_app_version = NSString::from_str(tagline);
    let val_version = NSString::from_str(version);
    let val_copyright = NSString::from_str(copyright);

    let keys: [&NSString; 4] = [&key_name, &key_app_version, &key_version, &key_copyright];
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
        submenu.setTitle(&NSString::from_str(app_name));

        // Catalogue-driven entries (About, Settings).
        for spec in all_commands().iter().filter(|s| s.category == Category::App) {
            submenu.addItem(&build_command_item(mtm, target, spec));
        }

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        // Built-in AppKit items below: hide/show/quit ride the
        // responder chain via NSApplication's selectors.
        let hide_title = format!("Hide {app_name}");
        submenu.addItem(&make_responder_item(mtm, &hide_title, sel!(hide:), "h", None));

        let hide_others = make_responder_item(
            mtm,
            "Hide Others",
            sel!(hideOtherApplications:),
            "h",
            Some(NSEventModifierFlags::NSEventModifierFlagCommand
                | NSEventModifierFlags::NSEventModifierFlagOption),
        );
        submenu.addItem(&hide_others);

        submenu.addItem(&make_responder_item(
            mtm,
            "Show All",
            sel!(unhideAllApplications:),
            "",
            None,
        ));

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        let quit_title = format!("Quit {app_name}");
        submenu.addItem(&make_responder_item(
            mtm,
            &quit_title,
            sel!(terminate:),
            "q",
            None,
        ));

        item.setSubmenu(Some(&submenu));
        item
    }
}

fn build_category_submenu(
    mtm: MainThreadMarker,
    target: &AppMenuTarget,
    category: Category,
    title: &str,
) -> Option<Retained<NSMenuItem>> {
    let cmds: Vec<&CommandSpec> = all_commands()
        .iter()
        .filter(|s| s.category == category)
        .collect();
    if cmds.is_empty() {
        return None;
    }

    unsafe {
        let item = NSMenuItem::new(mtm);
        let submenu = NSMenu::new(mtm);
        submenu.setTitle(&NSString::from_str(title));
        for spec in cmds {
            submenu.addItem(&build_command_item(mtm, target, spec));
        }
        item.setSubmenu(Some(&submenu));
        Some(item)
    }
}

fn build_edit_submenu(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    unsafe {
        let item = NSMenuItem::new(mtm);
        let submenu = NSMenu::new(mtm);
        submenu.setTitle(&NSString::from_str("Edit"));

        submenu.addItem(&make_responder_item(mtm, "Undo", sel!(undo:), "z", None));
        submenu.addItem(&make_responder_item(
            mtm,
            "Redo",
            sel!(redo:),
            "z",
            Some(
                NSEventModifierFlags::NSEventModifierFlagCommand
                    | NSEventModifierFlags::NSEventModifierFlagShift,
            ),
        ));

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        submenu.addItem(&make_responder_item(mtm, "Cut", sel!(cut:), "x", None));
        submenu.addItem(&make_responder_item(mtm, "Copy", sel!(copy:), "c", None));
        submenu.addItem(&make_responder_item(mtm, "Paste", sel!(paste:), "v", None));

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        submenu.addItem(&make_responder_item(
            mtm,
            "Select All",
            sel!(selectAll:),
            "a",
            None,
        ));

        item.setSubmenu(Some(&submenu));
        item
    }
}

fn build_window_submenu(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    unsafe {
        let item = NSMenuItem::new(mtm);
        let submenu = NSMenu::new(mtm);
        submenu.setTitle(&NSString::from_str("Window"));

        submenu.addItem(&make_responder_item(
            mtm,
            "Minimize",
            sel!(performMiniaturize:),
            "m",
            None,
        ));
        submenu.addItem(&make_responder_item(
            mtm,
            "Zoom",
            sel!(performZoom:),
            "",
            None,
        ));

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        submenu.addItem(&make_responder_item(
            mtm,
            "Close Window",
            sel!(performClose:),
            "w",
            None,
        ));

        item.setSubmenu(Some(&submenu));
        item
    }
}

/// Build a Feraille-owned menu item: looks up the spec, sets the
/// title, the tag (index into `TAG_TO_COMMAND`), the key equivalent
/// from the spec's default shortcut, and routes the click through
/// `actCommand:` on `target`.
unsafe fn build_command_item(
    mtm: MainThreadMarker,
    target: &AppMenuTarget,
    spec: &CommandSpec,
) -> Retained<NSMenuItem> {
    // Push into the tag table; the index becomes the menu item's tag.
    let tag = TAG_TO_COMMAND.with(|cell| {
        let mut v = cell.borrow_mut();
        let idx = v.len();
        v.push(spec.id);
        idx
    });

    let title = NSString::from_str(spec.title);
    let (key, mask) = match spec.default_shortcut {
        Some(sc) => (translate_key(sc.key), Some(translate_modifiers(&sc))),
        None => (String::new(), None),
    };
    let key_ns = NSString::from_str(&key);

    let item: Retained<NSMenuItem> = msg_send_id![
        mtm.alloc::<NSMenuItem>(),
        initWithTitle: &*title,
        action: Some(sel!(actCommand:)),
        keyEquivalent: &*key_ns,
    ];
    item.setTag(tag as isize);
    item.setTarget(Some(target));
    if let Some(m) = mask {
        item.setKeyEquivalentModifierMask(m);
    }
    item
}

/// Build an item that targets the responder chain — no Feraille
/// callback, just AppKit's first-responder dispatch. Used for built-in
/// selectors (Hide, Cut, Minimize, …).
unsafe fn make_responder_item(
    mtm: MainThreadMarker,
    title: &str,
    action: objc2::runtime::Sel,
    key: &str,
    mask: Option<NSEventModifierFlags>,
) -> Retained<NSMenuItem> {
    let title_ns = NSString::from_str(title);
    let key_ns = NSString::from_str(key);
    let item: Retained<NSMenuItem> = msg_send_id![
        mtm.alloc::<NSMenuItem>(),
        initWithTitle: &*title_ns,
        action: Some(action),
        keyEquivalent: &*key_ns,
    ];
    if let Some(m) = mask {
        item.setKeyEquivalentModifierMask(m);
    }
    item
}

/// Translate the catalogue's neutral key DSL ("T", "Up", ".") into a
/// macOS NSMenuItem key equivalent string. Named keys map to the
/// Unicode code points AppKit recognises in `keyEquivalent`.
fn translate_key(key: &'static str) -> String {
    match key {
        // Arrow keys (NSUpArrowFunctionKey / NSDownArrowFunctionKey / …).
        "Up" => "\u{F700}".to_string(),
        "Down" => "\u{F701}".to_string(),
        "Left" => "\u{F702}".to_string(),
        "Right" => "\u{F703}".to_string(),
        // Single-char keys: lowercase so the modifier mask, not the
        // case, drives whether Shift is required.
        other => other.to_lowercase(),
    }
}

fn translate_modifiers(sc: &Shortcut) -> NSEventModifierFlags {
    let mut m = NSEventModifierFlags::empty();
    if sc.primary {
        m |= NSEventModifierFlags::NSEventModifierFlagCommand;
    }
    if sc.shift {
        m |= NSEventModifierFlags::NSEventModifierFlagShift;
    }
    if sc.alt {
        m |= NSEventModifierFlags::NSEventModifierFlagOption;
    }
    m
}

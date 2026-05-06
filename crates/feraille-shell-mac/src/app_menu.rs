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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use feraille_core::commands::{all_commands, Category, CommandId, CommandSpec, Shortcut};
use objc2::declare_class;
use objc2::msg_send;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::Retained;
use objc2::sel;
use objc2::{ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSMenu,
    NSMenuItem,
};
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

    /// Snapshot of the host app's tab count. Read by `validateMenuItem:`
    /// to grey out `file.close_tab` when only one tab is open — letting
    /// AppKit's Cmd+W key-equivalent dispatch fall through to
    /// `Close Window` in the Window submenu (which uses NSWindow's
    /// built-in `performClose:`). The host calls `set_tab_count()` on
    /// every tab open / close.
    static TAB_COUNT: Cell<usize> = const { Cell::new(1) };

    /// Per-command on/off state used to render menu checkmarks (radio
    /// or check). The host writes here via [`set_command_state`]
    /// whenever an exclusive group's selection changes (today: theme
    /// preference); `validateMenuItem:` reads it on every menu open
    /// to keep the visual state in sync.
    static COMMAND_STATES: RefCell<HashMap<CommandId, bool>> =
        RefCell::new(HashMap::new());
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

        /// Per-item enable/disable. AppKit calls this for any item
        /// whose target+action point at us; returning `false` greys
        /// the item out *and* takes its key equivalent out of the
        /// dispatch chain so other matching items get a chance.
        ///
        /// Today we only conditionally-disable `file.close_tab`: when
        /// there's a single tab open, Cmd+W falls through to the
        /// Window submenu's Close Window. Everything else stays
        /// enabled.
        #[method(validateMenuItem:)]
        fn validate_menu_item(&self, item: &NSMenuItem) -> objc2::runtime::Bool {
            let tag = unsafe { item.tag() } as usize;
            let id = TAG_TO_COMMAND.with(|t| t.borrow().get(tag).copied());
            let enabled = match id.map(|i| i.0) {
                Some("file.close_tab") => TAB_COUNT.with(|c| c.get()) > 1,
                _ => true,
            };
            // Mirror the per-command on/off state into the item's
            // checkmark. Items not registered in COMMAND_STATES default
            // to "off" — i.e. no checkmark — which is the right
            // behaviour for non-toggle commands.
            if let Some(id) = id {
                let on = COMMAND_STATES.with(|s| s.borrow().get(&id).copied().unwrap_or(false));
                let state = if on { NSControlStateValueOn } else { NSControlStateValueOff };
                unsafe { item.setState(state) };
            }
            objc2::runtime::Bool::new(enabled)
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

/// Update the snapshot of the active window's tab count. Read by
/// `validateMenuItem:` to grey out `file.close_tab` when there's
/// only one tab. Call from the host app whenever a tab opens /
/// closes / is set on first show. No-op if `install_app_menu`
/// hasn't run yet.
pub fn set_tab_count(n: usize) {
    TAB_COUNT.with(|c| c.set(n));
}

/// Set whether a command's menu item should render a checkmark.
/// `validateMenuItem:` reads this on every menu open, so the change
/// is picked up the next time the user opens the menu — no need to
/// rebuild. Use for radio-button-style exclusive groups (the host
/// flips one to `true` and the rest to `false`).
pub fn set_command_state(id: CommandId, on: bool) {
    COMMAND_STATES.with(|s| {
        s.borrow_mut().insert(id, on);
    });
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
    main_menu.addItem(&build_window_submenu(mtm, &target));
    if let Some(item) = build_category_submenu(mtm, &target, Category::Help, "Help") {
        main_menu.addItem(&item);
    }

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
        for spec in all_commands()
            .iter()
            .filter(|s| s.category == Category::App)
        {
            submenu.addItem(&build_command_item(mtm, target, spec));
        }

        submenu.addItem(&NSMenuItem::separatorItem(mtm));

        // Built-in AppKit items below: hide/show/quit ride the
        // responder chain via NSApplication's selectors.
        let hide_title = format!("Hide {app_name}");
        submenu.addItem(&make_responder_item(
            mtm,
            &hide_title,
            sel!(hide:),
            "h",
            None,
        ));

        let hide_others = make_responder_item(
            mtm,
            "Hide Others",
            sel!(hideOtherApplications:),
            "h",
            Some(
                NSEventModifierFlags::NSEventModifierFlagCommand
                    | NSEventModifierFlags::NSEventModifierFlagOption,
            ),
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
    // Extract the theme cluster (View-only) into its own sub-submenu so
    // the View menu doesn't grow a flat run of "Light / Dark / Match
    // System" rows. Other categories keep the simple flat layout.
    let (cmds, theme_cmds): (Vec<&CommandSpec>, Vec<&CommandSpec>) = all_commands()
        .iter()
        .filter(|s| s.category == category)
        .partition(|s| !s.id.0.starts_with("view.theme_"));
    if cmds.is_empty() && theme_cmds.is_empty() {
        return None;
    }

    unsafe {
        let item = NSMenuItem::new(mtm);
        let submenu = NSMenu::new(mtm);
        submenu.setTitle(&NSString::from_str(title));
        for spec in cmds {
            submenu.addItem(&build_command_item(mtm, target, spec));
        }
        if !theme_cmds.is_empty() {
            submenu.addItem(&NSMenuItem::separatorItem(mtm));
            submenu.addItem(&build_subgroup_submenu(mtm, target, "Theme", &theme_cmds));
        }
        item.setSubmenu(Some(&submenu));
        Some(item)
    }
}

/// Build a nested submenu item titled `title` containing one row per
/// command in `cmds`. Used to cluster mutually-exclusive picks (the
/// theme group) inside a category submenu.
fn build_subgroup_submenu(
    mtm: MainThreadMarker,
    target: &AppMenuTarget,
    title: &str,
    cmds: &[&CommandSpec],
) -> Retained<NSMenuItem> {
    unsafe {
        let item = NSMenuItem::new(mtm);
        item.setTitle(&NSString::from_str(title));
        let submenu = NSMenu::new(mtm);
        submenu.setTitle(&NSString::from_str(title));
        for spec in cmds {
            submenu.addItem(&build_command_item(mtm, target, spec));
        }
        item.setSubmenu(Some(&submenu));
        item
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

fn build_window_submenu(mtm: MainThreadMarker, target: &AppMenuTarget) -> Retained<NSMenuItem> {
    unsafe {
        let item = NSMenuItem::new(mtm);
        let submenu = NSMenu::new(mtm);
        submenu.setTitle(&NSString::from_str("Window"));

        // Catalogue-driven entries first (next/prev tab).
        let cat_cmds: Vec<&CommandSpec> = all_commands()
            .iter()
            .filter(|s| s.category == Category::Window)
            .collect();
        let had_cat = !cat_cmds.is_empty();
        for spec in cat_cmds {
            submenu.addItem(&build_command_item(mtm, target, spec));
        }
        if had_cat {
            submenu.addItem(&NSMenuItem::separatorItem(mtm));
        }

        // Built-in AppKit items.
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

/// Translate the catalogue's neutral key DSL ("T", "Up", ".", "F5",
/// "Backspace") into a macOS NSMenuItem key equivalent string. Named
/// keys map to the Unicode code points AppKit recognises in
/// `keyEquivalent`.
fn translate_key(key: &'static str) -> String {
    match key {
        // Arrow keys (NSUpArrowFunctionKey / NSDownArrowFunctionKey / …).
        "Up" => "\u{F700}".to_string(),
        "Down" => "\u{F701}".to_string(),
        "Left" => "\u{F702}".to_string(),
        "Right" => "\u{F703}".to_string(),
        // Function keys (NSF1FunctionKey = U+F704 .. NSF12FunctionKey = U+F70F).
        "F1" => "\u{F704}".to_string(),
        "F2" => "\u{F705}".to_string(),
        "F3" => "\u{F706}".to_string(),
        "F4" => "\u{F707}".to_string(),
        "F5" => "\u{F708}".to_string(),
        "F6" => "\u{F709}".to_string(),
        "F7" => "\u{F70A}".to_string(),
        "F8" => "\u{F70B}".to_string(),
        "F9" => "\u{F70C}".to_string(),
        "F10" => "\u{F70D}".to_string(),
        "F11" => "\u{F70E}".to_string(),
        "F12" => "\u{F70F}".to_string(),
        // Editing keys.
        "Backspace" => "\u{0008}".to_string(),
        "Delete" => "\u{F728}".to_string(),
        "Tab" => "\u{0009}".to_string(),
        "Enter" => "\u{000D}".to_string(),
        "Escape" => "\u{001B}".to_string(),
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

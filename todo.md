# Feraille — Open Risks & Backlog

Free-form list of things to look into later. Not a roadmap (see
[docs/ROADMAP.md](docs/ROADMAP.md)) and not a feature ledger (see
[docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)) — both of those are
structured. This file is for the unstructured "remember to revisit X"
notes that would otherwise crowd CLAUDE.md or rot in code comments.

Two buckets: **Near-term** for items that would meaningfully improve
the app in the next handful of iters, and **Later** for architectural
or speculative work that needs design before code. When an item ships,
delete it; the commit + NOTES.md entry is the record.

## Near-term

### Command system follow-throughs (iter-5.8 unlocks)

- **Migrate keyboard handler to dispatch via `CommandId`.** The
  keyboard match in [crates/feraille-app/src/main.rs](crates/feraille-app/src/main.rs)
  still hard-codes the (key, modifier) → method calls that the menu
  bar already routes through `feraille_core::commands`. Convert that
  match to translate keystrokes into `CommandId`s and dispatch through
  the same `AppEvent::Command` arm — single source of truth, sets up
  user-remappable bindings later. Pure refactor.
- **Extend the catalogue with existing keyboard-only commands.** All
  these have App methods + keybindings but no menu/catalogue entry:
  - `file.refresh` (F5)
  - `file.new_folder` (Cmd+Shift+N)
  - `file.copy_path` (Cmd+Shift+C)
  - `file.reveal_in_finder` (Cmd+Opt+R)
  - `file.move_to_trash` (Cmd+Backspace / Delete)
  - `view.toggle_preview` (Cmd+P)
  - `window.next_tab` / `window.prev_tab` (Cmd+}/Cmd+{)

  Adding them = automatic menu items + future command-palette entries
  + future remap, no extra wiring.
- **Settings window — real one.** Iter-5.11 ships an NSAlert
  placeholder that prints current state. Replace with a proper modal
  panel (theme picker, hidden-files toggle, sidebar-width slider,
  …) backed by a config file so changes persist across runs.
- **Help submenu.** At minimum, "Feraille on GitHub" → `open` URL.
  Optional: keyboard-shortcuts cheat sheet generated from the
  catalogue.

### Other gaps that matter today

- **Flag unusual filenames in the UI.** Filenames are arbitrary byte
  sequences (within fs encoding rules), and some characters are
  visually invisible or ambiguous. The list and the rename editor
  should call them out so the user notices what they're dealing with
  — pre-rename and on the row itself. Cases to surface:
  - **Trailing or leading whitespace** (` foo` / `foo `) — easy to
    type by accident, invisible in monospace columns.
  - **Hidden / zero-width unicode** — ZWSP (U+200B), ZWNJ, BOM (U+FEFF),
    soft hyphen, RTL/LTR marks, U+202E override, etc.
  - **Control characters** — anything `< U+0020` plus DEL.
  - **Look-alike Cyrillic/Greek/etc.** — `а` (U+0430) vs `a` (U+0061).
    Probably out of scope for v1; ZWSP / trailing space first.
  - **Combining-character soup** — long combiner sequences that aren't
    obvious from the rendered glyph.

  Surface ideas: a small pill or icon on the row when the name has
  any of these; in the rename editor, render whitespace as a visible
  glyph (·) and combiners as their codepoint. Prereq: pre-rename we
  should also *show* the name as it actually is so the user knows
  what they're starting from. Inline rename already preserves
  whitespace verbatim post iter-5.7.5; this is the visibility half.

- **Drop targets** (drag-into-folder, drag-into-tree, drag-into-tab).
  Drag-out is done; the inverse pasteboard handling is the missing
  half of file-manager drag-drop.
- **TextInput IME / composition.** No full input-method-editor support
  yet. Will matter for non-Latin keyboards and Asian-language naming.
- **Volume display name.** [crates/feraille-app/src/main.rs:1053](crates/feraille-app/src/main.rs#L1053)
  still parses the path; fetch the real macOS volume label.

## Later

### Architectural

- **Streaming filesystem enumeration with cancellation.** Eager batch
  enumerate is the biggest performance gap for large or slow folders.
  Spec drafted in [docs/features/STREAMING_ENUMERATION.md](docs/features/STREAMING_ENUMERATION.md);
  implementation is multi-iter and should not begin without a fresh
  review of the spec first.
- **NodeStore identity model port.** Stable-id mapping that survives
  rename / move / mount changes — the durable substrate for ant trail,
  selection, and external watcher events. Deferred until streaming
  enumeration is in place; the two designs are coupled.
- **Status progress / task aggregation.** A central in-flight-task
  view (count, bytes, ETA) shared by enumeration, prefetch, copy,
  trash, search. Today the `ProgressStrip` is single-task and per-call.

### Feature surfaces (need design before code)

- **Real previews.** Text / image / PDF / Quick Look. Each is an
  async cancellable provider that returns a renderable bitmap or text
  buffer. Quick Look in particular is its own AppKit subsystem.
- **Persistent Ant Trail + metadata DB.** SQLite for ant-trail
  history, magic cache, recent folders, thumbnails. Per-user
  database; needs schema and migration plan.
- **Disk usage.** Per-folder size pipeline, off-thread, cache-keyed
  by folder identity + mtime / change token.
- **Duplicate finder.** Size → partial-hash → full-hash funnel, all
  off-thread.
- **Command palette UI.** Fuzzy search over `all_commands()`. Needs a
  modal control + scoring algorithm + keybinding system to invoke it.
- **User-overridable key bindings.** Config file format (JSON / TOML /
  whatever the rest of the app picks), override layer on top of
  `default_shortcut`, per-OS sections.
- **Plugin / scripting surface.** Letting external scripts register or
  invoke commands by id. Needs a sandbox / IPC story; far future.

## Branding / packaging

- **Rework the app icon to macOS conventions.** The current icon
  ([crates/feraille-app/resources/feraille.png](crates/feraille-app/resources/feraille.png))
  is the Windows-shaped folder reused from Ferail. Macs expect the
  standard squircle silhouette — a square with rounded borders, full-bleed
  background, content centred — at 1024×1024 with the canonical macOS
  rounding radius. Generate `.iconset` folder and `iconutil` it into
  `feraille.icns` for eventual `.app` bundling.
- **Ship as a real .app bundle.** Without `Info.plist` + `Contents/
  Resources/AppIcon.icns`, the dock/About icon has to be set at runtime
  (currently via `feraille_shell_mac::set_app_icon_from_png_bytes`) and
  the binary identifies as a generic exec to launch services. cargo-bundle
  or a hand-rolled bundle script is the fix. Triggers that flip this
  from "nice-to-have" to "needed":
  - **TCC identity matters.** When the Documents/Desktop/Downloads
    permission grants need to stick across runs (today every cargo build
    re-prompts because the binary changes), a stable bundle ID is the fix.
  - **File associations.** "Open With Feraille" on a file from Finder
    requires the bundle to advertise UTIs in `Info.plist`.
  - **Code signing / Gatekeeper.** Anything we want to share with another
    Mac needs a signed bundle, otherwise it's quarantined.
  - **Drag-and-drop from Finder with full UTIs.** Bundle declares which
    document types it accepts.

  Until then, keep `cargo run` as the dev path. When we bundle, expect:
  dev cycle becomes `cargo build && open .../Feraille.app` (or a tools/
  script); stderr disappears unless launched via `open -a` from a terminal;
  TCC grants tied to the bundle ID instead of the binary path.

## Notes from the porting effort

- The "Source" column in [docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)
  references docs in the original Windows project at
  `/Users/jkn/Source/Ferail/`. When something here looks regressed
  compared to Ferail, read the analogous code/doc there before
  redesigning. (See CLAUDE.md → Cross-Reference: Ferail.)

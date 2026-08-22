# Headless Screenshots — The Visual Dev Loop

Ferail can render any UI state to a PNG without opening a visible window.
This is how we (human and AI) iterate on the UI: change code, render the exact
state you care about off-screen, open the PNG, repeat. No manual click-paths,
no screen-capture, no "works on my machine" mouse choreography.

The harness lives in [`screenshot.rs`](../../crates/ferail-gpui/src/screenshot.rs).

← Back to [feature notes](README.md) · related: [TESTING_OVERLAYS.md](TESTING_OVERLAYS.md)

## How it works

`main.rs` dispatches to the headless path the moment it sees `--screenshot`:

1. A GPUI window is opened with `show: false, focus: false` — it never appears
   on screen, on any platform.
2. The requested view is built (Shell by default, or Settings / Disk Usage /
   Viewer / drag-ghost depending on flags).
3. CLI flags are *applied* to the live entity through the real update paths —
   `navigate`, `select_tab`, `start_duplicate_scan`, `dispatch_keystroke`, etc.
   We drive the actual code, not a mock, so focus/keymap/subscription routing is
   exercised exactly as it is for a real user.
4. A **2500 ms settle timer** runs so async prefetch (magic sniffing, quarantine
   checks, Quick Look thumbnails) can land before the frame is sampled.
5. `Window::render_to_image` captures the framebuffer; the PNG is written and the
   process quits.

Because step 3 runs real code, a screenshot run doubles as a smoke test of the
keybinding and async-scheduling paths — if a keystroke routes to the wrong
focus handle or a scan deadlocks, the render reflects it.

## Invocation

```sh
cargo run --bin ferail-gpui -- --screenshot screenshots/<feature>.png [flags…]
```

Per [CLAUDE.md](../../CLAUDE.md), screenshots go in `screenshots/` (gitignored
scratch). If a committed doc needs to reference an image, copy it into
`docs/images/` instead, or the link breaks on GitHub.

`--help` prints the full, authoritative flag list — the source of truth is
`print_help()` in `screenshot.rs`, not this doc.

## The flag families

- **Frame** — `--width`, `--height`, `--scale`, `--theme light|dark`,
  `--ui-scale`.
- **Navigation** — `--navigate <path>` (repeatable; chaining seeds realistic
  ant-trail visit counts), `--new-tab <path>` (repeatable), `--tab <idx>`,
  `--expand <path>` (unfurl the sidebar tree), `--show-hidden`.
- **Selection & sort** — `--select-row N`, `--select-name <name>`,
  `--select-rows a,b,c` (first = anchor, last = lead), `--sort <col[-desc]>`,
  `--view grid|list`.
- **Search & dupes** — `--filter <text>`, `--search`, `--search-subtree <needle>`,
  `--find-duplicates`, `--dupe-panel`.
- **Live input simulation** — `--breadcrumb <text>` (enters Cmd+L edit mode and
  *types* through the completion provider), `--keys "<gpui keystrokes>"`
  (dispatched through the real window key path; `pause` token waits out async UI
  between keys), `--context-menu-row N` (synthesises a real mouse-move +
  right-click over row N, so the row context menu builds exactly as it does for
  a user — it lives in a mouse-event listener, so no action can open it),
  `--context-menu-background` (same synthesis aimed at the file-list body's
  midpoint, capturing the empty-space folder menu; point it at an empty
  folder, or one whose rows stop above the midpoint).
- **Panels & overlays** — `--preview`, `--properties` (Get Info),
  `--rename`, `--new-folder`, `--shortcuts-help[-filter]`, `--simulate-toast`,
  `--simulate-progress`, `--simulate-task-panel`, `--update-dialog <state>`
  (Software Update dialog seeded with `checking` / `uptodate` / `available` /
  `elsewhere` / `noasset` / `downloading` / `done` / `failed` — no network;
  `live` runs the real GitHub check, the one networked capture).
- **Alternate windows** — `--settings <page>`, `--disk-usage <path>`
  (`--du-depth`, `--du-coloring`), `--viewer <path>` (`--viewer-adjust`),
  `--drag-ghost N`.

## Complex examples

These chain flags to reach states that would be tedious or impossible to set up
by hand.

### Multi-tab session, second tab active, icon view, dark

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/multitab-grid-dark.png \
  --theme dark --width 1400 --height 900 \
  --navigate ~/Documents \
  --new-tab ~/Downloads --new-tab ~/Pictures \
  --tab 2 --view grid
```

Opens three tabs, makes the Pictures tab active, forces the icon grid (without
touching the user's persisted default), all in dark theme.

### Recursive search → select a hit → show it in the preview pane

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/subtree-search-preview.png \
  --width 1200 --navigate ~/Source/Ferail \
  --search-subtree "Cargo.toml" \
  --select-name Cargo.toml --preview
```

Launches the streaming subtree walk exactly as Enter-in-the-filter-box does,
parks the cursor on the first match, and opens the preview pane (needs
`--width ≥ 900` or the pane auto-hides).

### Duplicate-finder card panel

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/dupes-panel.png \
  --width 1300 --height 850 \
  --navigate ~/Downloads --dupe-panel
```

`--dupe-panel` implies `--find-duplicates` and forces the dedicated card
presentation regardless of the saved `DupePresentation` setting.

### Breadcrumb autocomplete, then drive it with the keyboard

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/breadcrumb-pick.png \
  --navigate ~ \
  --breadcrumb "~/Doc" \
  --keys "down down pause enter"
```

`--breadcrumb` enters Cmd+L edit mode and *types* `~/Doc` through the
completion provider (so the autocomplete menu populates), then `--keys` sends
real keystrokes through the window: arrow down twice, `pause` to let the menu's
async accept-task settle, then `enter` to commit the highlighted completion.

### Command palette driven entirely by keyboard

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/palette-run.png \
  --navigate ~/Documents \
  --shortcuts-help-filter "new folder" \
  --keys "enter"
```

Opens the shortcuts/command-palette overlay pre-filtered to "new folder", then
presses Enter to run the top match — verifying the palette's filter + dispatch
wiring headlessly. (`--shortcuts-help-filter` is applied *before* `--keys`
precisely so the keyboard can drive the open overlay.)

### Task panel with live + historical tasks

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/task-panel.png \
  --navigate ~/Source --simulate-task-panel --simulate-progress 0.62
```

Opens the task panel pre-populated with two running tasks (enumeration, disk
usage) and a seeded "Recent" history covering each outcome — succeeded,
cancelled, failed — plus a determinate footer progress strip at 62%.

### Get Info on a specific row

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/get-info.png \
  --navigate ~/Documents --select-name "Annual Report.pdf" --properties
```

The 2500 ms pre-capture wait lets the background metadata gather land before the
popup is sampled. Without a `--select-*` flag, `--properties` targets the folder
itself.

### Row context menu, on the *first* right-click after a load

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/context-menu-first-right-click.png \
  --navigate ~/Downloads --context-menu-row 2
```

With no `--select-*` flag this captures the case that used to differ from every
later right-click: a freshly loaded folder, nothing selected, menu built from
scratch. Add `--select-rows 1,2` to capture the multi-selection form instead
(bulk "Rename N Items…", no single-only commands). List view only, and the row
must be on screen — the click point comes from the laid-out row geometry.

Because the click is real, the capture also exercises the menu's async fill-in:
"Open With" starts as a disabled placeholder and the open menu rebuilds itself
once the off-thread LaunchServices fetch lands.

### Disk-usage treemap, depth-limited, coloured by depth

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/du-source.png \
  --width 1400 --height 950 \
  --disk-usage ~/Source --du-depth 3 --du-coloring depth
```

Skips the shell entirely and renders the Disk Usage window's treemap straight
into the frame.

### Viewer window with the colour/enhance panel open

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/viewer-adjust.png \
  --viewer ~/Pictures --viewer-adjust
```

A directory builds the full playlist and renders its first file; a single file
renders a one-entry playlist. `--viewer-adjust` opens the adjustments panel.

### Drag ghost in isolation

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/drag-ghost.png --drag-ghost 4
```

Renders the cursor drag-ghost for a 4-item drag against a neutral backdrop —
the only way to capture the ghost headlessly, since it never exists outside a
live drag.

## Gotchas

- **Settle timing.** Selection-by-index/name waits an extra 700 ms for streamed
  enumeration batches before resolving rows; the main 2500 ms settle runs after
  all flags apply. If a capture looks empty or un-prefetched, the data was still
  in flight — it's not a render bug.
- **Overlay compositing.** `render_to_image` does *not* fully composite
  absolute-positioned overlay layers. Toasts (`--simulate-toast`) and some
  dialogs bleed partial state in headless capture but render correctly in the
  live window. Verify those interactively.
- **Path expansion.** Paths get `~` expansion + `canonicalize`, falling back to
  the literal path so navigation to a not-yet-existing path still works.
- **Stage-deferred flags.** A few flags (`--splitter`, `--scroll`, `--ui-scale`,
  `--mac-chrome`) are recognised but emit a single log warning instead of
  acting — they map to features not yet wired in the GPUI shell.

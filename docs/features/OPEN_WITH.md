# Open With & Custom Tools

← [Feature notes](README.md) · [Architecture](../ARCHITECTURE.md) ·
[Context menus](CONTEXT_MENU.md) · [Open work (TODO)](../../TODO.md)

Sections 5 onward are a study of user-defined custom tools: a design, not
a description of code that exists.

The idea: keep double-click on the system default, and let **Open With**
offer, alongside the apps the OS registers, a list of **user-defined
tools**: "run this program on this file", with the user's own arguments.

Two questions were asked of this note:

1. Can we get the handler list from the system? **Yes, and it is already
   built, on all three platforms.** §2 explains what each OS actually
   registers and §3 shows what we already call.
2. Can the user add their own tools? Not today. §5 designs it.

**Short answer.** The system half is done; the gap is everything around
it, no way to pick an app that isn't in the list, no user-defined
commands, no "always open with", and the submenu hides itself on a
multi-selection even though the dispatch already handles many files. The
custom-tool half should be a sibling of the shipped
[`ferail_core::terminal::TerminalSpec`](../../crates/ferail-core/src/terminal.rs)
the same "program + pre-split argv tokens + `{placeholder}` substituted per
token" model, which already solves the quoting problem this feature would
otherwise re-invent badly.

---

<!-- toc depth=2 -->

- [1. What "programs register to handle file types" actually means](#1-what-programs-register-to-handle-file-types-actually-means)
- [2. What already ships](#2-what-already-ships)
- [3. Measurements](#3-measurements)
- [4. The gaps, precisely](#4-the-gaps-precisely)
- [5. Design proposal - custom tools](#5-design-proposal---custom-tools)
- [6. Security - the part that must not be hand-waved](#6-security---the-part-that-must-not-be-hand-waved)
- [7. Phasing](#7-phasing)
- [8. What not to do](#8-what-not-to-do)
- [9. Verification plan (when built)](#9-verification-plan-when-built)

<!-- /toc -->

## 1. What "programs register to handle file types" actually means

The user's hunch is right, and it works differently on each OS. This
matters because it decides what we can *read* and what we can *write*.

| | macOS | Windows | Linux (freedesktop) |
|---|---|---|---|
| Type identity | **UTI** (`public.png`), derived from extension/magic | file **extension** (`.png`) | **MIME type** (`image/png`) |
| How an app registers | `CFBundleDocumentTypes` / `LSItemContentTypes` in its `Info.plist`, indexed by Launch Services when the bundle appears | registry: a **ProgID** under `HKCR`, plus `OpenWithProgids` / `OpenWithList` per extension | a `.desktop` entry declaring `MimeType=`, indexed into `mimeinfo.cache` by `update-desktop-database` |
| Enumerate handlers | `NSWorkspace.URLsForApplicationsToOpenURL:` | `SHAssocEnumHandlers` (`ASSOC_FILTER_RECOMMENDED`) | scan XDG app dirs for entries matching the MIME |
| Read the default | `NSWorkspace.URLForApplicationToOpenURL:` | first recommended handler | `xdg-mime query default` |
| **Set** the default | `LSSetDefaultRoleHandlerForContentType`: allowed | **not programmatically settable** (Win10+ deliberately blocks it; apps must send the user to Settings → Default apps) | `xdg-mime default`: allowed |

Two consequences worth stating up front:

- The three registries key on **three different things**. A tool-matching
  rule of our own should therefore key on what we already hold in
  `FileEntry`: extension, kind, name, and treat the OS type as a
  platform detail, or we inherit three incompatible vocabularies.
- **"Always Open With" is not portable.** macOS and Linux can set the
  default; Windows can only *open the OS UI*. Any such command must
  degrade honestly per platform rather than silently doing nothing.

## 2. What already ships

All three shell crates implement the same
`open_with_candidates(path) -> Vec<OpenWithCandidate>` (name, path,
`is_default`), each against its native registry:

| Platform | Implementation |
|---|---|
| macOS | [`ferail-shell-mac/src/open_with.rs`](../../crates/ferail-shell-mac/src/open_with.rs): `URLsForApplicationsToOpenURL:` + `URLForApplicationToOpenURL:`, deduped, default pinned first |
| Windows | [`ferail-shell-win32/src/lib.rs:1406`](../../crates/ferail-shell-win32/src/lib.rs#L1406): `SHAssocEnumHandlers` with `ASSOC_FILTER_RECOMMENDED`, capped at 12 |
| Linux | [`ferail-shell-linux/src/lib.rs:262`](../../crates/ferail-shell-linux/src/lib.rs#L262): `xdg-mime` for the type + default, then a pure-`std` scan of `.desktop` entries (unit-tested) |

The GPUI side is Prime-Directive-clean already:

- `spawn_open_with_warm` fetches candidates **off the UI thread** on
  selection-lead change; the menu builder reads only that warm cache
  ([file_list.rs:2300](../../crates/ferail-gpui/src/file_list.rs#L2300)).
- A cache miss opens a retained submenu with a disabled *"Loading…"* row.
  When the fetch lands, only that submenu is rebuilt through upstream
  `PopupMenu::rebuild`: Finder's "Fetching…" behaviour, without polling or
  replacing the root menu.
- Picking an entry dispatches `OpenWithSlot0..11`, resolved against *the
  same warm snapshot the menu was built from* (a re-fetch could reorder
  candidates and launch the wrong app), then runs
  `open_with_app_many` on the background executor.
- The list view and the icon grid share one `context_menu` delegate
  method, so both already carry it.

## 3. Measurements

`NSWorkspace` handler enumeration, timed via a small ObjC harness
(fresh process per run, Apple silicon):

| Target | Candidates | First call in process | Warm |
|---|---|---|---|
| [README.md](README.md) | 6 | 9.4 ms | 0.03–0.06 ms |
| `t.txt` | 8 | 4.7 ms | 0.02–0.05 ms |
| `Cargo.toml` | 1 | 0.23 ms | 0.03 ms |
| a folder | 7 | 0.27 ms | 0.01–0.04 ms |
| `t.zzqq` (unknown ext) | **0** | 0.14 ms | 0.03 ms |

Two findings:

- **Launch Services is much cheaper than the code assumes.** The source
  comment says "~10–50 ms typical"; the real cost is a one-time ~5–10 ms
  client bootstrap and then **tens of microseconds**. The warm-cache
  machinery is still right (the first call in a process, and cold
  bundles on slow volumes, are unbounded), but richer uses, enumerating
  for several types, precomputing a per-extension default map: are
  effectively free once LS is warm.
- **An unknown extension yields zero candidates and no default**, and the
  current code omits the submenu entirely for an empty set. So the file
  type where the user is *most* likely to want a specific tool is exactly
  the one that offers nothing. That alone justifies this feature.

## 4. The gaps, precisely

| Gap | Detail |
|---|---|
| No **"Other…"** | The user cannot pick an app the OS didn't offer. Note `gpui::PathPromptOptions` has only `files` / `directories` / `multiple` / `prompt`: **no type filter**, and a macOS `.app` is a *directory*, so a portable chooser needs a platform entry point (`NSOpenPanel` with `allowedContentTypes` + `treatsFilePackagesAsDirectories`), not just the gpui prompt. |
| No **custom tools** | The whole subject of §5. |
| No **"Always Open With"** | Nothing calls `LSSetDefaultRoleHandlerForContentType` / `xdg-mime default`; there is no per-extension override of our own either. |
| **Hidden on multi-selection** | The submenu is `SingleOnly`, yet `open_with_slot` already resolves the full selection and calls `open_with_app_many`. The restriction is the *menu*, not the machinery. |
| **Empty set ⇒ no submenu** | See the `.zzqq` row above. |
| **12-slot ceiling** | `OpenWithSlot0..11` are twelve unit actions; the menu `take(12)`s. Custom tools would double the pressure on that scheme: see §5.6. |
| Only in the file pane | The disk-usage treemap menu has Open/Reveal/Quick Look but no Open With. |

## 5. Design proposal - custom tools

### 5.1 The model: a `ToolSpec` sibling of `TerminalSpec`

`ferail_core::terminal` already solved this problem once, correctly:
program + **pre-split argv tokens** + a placeholder substituted **per
token after splitting**, so a path with spaces can never re-split. Copy
that shape into `ferail_core::tools`:

```rust
pub struct ToolSpec {
    pub id: ToolId,                 // uuid, stable across renames
    pub name: String,               // user-facing label (user data, never localized)
    pub program: String,            // absolute path, .app bundle, .desktop, or PATH name
    pub args: Vec<String>,          // split_args() tokens; may contain placeholders
    pub match_rule: MatchRule,      // when to offer it
    pub multi: MultiMode,           // OnePerFile | AllAtOnce
    pub run: RunMode,               // Detached | Tracked | InTerminal
    pub working_dir: WorkingDir,    // FileParent | Fixed(String) | Inherit
    pub confirm: bool,              // ask before running (destructive tools)
}
```

**Placeholders** (extending `DIR_PLACEHOLDER`'s exact mechanism):
`{path}` full path · `{paths}` every selected path, one token each
(`MultiMode::AllAtOnce`) · `{dir}` parent directory · `{name}` leaf ·
`{stem}` leaf without extension · `{ext}` extension. A tool whose args
contain no placeholder gets the paths appended, matching how
`resolved_args` reports `had_placeholder` today.

### 5.2 Matching - and the Prime Directive constraint on it

```rust
pub enum MatchRule {
    Always,
    Kinds(Vec<EntryKind>),          // file / folder / symlink
    Extensions(Vec<String>),        // lowercased, no dot
    NameGlob(String),
    FormatLabel(Vec<String>),       // magic-derived, cached-only: see below
}
```

Matching **must read only fields already cached on `FileEntry`**. The
tempting rule: "offer this tool for anything the magic sniffer calls a
*PNG image*": is a genuine differentiator over Finder, but
`display_magic` is filled *lazily* by the prefetch worker and is empty
when Settings → Performance has file-detail scanning off. So the rule is:
match on it **when present, never compute it at menu time**. A tool that
silently disappears because a sniff hasn't landed is confusing; document
that `FormatLabel` rules are best-effort and pair them with an extension
rule.

### 5.3 Execution

- Build an **argv array; never a shell string.** No `sh -c`, no
  interpolation into a command line. `split_args` + per-token placeholder
  substitution gives argv directly, and that is what makes filenames with
  spaces, quotes, `$`, and newlines safe by construction.
- `Command::status` / `Command::output` are **banned in `ferail-gpui`** by
  the clippy wall: spawn on `cx.background_executor()`, exactly as
  `open_with_slot` already does.
- `RunMode::Detached` for GUI apps (spawn and forget, the current
  behaviour). `RunMode::Tracked` registers a `TaskKind` in the existing
  task registry so a long tool is visible/cancellable in the status bar,
  and reports exit status through the existing error-notification path
  (headline + **Show details** + **Copy**). `RunMode::InTerminal` reuses
  `exec_prefix_for` / the resolved `TerminalSpec` so a CLI tool's output
  lands in the user's terminal instead of nowhere.
- Multi-selection: `AllAtOnce` mirrors `open_with_app_many`'s single
  invocation; `OnePerFile` should be **counted and confirmed above a
  threshold**, "Run *Convert* on 412 items?", because a per-file tool
  over a large selection is a fork bomb the user did not intend.

### 5.4 Persistence

Recommend a **`tools.json`** beside `gpui-state.txt`, not the metadata DB
and not `app_state`:

- `app_state` is a flat `key=value` file: a list of structured records
  does not belong there.
- The metadata DB is explicitly cache-shaped: a version mismatch renames
  it to `.bak` and recreates it ([db.rs:236](../../crates/ferail-meta/src/db.rs#L241)).
  Favorites already accept that risk; hand-authored tool definitions
  (which the user cannot reconstruct) should not.
- JSON makes tools **shareable and hand-editable**, and the app already
  ships an export → edit → import workflow for JSON language packs, so
  the pattern and its UI are precedented.

Load it once into an in-memory cache with the same contract as
`app_state::load()`: **the menu builder must never touch the disk**.

### 5.5 Menu shape

```
Open With ▸
    Preview (default)
    Visual Studio Code
    …system handlers…
    ────────────────
    Optimize PNG            ← custom tools matching this file
    Open in Ghostty
    ────────────────
    Other…                  ← app chooser, adds a pinned entry
    Always Open With ▸      ← macOS/Linux only; Windows opens Settings
```

Also give tools a **top-level presence** for the ones used constantly,
either directly in the row menu above the submenu, or via a per-tool
"show at top level" flag. Burying a daily tool two levels deep is the
main way this feature fails in other file managers.

### 5.6 Dispatch: drop the slot ceiling

Twelve unit actions do not extend to an unbounded tool list. Two options,
both already available in-tree:

- **Closure items.** `PopupMenuItem::new(label).on_click(closure)` is
  supported by gpui-component and already used here for the disabled
  loading placeholder. A tool item captures its `ToolId` and dispatches
  directly. Cost: closure items get no keyboard-shortcut hint and cannot
  be bound in the keymap.
- **A payload-carrying action.** `ferail_core::commands::CommandPayload`
  *already* models exactly this (`OpenWithApp { app_path }`) and currently
  has **no users** anywhere in the shell crates: a leftover seam from the
  Win32-era menu plan. Reviving it for `file.run_tool { tool_id }` keeps
  dispatch uniform and keymap-bindable.

Recommend closures for the tool list (unbounded, no shortcuts needed) and
keeping the slot actions for the system apps that already work.

### 5.7 Settings UI

A **Tools** group on the existing **Plugins** page (today that page holds
only the video-backend/mpv settings, so the name is already aspirational)
or its own page: a list with add/edit/remove/reorder, and an editor with
Name, Program (+ Browse…), Arguments, "Show for", multi-select behaviour,
run mode, and a **live preview of the exact argv** that will be executed.
That preview is the single best safety feature in the whole design: it
makes "what will this actually run?" answerable before it runs.

Icons: a tool needs its own glyph per [ICONS.md](ICONS.md); the spare
upstream pool has nothing tool-shaped (`wrench`/`hammer` are absent), so
expect to vendor one in house style. Chrome strings go through `tr!`;
**tool names and arguments are user data and are never localized.**

## 6. Security - the part that must not be hand-waved

A custom tool is *arbitrary code execution configured through the UI*.
That is legitimate (every serious file manager has it) but it changes the
threat model, so the rules are non-negotiable:

1. **argv, never a shell.** A shell string re-introduces injection through
   filenames; the argv model makes it structurally impossible. If a "run
   through a shell" mode is ever added, it must be an explicit per-tool
   opt-in, labelled as such.
2. **Filenames are not options.** A file literally named `--delete` is
   passed as an argument today. Where the tool supports it, insert `--`
   before the paths; where it does not, the argv preview is the mitigation.
3. **Importing a tool definition is importing an executable.** Any
   import/paste flow must show the full program + argv and require
   confirmation. Never auto-import from a downloaded file.
4. **Confirm bulk runs**, per §5.3.
5. **Quarantined and downloaded files**: running a user tool *on* a
   quarantined file is fine; making the *program* a quarantined binary the
   user just downloaded is worth a warning: `is_quarantined` is already
   on `FileEntry` and the badge machinery exists.
6. **No elevation.** Tools run as the user. The privileged-worker path in
   [FILE_OPS.md](FILE_OPS.md) exists for file operations with a specific,
   audited descriptor; it must not become a generic "run this as root".

## 7. Phasing

1. **Close the cheap gaps first**: they are small and independently
   useful: allow the submenu on multi-selection (the dispatch already
   handles it), add **Other…** (needs the platform app-chooser entry
   point), and stop hiding the submenu when the candidate set is empty.
2. **Custom tools, v1**: `ToolSpec` + `tools.json` + matching + the
   Settings editor with argv preview + closure-backed menu items.
   `Detached` and `InTerminal` run modes.
3. **Polish**: `Tracked` mode with task/notification integration,
   top-level pinning, per-tool icons, tools in the disk-usage menu, and
   import/export of tool sets.
4. **"Always Open With"**: macOS `LSSetDefaultRoleHandlerForContentType`,
   Linux `xdg-mime default`, Windows: open the Settings pane and say so.
   Last because it is the least portable and the easiest to get subtly
   wrong.

## 8. What not to do

- **Don't build a plugin runtime.** A tool is a program plus arguments.
  Anything that starts to look like an embedded scripting host is a
  different, much larger feature.
- **Don't sniff, stat, or read the tool file at menu-open time**: match
  on cached `FileEntry` fields only.
- **Don't re-fetch candidates at dispatch.** The existing code documents
  why (a fresh fetch can reorder and launch the wrong app); tools must
  follow the same snapshot discipline.
- **Don't put tool definitions in the metadata DB** while its policy is
  "recreate on version mismatch".

## 9. Verification plan (when built)

- Unit tests in `ferail-core::tools` for placeholder expansion (spaces,
  quotes, unicode, multi-file `{paths}`) and for `MatchRule`: mirroring
  the existing `terminal.rs` tests.
- A test asserting menu construction performs **no I/O** (the path guard
  already panics on path resolution during render).
- Cross-platform smoke tests for `open_with_candidates` shape.
- Screenshot of the extended submenu and the Settings editor into
  `screenshots/open-with-*.png`.
- Manual: a tool with a space-and-quote-laden filename; a per-file tool
  over a 500-item selection (confirm prompt); a tool whose program was
  deleted (must fail with a real message, not silence).

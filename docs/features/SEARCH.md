# File Search

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Today Ferail only has an *in-directory filter*: a case-insensitive substring
match over the rows already loaded for the current folder
([file_list.rs](../../crates/ferail-gpui/src/file_list.rs), applied in the
streaming load at [shell/loading.rs](../../crates/ferail-gpui/src/shell/loading.rs)).
That is the right behavior for "narrow what I'm looking at" but it is not
*search*: it never leaves the current directory and never consults an index.

This note specifies real search, built in tiers, all behind the
[prime directive](../ARCHITECTURE.md#prime-directive): the UI never blocks on
I/O. Every tier streams incremental results off the UI thread and is
cancellable.

<!-- toc depth=2 -->

- [What is built](#what-is-built)
- [Filter tokens](#filter-tokens)
- [The three tiers](#the-three-tiers)
- [Rely on the OS index where it exists](#rely-on-the-os-index-where-it-exists)
- [Tier 1 - recursive subtree walk (build first)](#tier-1---recursive-subtree-walk-build-first)
- [GPUI integration (shared by all tiers)](#gpui-integration-shared-by-all-tiers)
- [Build order](#build-order)
- [Verification](#verification)

<!-- /toc -->

## What is built

- Tier 0 (in-directory filter): **shipped**.
- Tier 1 (recursive subtree walk): **shipped**: built-in walker, cancellable.
- Tier 2 (global / indexed): **shipped on macOS**: Spotlight via `mdfind`,
  with Tier 1 as the automatic fallback; Windows MFT + Linux Tracker land with
  those ports.

How you trigger it: focus the filter field (Cmd+F), type, and press **Return**
to escalate the in-directory filter into a recursive / Spotlight search of the
current folder and below. Esc clears. The engine is selectable in Settings →
Search & Duplicates. A result file's **Open in New Tab** context command opens
its containing folder in a new tab and selects the exact result; a result
folder opens directly.

**Honest scope: this is the mechanism, not Finder-grade search UX.** What
ships is a *single query box*: free text (substring/name via the walker, or
Spotlight's natural-language name+content query) plus the structured
[filter tokens](#filter-tokens) below, streamed into the list with the hit's
location shown. What it deliberately does **not** have yet, and where the
system explorers are still ahead: filter chips as first-class UI, **saved
smart folders**, and **live-updating** results. Those are the real
follow-ups.

A pinned, live "smart folder" tab is the highest-value next step, and it's
cheap *if Spotlight-backed*: a live `MDQuery` (or a debounced `mdfind` re-run on
an FSEvents tick) gets deltas from the OS index without us walking the disk,
which is exactly how Finder keeps its smart folders fresh. Walker-backed views
and duplicate results are expensive to keep live and should stay snapshots
(re-run on demand). So the model is: ephemeral results tabs by default; opt-in
pin → a Spotlight-backed live tab.

## Filter tokens

The filter box (and, via Enter, subtree search) understands `key:value`
tokens alongside free text. The language lives in
`ferail_core::filter_expr`: one parser, three consumers: the Tier 0
in-directory filter (`shell/loading.rs`), the Tier 1 walker
(`SearchQuery::expr`), and the Spotlight path (text terms feed the
`mdfind` query, metadata terms post-filter the hits), so every surface
enforces identical semantics.

### Autocomplete keys

Every field with a suggestion list (the filter box, the breadcrumb path
editor, and the Go to Folder prompt) follows one contract:

| Key | Does |
|---|---|
| Up / Down | move the highlight inside the suggestion list |
| Tab | accept the highlighted suggestion into the field |
| Enter | run / navigate to **exactly what the field holds** |
| Escape | close the suggestion list, then leave the field |

Enter never silently accepts a suggestion. Pasting a folder path that has
subfolders used to append the first one on Enter, so the user landed
somewhere they never typed; completion is opt-in through Tab precisely so
that cannot happen.

| Token | Values | Notes |
|---|---|---|
| `kind:` / `type:` | `folder` `file` `link` | entry kind; `type:` is a friendly alias |
| `ext:` | `ext:rs`, `ext:pdf` | extension, case-insensitive, no dot |
| `size:` | `>10mb` `<=1gb` `1mb..100mb` `100` | 1024-based units matching the Size column; bare number = bytes, bare value means ≥ |
| `mod:` / `created:` | `today` `yesterday` `week` `month` `year` `>2026-01-01` `2026-01-01..2026-06-30` | local-midnight day boundaries; `>` excludes the named day, `>=` includes it; ranges inclusive |
| `locked:` | `yes` `no` | macOS `UF_IMMUTABLE`/`SF_IMMUTABLE` (Finder's Locked), Windows read-only attribute |

Grammar rules, all deliberate:

- Terms AND together; bare words are substring matches over the name and
  the Format label (so multi-word text now means "all words", not one
  space-containing substring). `"quoted phrase"` restores exact-phrase
  matching.
- A token that fails to parse (`size:banana`, unknown key) degrades to a
  literal substring term, typing never silently changes meaning.
- Metadata predicates read only cached `FileEntry` fields captured at
  enumerate time (`size`, `mtime_unix`, `created_unix`, `locked`, kind,
  name), never fresh I/O, per the prime directive. A value the
  filesystem didn't provide (`created` on some network volumes) fails the
  predicate quietly.
- Dates resolve against a `DateCtx` (now + local zone offset) built per
  load on the worker, so parsing is pure and testable.

**Autocomplete** (`filter_complete.rs`): the compact filter input renders a
small completion menu over `filter_expr::TOKEN_HELP`: typing a key prefix
offers keys with a one-line description, accepting a key chains into its
example values, and an empty field lists the whole token set as a cheat-sheet.
After a plain-name term the menu stays available and appends a chosen token,
so adding a criterion never replaces the search already typed. Static table
lookup only, no I/O; parser tests round-trip `TOKEN_HELP` so the menu can't
advertise syntax the parser rejects.

**Cheat sheet** (`filter_help.rs`): a (?) button to the right of the filter
field opens a `Dialog` listing every `TOKEN_HELP` entry with its examples
plus the date/size grammar: the stopgap discoverability surface until the
filter grows chips. Same static table; `--filter-help` captures it
headlessly.

A token-only query (`mod:week` with no text) routes subtree search to the
walker even when Spotlight is preferred: Spotlight needs a query string;
the walker runs a pure metadata scan.

## The three tiers

| Tier | What | Engine | Scope |
|---|---|---|---|
| 0 | Filter the current listing | in-memory | current folder, loaded rows |
| 1 | Recursive subtree walk | own walker (`ferail-fs-native`) | a chosen folder and below |
| 2 | Global / indexed | OS index, falling back to Tier 1 | a volume or everything |

Tiers compose, not replace. The same input box drives all three; the UI is
**engine-agnostic** because every engine streams the same `SearchFact` results
into the same results model. Where the user starts a search and how broad it is
selects the engine; an empty subtree result can offer to escalate to Tier 2.

## Rely on the OS index where it exists

The strongest single lesson from surveying the field: **do not build and
maintain a whole-disk index if the OS already keeps one fresh.** Windows'
"Everything" had to reverse-engineer the NTFS MFT only because the OS gave it
nothing usable; macOS already ships Spotlight, kept live by FSEvents.

- **macOS: Spotlight.** Query `MDQuery` (or `mdfind` as a first spike) for
  name *and* content matches against an index the OS maintains for us: instant
  whole-disk search at ~zero ongoing CPU, nothing for us to build, store, or
  keep warm. **This is the default Tier 2 engine and the one to rely on
  whenever it is available.** Fall back to a Tier 1 live walk only for paths
  Spotlight excludes (some external/network volumes, `mdutil`-disabled trees).
- **Windows: NTFS MFT + USN journal.** Read the Master File Table directly for
  an instant whole-volume name index, and tail the USN change journal to keep
  it live. This is how "Everything" achieves its speed. It typically requires
  **elevation / admin rights** (raw volume handle access). The plan: ship our
  **own recursive walker as the always-available fallback** (no privileges
  needed), offer the MFT engine when we can elevate, and **let the user choose
  which engine to use** rather than forcing elevation. The same MFT reader is
  also the fast path for Windows disk usage: build it once, use it for both.
  See [windows-port.md](windows-port.md).
- **Linux: Tracker / Baloo, else own walk.** Query the desktop index via
  D-Bus when present; otherwise the Tier 1 walker. See
  [linux-port.md](linux-port.md).

### Engine selection is the user's call

Search engines are pluggable and the user decides the policy, per the same
philosophy we apply to Windows elevation: an OS index is faster but may be
incomplete, stale, scope-restricted, or (Windows) privilege-gated. Surface the
trade-off and default sensibly (Spotlight on macOS, own-walk fallback
everywhere), but let the user force the built-in walker if they distrust the
index or it is disabled for the tree they care about.

```
enum SearchEngine {
    SubtreeWalk,        // Tier 1, always available, no privileges
    Spotlight,          // macOS Tier 2
    NtfsMft,            // Windows Tier 2 (needs elevation)
    DesktopIndex,       // Linux Tracker/Baloo via D-Bus
}
```

A platform exposes the engines it supports; `SubtreeWalk` is always one of
them. The GPUI layer picks a default and honors a user override.

## Tier 1 - recursive subtree walk (build first)

A pure-function walker in `ferail-fs-native`, modeled directly on
[`scan_disk_usage`](../../crates/ferail-fs-native/src/disk_usage_scanner.rs):
same DFS stack, same `batch_size`, same `AtomicBool` cancel checked between
dirents and at level boundaries, same throttled `on_progress`, same
host-owns-the-thread contract.

```rust
pub struct SearchQuery {
    pub needle: String,       // case-insensitive substring; glob/regex later
    pub match_path: bool,     // name only vs. full relative path
    pub include_hidden: bool,
}

pub enum SearchFact {
    Match { node: NodeId, path: PathBuf, name: String, is_dir: bool, size: u64 },
    DirScanned,               // progress accounting
}

impl NativeFs {
    pub fn search_subtree(
        &self,
        root: &Path,
        query: &SearchQuery,
        batch_size: usize,
        cancel: &AtomicBool,
        descend_packages: bool,
        on_batch: impl FnMut(Vec<SearchFact>),
        on_progress: impl FnMut(SearchStats),
    ) -> Option<EnumerationError>;
}
```

Mac-safe behavior (mirrors the disk-usage walker):

- **Skip dataless / cloud placeholders**: test the dataless flag and
  `is_icloud_path`; never trigger an iCloud download just to match a name.
- **Bundles opaque by default** (`descend_packages = false`): `*.app`,
  `*.bundle`, `*.framework` match as units, not exploded into inner files.
- **Symlinks** walked via `symlink_metadata`, never followed: cycle-safe.
- Per-directory permission errors are absorbed; the scan reports
  partial-but-complete.

## GPUI integration (shared by all tiers)

Search is a tab-local [Tool Result Surface](TOOL_RESULTS.md). Pressing Return
in the filter launches `Shell::start_subtree_search`, which:

- stores `ToolResultSurface::Search` on the active tab;
- bumps the tab load generation and cancels any superseded tab worker;
- registers `TaskKind::Search` via `begin_with_cancel`;
- spawns the selected engine on `cx.background_executor()`;
- streams `LoadBatch` rows into the existing table delegate.

The table path is the same one directory enumeration uses, so selection, sort,
preview, context menus, and file operations keep working. Tier 2 engines such
as Spotlight emit paths that are converted to the same table rows; platform
shell crates return data only and paint no UI.

## Build order

1. **Tier 1 walker + tab result surface.** Shipped; self-contained, exercises
   the shared plumbing, immediately useful.
2. **Tier 2 Spotlight.** Spike with `mdfind` to validate UX, then decide
   whether the `MDQuery` FFI (live/async batching) is worth it over the CLI.
   Route by scope, fall back to Tier 1 where Spotlight is blind.
3. **Windows MFT / Linux desktop index.** Land with their respective ports;
   reuse the `SearchEngine` selection and the MFT reader for disk usage too.

## Verification

- `cargo check -p ferail-fs-native -p ferail-gpui`; `cargo test` for the
  walker (match correctness, cancellation, stale-generation drop).
- One screenshot of streaming results into the list.

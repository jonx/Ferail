# Manual test checklist - 2026-07-02 stability pass + DU features

Point-in-time QA sheet for everything added or fixed in the
2026-07-02 batch (commits `d7d008e` … `c0f1de2`). Check items off as
you verify them; when the sheet is fully green it can be deleted:
regressions worth keeping live in each feature's doc under
[docs/features](../features/README.md).

Build: `cargo run --release --bin ferail-gpui`

## Reported bugs

- [ ] **Settings resize flicker**: open Settings (Cmd+,), drag the
  window edge continuously: toggles/dropdowns stay pinned right with no
  left↔right jumping; wrapped descriptions reflow smoothly.
- [ ] **Dock black area**: toolbar → Dock Right: the window keeps its
  exact size and vertical position (pure slide, no black band). Slam
  the cursor into the edge to reveal; undock restores the original
  frame.
- [ ] **DU menu collision**: Disk Usage → right-click a square:
  exactly one menu opens. Right-click empty background: the view menu
  (Zoom Out + view HTML export).
- [ ] **DU selection survives right-click**: Cmd-click 3 squares,
  right-click one of them: all 3 stay selected, footer reads
  "3 selected · …", and the menu acts on all 3.
- [ ] **DU selection visibility**: selected squares draw a 2px
  near-black border, readable on every tile color.

## New features

- [ ] **DU multi-select**: plain click selects one; Cmd-click toggles;
  same in the "Largest files" panel; footer shows multi-select totals.
- [ ] **DU context menu verbs**: Open, Reveal in Finder, Get Info,
  Copy (then paste the files in Finder), Copy Path(s), Zoom In/Out,
  Move to Trash (toast + automatic re-scan + shell tab reload).
- [ ] **HTML export (folder)**: right-click a folder square →
  Export as HTML ▸ *Copy This Folder*: paste into an HTML-capable
  editor. *Save This Folder…*: file lands in `~/Downloads` and is
  revealed.
- [ ] **HTML export (whole view)**: background right-click →
  Copy/Save View as HTML. Open the saved file: treemap matches the
  window (colors, labels, legend, hover tooltips), self-contained
  (no network, no scripts).
- [ ] **Column order + widths persist**: reorder and resize file-table
  columns, quit, relaunch: layout restored; new tabs inherit it.

## Stability / correctness spot-checks

- [ ] **Status-bar free space**: sensible value after navigation;
  updates on volume mount/unmount; no per-frame recomputation
  (idle CPU ~0%).
- [ ] **Cmd+Z on a big paste**: paste a large folder, Cmd+Z: UI stays
  responsive (task row visible), completion toast. Then: move a file
  out, create a same-named file at the origin, Cmd+Z → refuses to
  overwrite.
- [ ] **Rapid navigation**: bounce between two large folders quickly:
  Format/quarantine badges never show the other folder's data.
- [ ] **KeepBoth naming**: paste `a.tar.gz` over itself → copy is
  `a.tar 2.gz` (was `a.gz`).
- [ ] **Duplicate panel**: select rows in a folder, run Find
  Duplicates: nothing arrives pre-marked for trash.
- [ ] **Confusable names**: `Договор.pdf` shows no
  deceptive-character highlight; mixed `invοice.pdf` (Greek ο) still
  does.
- [ ] **Disk Usage counting**: scan `/`: external volumes don't
  inflate the total; a hardlink-heavy dir (Homebrew Cellar) counts
  once; an `.app` bundle shows its real size in **Allocated** mode.
- [ ] **Tags on a big selection**: toggle a tag color on hundreds of
  files: instant UI, dots update when the worker finishes.
- [ ] **Open With multi-select**: several files → Open With → app:
  one app bounce, no freeze.
- [ ] **Idle CPU**: leave the app on a short folder: ~0% (stripe-
  filler and resizable repaint loops fixed).
- [ ] **Settings safety**: settings survive relaunch (atomic
  write-behind); favorites survive opening a second app instance.
- [ ] **Dead-mount resilience** (if you have a network share):
  disconnect it, then: click around, Cmd+C files, toggle favorites,
  open Settings. Nothing freezes.

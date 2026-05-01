# Feraille UX — Overview

## What this app is

A desktop file explorer for Windows. The differentiator is **speed at scale**: a folder with a million files opens, scrolls, sorts, and searches without ever feeling laggy. Everything else — UI polish, shell integration, tabs — is table-stakes that has to be there but is not the *reason* to use it.

## What it is *not*

- A file manager with workflow automation (no batch-rename DSL, no scripting).
- A cloud-sync client.
- A dual-pane Norton-style commander. (Tabs, yes; orthopanes, no.)
- A Mac/Linux app. macOS is dev-mode only; Linux is out of scope.

## Mental model the user brings

Users come from one of three places:

1. **Windows Explorer.** Their muscle memory: F2 rename, Delete sends to Recycle, Ctrl+L for address, Alt+arrows for nav, right-click for context menu, drag to copy/move with modifier keys. **Match Explorer's keyboard map exactly** unless we have a strong reason not to. Surprise here is bad surprise.

2. **VS Code / IDE file pane.** Quick switcher (Ctrl+P style fuzzy nav), keyboard-first multi-select, type-ahead. **We adopt these on top of Explorer's map**, never replacing.

3. **Total Commander / Directory Opus power users.** Want column control, custom sorting, multi-select that survives scrolling, drag with confidence about copy-vs-move. **We give them keyboard shortcuts and density**, not skinning.

If a feature pleases group 3 at the cost of confusing group 1, group 1 wins. There are vastly more of them.

## The five primary tasks

Optimize for these. Everything else is secondary.

1. **Navigate.** Get to a folder fast. (Tree click, breadcrumb click, Ctrl+L typed path, tabs, back/forward.)
2. **Find.** Locate a file by name within a folder, by attribute, or by full-text. (Type-ahead, search box, filter chips.)
3. **Inspect.** See what's there — size, modified date, type — without opening. (Columns, sort, optional preview pane.)
4. **Manipulate.** Copy, move, rename, delete. (Drag, keyboard, context menu.)
5. **Open.** Hand the file to another app. (Double-click, Enter, "Open with…".)

## Performance is UX

The numbers in [05-performance.md](05-performance.md) are not engineering aspirations — they are user experience. A 33 ms hitch when scrolling a 100k-file folder is the difference between "fast tool" and "broken tool." Latency budgets in this spec are *part of the design*, not implementation detail.

## Information density

Default to **dense**. Row height 28 DIPs (the spec default), text size 13, no row stripes, no excessive padding. Provide a "comfortable" mode (row 32, text 14) for users who want it, but don't ship as default. Files App's default density is too generous for power users; Explorer's is right.

## The opinion that drives every decision

> Every interaction has a fastest possible reaction time. Treat that as the spec, work backwards. If a design choice — animation, modal, confirmation, "are you sure?" — makes the median path slower than the fastest possible path, it is wrong.

This is the answer when a tradeoff is hard.

---

## Sub-specs in this folder

- [01-navigation.md](01-navigation.md) — moving between folders, tabs, history.
- [02-selection.md](02-selection.md) — selection model and multi-select gestures.
- [03-keyboard.md](03-keyboard.md) — full keyboard map.
- [04-drag-drop.md](04-drag-drop.md) — drag-drop affordances and rules.
- [05-performance.md](05-performance.md) — latency, frame, and memory budgets.
- [06-error-and-empty-states.md](06-error-and-empty-states.md) — what to show when nothing is there, or it failed.

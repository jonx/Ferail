# Keyboard Map

The default. All bindings remappable in v2; in v1 they're code constants.

Where a binding has a Windows convention, **we match it**. The "Feraille" column lists the actual binding; the "Source" column says where the convention comes from. Disagreements between sources are resolved by: Explorer > VS Code > nothing.

## Global

| Action | Feraille | Source |
|---|---|---|
| Focus address bar | `Ctrl+L`, `F4` | Explorer |
| Open new tab (at active folder) | `Ctrl+T` | browsers, VS Code |
| Close tab | `Ctrl+W` | browsers, VS Code |
| Reopen last closed tab | `Ctrl+Shift+T` | browsers |
| Cycle tabs | `Ctrl+Tab` / `Ctrl+Shift+Tab` | browsers |
| Jump to tab N | `Ctrl+1` … `Ctrl+9` | browsers |
| Open in new window | `Ctrl+N` | Explorer |
| Quit | `Ctrl+Q` | apps |
| Find in folder | `Ctrl+F` | universal |
| Toggle navigation pane | `Ctrl+B` | VS Code |
| Toggle preview pane | `Ctrl+P` | (we steal the slot; Explorer uses Alt+P) |
| Refresh | `F5` | Explorer |
| Go to parent | `Alt+Up`, `Backspace` | Explorer |
| Back / forward | `Alt+Left` / `Alt+Right` | Explorer, browsers |
| Open command palette | `Ctrl+Shift+P` | VS Code |
| Quick switcher (recent folders) | `Ctrl+P` *(taken)* — using `Ctrl+E` instead | (ours) |

> **Conflict noted:** `Ctrl+P` is "preview pane" here, not "quick file switcher." Reasoning: in a *file explorer*, the quick switcher would mean "jump to a folder," but we already have the address bar with completion (`Ctrl+L`) and the recent-folder palette (`Ctrl+E`). Preview-pane toggle is the higher-traffic action. If user research shows otherwise, this is the first binding to revisit.

## File pane (when focused)

| Action | Feraille | Source |
|---|---|---|
| Open / activate | `Enter` | Explorer |
| Open with… | `Shift+Enter` | (ours) |
| Rename | `F2` | Explorer |
| Delete (recycle) | `Delete` | Explorer |
| Delete (permanent) | `Shift+Delete` | Explorer |
| Copy | `Ctrl+C` | universal |
| Cut | `Ctrl+X` | universal |
| Paste | `Ctrl+V` | universal |
| Copy path | `Ctrl+Shift+C` | VS Code |
| Properties | `Alt+Enter` | Explorer |
| Select all | `Ctrl+A` | universal |
| Invert selection | `Ctrl+Shift+I` | (ours; Explorer uses ribbon) |
| Multi-select mode toggle | `Ctrl+Shift+M` | (ours) |
| New folder | `Ctrl+Shift+N` | Explorer |
| New file | (context menu only) | Explorer |
| Show hidden files | `Ctrl+H` | (Linux convention; Explorer uses ribbon) |
| Cycle view density | `Ctrl+Shift+D` | (ours) |
| Sort by name / size / date / type | `Ctrl+Shift+1..4` | (ours) |

## Tree (when focused)

| Action | Feraille | Source |
|---|---|---|
| Expand / collapse | `→` / `←` | universal |
| Expand all children | `*` (numpad), `Shift+→` | OS trees |
| Move down / up | `↓` / `↑` | universal |
| Activate (navigate file pane to selected) | `Enter` | (ours) |
| Find in tree | `Ctrl+Shift+F` | (ours) |

## Address bar (edit mode)

| Action | Feraille |
|---|---|
| Submit | `Enter` |
| Cancel | `Esc` |
| Tab-complete segment | `Tab` |
| Cycle completions | repeated `Tab` |
| Backward-cycle | `Shift+Tab` |
| Move by word | `Ctrl+←` / `Ctrl+→` |

## Search box

| Action | Feraille |
|---|---|
| Submit search | `Enter` |
| Clear and exit | `Esc` |
| Convert "key:value" segment to chip | `Tab`, space |
| Delete rightmost chip | `Backspace` at start of input |
| Move focus to results | `↓` |

## Context menu

When open:

| Action | Feraille |
|---|---|
| Move | `↑` / `↓` |
| Submenu | `→` |
| Close submenu | `←` |
| Activate | `Enter`, `Space` |
| Close menu | `Esc` |
| Activate underlined letter | the letter |

## Type-ahead (file pane)

Pressing a printable character that isn't a bound shortcut moves cursor to the first row whose display name starts with the typed prefix. Continuing to type extends the prefix. 800 ms of idle resets the buffer.

If multiple rows match, repeated presses of the *same* key cycle through them (Explorer behavior).

## Modifiers (drag)

| Modifier held during drop | Effect |
|---|---|
| (none, same volume) | Move |
| (none, different volume) | Copy |
| `Ctrl` | Force copy |
| `Shift` | Force move |
| `Alt` | Create shortcut |
| `Ctrl+Shift` | Create shortcut (VS Code-style alias) |

The cursor changes to reflect the resolved action: arrow with "+" (copy), arrow alone (move), arrow with shortcut glyph (link). Match Windows shell cursor exactly.

## Discoverability

Every menu item shows its keybinding right-aligned, in `text.xs` `fg.tertiary`. Tooltips on toolbar buttons show the binding too. Users who don't read documentation should still discover their next 5 shortcuts within an hour of use.

## Accessibility

- All interactive controls Tab-reachable.
- Tab order matches **visual layout**, not creation order.
- `F6` cycles focus between major regions (tabs → toolbar → tree → file pane → status bar). Same as Explorer.
- Screen-reader announcements: row count on enumeration complete, selection count on change, current folder on navigation.

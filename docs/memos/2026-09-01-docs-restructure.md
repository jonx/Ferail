# Documentation restructure, 2026-09-01

← [Documentation map](../README.md) · [Rules](../DOCUMENTATION.md) ·
[Status](../STATUS.md)

What the documentation tree looked like before this pass, what moved, and what
was deliberately left alone.

## What the measurements said

| Symptom | Measure |
| --- | --- |
| Status stated in five places | the README table, a 43-row table in the feature index audited 2026-06-20, six per-file `Status:` lines, TODO prose, and the Windows plan's checkboxes |
| A 2.246-line handover that was mostly a session log | 151 journey words, 36 dated sections, out of order |
| No table of contents anywhere | 0 of the 20 documents over 300 lines had one |
| Documents nobody linked to | 4, plus 8 feature notes missing from the feature index |
| Nothing enforced any of it | no rules document, no checker |

## What moved

- **[docs/STATUS.md](../STATUS.md)** is new and is now the only file that
  states status. The feature index's status table moved into it, the README
  keeps four platform rows and a link, and the per-file `Status:` lines are
  gone.
- **[docs/README.md](../README.md)** is new: the map of every document.
- **[docs/DOCUMENTATION.md](../DOCUMENTATION.md)** is new: the rules, with
  [CLAUDE.md](../../CLAUDE.md) repeating the four hardest ones.
- **[scripts/check-docs.py](../../scripts/check-docs.py)** is new and runs in
  CI on any Markdown change. It checks links, anchors, generated tables of
  contents, `WIN-nnn` identifiers, feature-note navigation lines and index
  membership. It found nine broken links on its first run.
- **[windows-sessions-2026-08.md](windows-sessions-2026-08.md)** holds the 36
  dated entries extracted from the Windows handover, which went from 2.246
  lines to 219 of purpose, queue, invariants and handback template.
- The GPUI migration checkpoint and the July manual test checklist moved into
  this directory.
- Every feature note gained a navigation line; 45 documents gained a generated
  table of contents; backticked repository paths became links.

## What was corrected, not just moved

The context-menu note claimed a Services / Quick Actions submenu populated
through a `ServicesAnchor` responder, and listed a `Share…` entry. Neither
exists in the GPUI app: both were true of the Cocoa predecessor. The claim is
gone and the gap is recorded in [STATUS.md](../STATUS.md). The feature tour
understated magic recognition at "~67 signatures" against 111 real format
types, and its platform-status paragraph became a link.

## What was left alone

The technical bodies of the design notes. They describe systems this pass did
not verify line by line, so they got structure (navigation, tables of
contents, working links) and the rule to convert a section when it is next
touched. Rewriting 20.000 lines of unverified prose is how documentation
starts contradicting code.

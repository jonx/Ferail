# Documentation Rules

← [Documentation map](README.md) · [Status](STATUS.md) ·
[Operating manual](../CLAUDE.md)

How this repository's documentation is written and maintained. These rules
bind every contributor, human or agent, and `python3 scripts/check-docs.py`
enforces the mechanical ones. The test for any sentence is: **would it still
be true and useful a year from now?** If not, it belongs in the journal or
nowhere.

<!-- toc depth=2 -->

- [The one rule](#the-one-rule)
- [Four kinds of document, one home per fact](#four-kinds-of-document-one-home-per-fact)
- [Per-document rules](#per-document-rules)
- [Tables of contents, links and blocks](#tables-of-contents-links-and-blocks)
- [Style](#style)
- [Before you commit](#before-you-commit)

<!-- /toc -->

## The one rule

Every document except the journal describes the **finished product**: what the
app is, how it is built, what a procedure does, what a release contains. It
does not tell the story of how it got there.

| Write | Do not write |
| --- | --- |
| "The Trash menu offers Put Back." | "The Trash menu now offers Put Back." |
| "Ferail can put back only what it trashed itself." | "The first attempt read Finder's store and failed, so we..." |
| "Release: 0.7.7." | a version number repeated in six design notes |
| one caveat per document | the same caveat in every paragraph |

Words that almost always mark journey text: *now*, *new*, *still*, *remains*,
*no longer*, *the latest*, *today*, *currently*, *after the fix*, *was
corrected*. Delete them, or move the sentence into history: a decision belongs
in [NOTES.md](../NOTES.md), a session or a checkpoint in
[docs/memos/](README.md#history).

Those two are the exception. They narrate in the past tense and link to the
durable documents instead of restating them.

## Four kinds of document, one home per fact

| Kind | Answers | Home | Never contains |
| --- | --- | --- | --- |
| **State** | where are we? | [docs/STATUS.md](STATUS.md) | design, procedure, narrative |
| **Design** | how is it built, and why is it bounded that way? | [docs/ARCHITECTURE.md](ARCHITECTURE.md), [docs/features/](features/README.md) | status, dates, versions, test transcripts |
| **Procedure** | what do I run, and what does it prove? | [GETTING_STARTED.md](../GETTING_STARTED.md), [docs/testing/](testing/WINDOWS_RELIABILITY_TEST_PLAN.md), [docs/REPORTING_BUGS.md](REPORTING_BUGS.md) | design rationale, status |
| **History** | what was tried, decided, delivered? | [NOTES.md](../NOTES.md), [docs/memos/](README.md#history), [CHANGELOG.md](../CHANGELOG.md) | anything another document owns |

Anything point-in-time (a handover, a migration checkpoint, a session log, a
review memo) goes under [docs/memos/](README.md#history) with a date in its
name. A memo is never cited as a current statement about the app.

A fact is written once. Every other mention is a link. If the same fact
appears in two documents, delete one and link to the other.

## Per-document rules

### Root `README.md`

Product-facing: what Ferail is, what makes it different, how to get it, how to
build it, the crate map, the documentation map, the licence. Its status
section is the four platform rows and a link to [STATUS.md](STATUS.md), never
a paragraph.

### `docs/STATUS.md`

The only file that states status.

- "Where we are" at the top: at most six rows.
- One row per platform, one row per feature note.
- A status cell is one line: the state, what ships, what is open.
- Versions, pinned revisions and build stamps appear here and nowhere else.
- Every row links its design note.

### `docs/features/*.md`

1. `# Title`
2. the navigation line:

   ```markdown
   ← [Feature notes](README.md) · [Status](../STATUS.md) ·
   [Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)
   ```

3. the design: contract, boundaries, invariants, what is deliberately out of
   scope.

No `## Status` section, no dates, no version numbers, no "currently", no list
of which tests passed. Every note is listed in
[docs/features/README.md](features/README.md); a note nobody links to is a bug.

### `TODO.md`

What is not done, by area and priority. When an item ships, delete it and let
[CHANGELOG.md](../CHANGELOG.md) and git history carry the record.

### `CHANGELOG.md` and `RELEASE_NOTES.md`

The changelog answers "what would I notice as a user?", one bullet per change,
newest first; the full rules are in
[CLAUDE.md](../CLAUDE.md#changelog). The release notes are not the changelog:
they are the short case for updating, written for someone who has not read a
single commit. If a release note runs past one screen, it is describing the
work instead of the result.

### `NOTES.md`

The decision log, one block per spec or feature: what was decided, why, what
was deferred, what would change our mind. It is the home for "we tried X and
it cost Y", and the only place a superseded approach is described.

## Tables of contents, links and blocks

- A document over about 150 lines carries a `<!-- toc -->` / `<!-- /toc -->`
  block after its introduction, generated by
  `python3 scripts/check-docs.py --write-toc`. Never hand-write one, and never
  put one in a short file.
- Link to headings, never to line numbers. The checker verifies anchors.
- A path that names a file in this repository is a link, not backticked text.
- Every `WIN-nnn` identifier must resolve to a heading in
  [WINDOWS_COMPATIBILITY_PLAN.md](features/WINDOWS_COMPATIBILITY_PLAN.md).

## Style

- English, present tense, imperative mood for procedures.
- Short sentences. A table beats a paragraph when the content is a list of
  facts.
- **No em dashes**, anywhere. Use a comma, a colon, or a full stop.
- Identifiers in backticks; counts grouped with `.` every three digits.
- One caveat per document, stated once.
- No AI or assistant attribution.

## Before you commit

- [ ] status changed → one line in [STATUS.md](STATUS.md), nowhere else
- [ ] a version or pinned revision changed → [STATUS.md](STATUS.md) only
- [ ] new feature note → the note, its navigation line, a row in
      [features/README.md](features/README.md) and one in
      [STATUS.md](STATUS.md)
- [ ] an icon changed → [ICONS.md](features/ICONS.md)
- [ ] user-visible text changed → the language packs
- [ ] a user could notice the change → [CHANGELOG.md](../CHANGELOG.md)
- [ ] a decision worth keeping → [NOTES.md](../NOTES.md); a session or
      checkpoint → [docs/memos/](README.md#history)
- [ ] `python3 scripts/check-docs.py` passes

When you edit a document that still contains journey text, convert the section
you touched. Do not append a new paragraph after the old one.

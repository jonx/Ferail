# Ferail Documentation

← [Project README](../README.md) · [Getting started](../GETTING_STARTED.md)

Every document in the repository, and what it is for. If a fact is not where
this map says it lives, the map is the thing to fix.

## Start here

| Document | Purpose |
| --- | --- |
| [FEATURES.md](FEATURES.md) | The feature tour: what the app does, with a picture per feature. |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Source of truth for crate boundaries, the prime directive, the data model, and scheduling. |
| [STATUS.md](STATUS.md) | Where the project is: release, platforms, per-feature state. The only file that states status. |
| [../TODO.md](../TODO.md) | The single list of unfinished work, by area and priority. |
| [../CHANGELOG.md](../CHANGELOG.md) | What changed, for users, newest first. |
| [../RELEASE_NOTES.md](../RELEASE_NOTES.md) | The current release, written for someone deciding whether to update. |

## Design notes

| Document | Purpose |
| --- | --- |
| [features/](features/README.md) | One note per feature: what it is, how it is built, why it is bounded that way. Indexed by topic. |
| [GPUI-UPSTREAM.md](GPUI-UPSTREAM.md) | The relationship with the gpui / gpui-component upstreams: pinning, local deltas, and how to move the pins. |
| [DOCUMENTATION.md](DOCUMENTATION.md) | The rules this tree follows, and the checker that keeps it honest. |

## Procedures

| Document | Purpose |
| --- | --- |
| [../GETTING_STARTED.md](../GETTING_STARTED.md) | Zero to running, per platform, with the permission caveats. |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | How a change gets in. |
| [../CLAUDE.md](../CLAUDE.md) | The operating manual for AI and human edits. |
| [REPORTING_BUGS.md](REPORTING_BUGS.md) | Exactly which crash and freeze files to attach to a report. |
| [testing/WINDOWS_RELIABILITY_TEST_PLAN.md](testing/WINDOWS_RELIABILITY_TEST_PLAN.md) | The Windows environments, corpora, measurements and sign-off procedure, including the four-million-row regression gate. |
| [testing/WINDOWS_HANDOVER.md](testing/WINDOWS_HANDOVER.md) | How to resume the Windows campaign on a real Windows machine: resume point, queue, invariants, handback template. |

## Policy

| Document | Purpose |
| --- | --- |
| [../PRIVACY.md](../PRIVACY.md) | Local storage, update requests, diagnostics, permissions, deletion. |
| [../SECURITY.md](../SECURITY.md) | How to report a vulnerability. |
| [../CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md) | Expected conduct. |
| [../THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) | Components incorporated into a built binary, and their licences. |
| [../CHANGELOG-DEPS.md](../CHANGELOG-DEPS.md) | Dependency pin movements, kept out of the user changelog. |

## History

Point-in-time documents. Nothing here is a current statement about the app.

| Document | Purpose |
| --- | --- |
| [../NOTES.md](../NOTES.md) | The engineering journal: what was attempted, what it cost, what it taught. |
| [memos/2026-09-01-docs-restructure.md](memos/2026-09-01-docs-restructure.md) | What this documentation tree looked like before it was reorganized, and what moved. |
| [memos/windows-sessions-2026-08.md](memos/windows-sessions-2026-08.md) | The Windows session log, newest first. |
| [memos/gpui-migration-2026-08.md](memos/gpui-migration-2026-08.md) | The August 2026 move to the current gpui-component / Zed pair. |
| [memos/2026-07-02-manual-test-checklist.md](memos/2026-07-02-manual-test-checklist.md) | A manual test pass from July 2026. |

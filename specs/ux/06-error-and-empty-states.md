# Error And Empty States

Error states must preserve control. The user should always be able to navigate
away, retry, close a panel, or keep working in another tab.

## Empty Folder

- Title: "This folder is empty"
- Body: show the path subtly.
- Actions: New Folder, Paste if clipboard has file URLs.

## Empty Filter

- Title: "No matches"
- Body: show the current filter.
- Actions: Clear Filter.

## Loading

If a task exceeds the instant path:

- Keep existing content if present.
- Show partial results as they stream.
- Show progress in the status bar.
- Do not cover the file list with a blocking spinner.

## Permission Denied

- Title: "You do not have access to this folder"
- Actions: Retry, Reveal Parent, Open in Terminal.
- Keep history intact.

## Missing Folder Or Ejected Volume

- Title: "This folder is no longer available"
- Actions: Go to Parent, Retry.
- Do not auto-navigate unless the user chooses an action.

## Network Or Cloud Delay

- Show a nonblocking loading/progress state.
- Allow navigation away.
- Drop stale results if they arrive later.
- Do not fault cloud files into local storage unless the user started a content
  operation that clearly needs it.

## Partial Failure

If some children enumerate and some fail:

- Show the successful items.
- Put a concise warning in status/toast.
- Details can open a panel later.

## Crash/Worker Failure

- Log enough context.
- Keep app state usable if possible.
- Surface a toast/status message.
- Never spin or retry forever on the UI thread.

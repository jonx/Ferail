# Mouse Predictor

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Mouse prediction is a future prewarm feature from Ferail-Win32: use recent pointer
motion to guess the next likely row or folder and prepare cheap state ahead of
the click.

## What is built

Nothing. This note is a design; the work is tracked in
[TODO.md](../../TODO.md).

## What It May Do

- Track the last few pointer samples.
- Predict the likely tree/list row.
- Warm pure app-side state for that row.
- Ask background workers to prefetch low-priority metadata after debounce.
- Combine with Ant Trail heat.

## What It Must Not Do

- Block pointer move handling.
- Perform I/O from `CursorMoved`.
- Change selection before the user acts.
- Trigger visible UI that feels jumpy or spooky.

## Useful Predictions

- Likely folder expansion.
- Likely context menu target.
- Likely preview target.
- Likely scroll direction.

## Remaining Work

- Pure predictor module.
- Debug overlay.
- Integration with task scheduler.
- Ant Trail blend.
- Performance tests for pointer path.

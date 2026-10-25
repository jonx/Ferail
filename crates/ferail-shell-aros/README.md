# AROS shell integration

File Copy, Cut and Paste use native `clipboard.device` unit 0 via iffparse's
`OpenClipboard`/`CloseClipboard`. This is an application file-reference format,
not a claim that Workbench or other file managers understand the file list.
Normal text applications can read the `CHRS` text. Foreign text, including text
that resembles paths, is never interpreted as a list of files.

The whole stream is a big-endian IFF `FORM FTXT`. `CHRS` holds the raw Latin-1
DOS paths separated by LF, for display only. The typed `FSAF` chunk holds:

| Field | Encoding |
| --- | --- |
| Version | u16 BE, currently 1 |
| Operation | u8: 0 Copy, 1 Move |
| Reserved | u8, zero |
| Entry count | u32 BE |
| Each entry | u8 directory flag (0/1), three zero bytes, u32 BE byte length, raw DOS path bytes |

Paths retain native bytes, including accented names and newlines. They must
have a nonempty volume/assign prefix followed by `:`, with no NUL or second
colon. `SYS:` is valid. No filesystem lookup occurs while encoding. IFF chunks
have their normal even-byte padding. Readers reject duplicate typed chunks,
unknown versions/operations, invalid flags, bad lengths and trailing entry data.
Limits are 2 MiB total, 20,000 entries, 32,768 bytes per path and 128 chunks.
The list describes references, never file contents. A Move indication is an
intent; clipboard publication does not move a file.

UI handlers reserve an ordered ticket, capture their selection/destination and
call `begin_write`/`begin_read`. A dedicated worker performs device I/O and
parsing, and returns an asynchronous result. At most 64 outstanding tickets
are accepted. Canceled undispatched tickets are skipped; result sequence numbers
prevent older UI completions replacing a newer Cut marker. Synchronous
compatibility functions require a worker too; debug guards reject UI I/O.
The main task owns a `FileClipboardSession` guard before CLI/UI dispatch, closes
admission and joins the retained worker before returning. Explicit CLI exits
call the same idempotent shutdown. Queued requests resolve in error without
performing I/O; an already executing request must finish before the join returns.
The AROS device implements `AbortIO` as "Nothing to abort" and a foreign
`CBD_POST` can block `CMD_READ` until its owner publishes data. Cancellation of
such active I/O is unavailable: shutdown retains the loaded command and waits,
rather than abandoning its task or claiming a timeout/success. Only requests
that have not started can be canceled. The caller must own and join any thread
it uses to call a synchronous compatibility function.
A failed Copy/Cut sets an error barrier: a following Paste refuses to act on
older clipboard references until a new successful Copy/Cut. This state is an
error, not a simulated empty or replacement clipboard. Older write errors remain
visible even when a newer UI callback has already updated the Cut marker.
No process-local payload is used as a fallback. Read snapshots are released
with an end read; writers always finish an acquired writer with `CMD_UPDATE`,
including short/error writes, before closing the handle. A failed write may
leave an invalid/partial stream and is never reported as successful.

Tests:

```sh
cargo test -p ferail-shell-aros
clang -std=c11 -Wall -Wextra -Werror -fsanitize=address,undefined \
  -Icrates/ferail-shell-aros/tests/clipboard-device/include \
  crates/ferail-shell-aros/tests/clipboard-device/test.c -o /tmp/clipboard-device-test
/tmp/clipboard-device-test
```

The C tests execute the production transport against an instrumented device
boundary; they test cleanup and error behavior, not native device execution.
For native qualification, build the `aros-file-clipboard` example with the
integrated AROS SDK and run it in an exclusively owned test session:

```text
FileClipboardProbe --exclusive-primary-clipboard
```

It temporarily replaces clipboard unit 0, checks raw names, Copy/Move, worker
I/O, foreign text and malformed streams, and restores/verifies its original
raw bytes. Do not edit the clipboard concurrently. It refuses to write if the
original clipboard cannot be read within the limits. The source-only SDK/link
qualification does not establish runtime or GUI behavior; those require the
native run and a rebuilt application.

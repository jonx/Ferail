/* SPDX-License-Identifier: MIT */
/* Copyright (c) 2026 John Knipper */
#include <proto/exec.h>
#include <proto/iffparse.h>
#include <libraries/iffparse.h>
#include <devices/clipboard.h>

#define FSA_CLIP_MAX (2UL * 1024UL * 1024UL)

/* Whole IFF stream, not a process-local clipboard. The positive clip ID keeps
 * a read bound to one version until the final past-end CMD_READ releases it. */
int fsa_clip_read_blob(unsigned char *buffer, unsigned int capacity,
                       unsigned int *length)
{
    struct ClipboardHandle *clip;
    struct IOClipReq *req;
    unsigned int total = 0, rounds = 0;
    int rc = 0;
    if (!buffer || !length || capacity != FSA_CLIP_MAX + 1) return -1;
    *length = 0;
    clip = OpenClipboard(PRIMARY_CLIP);
    if (!clip) return -2;
    req = &clip->cbh_Req;
    req->io_ClipID = 0;
    req->io_Offset = 0;
    do {
        req->io_Command = CMD_READ;
        req->io_Data = (STRPTR)(buffer + total);
        req->io_Length = capacity - total;
        if (++rounds > 256 || DoIO((struct IORequest *)req) != 0) { rc = -3; break; }
        if (req->io_Actual > capacity - total) { rc = -3; break; }
        total += req->io_Actual;
        if (total > FSA_CLIP_MAX) { rc = -4; break; }
    } while (req->io_Actual != 0 && req->io_ClipID > 0);
    if (req->io_ClipID > 0) {
        /* AROS readCb tests offset >= clipSize before touching data. This
         * bounded past-end read also ends oversized/truncated/error reads,
         * without draining arbitrary amounts of foreign clipboard data. */
        req->io_Command = CMD_READ;
        req->io_Data = NULL;
        req->io_Offset = ~(ULONG)0;
        req->io_Length = 1;
        if (DoIO((struct IORequest *)req) != 0 || req->io_ClipID > 0) rc = -5;
    }
    CloseClipboard(clip);
    if (!rc) *length = total;
    return rc;
}

int fsa_clip_write_blob(const unsigned char *buffer, unsigned int length)
{
    struct ClipboardHandle *clip;
    struct IOClipReq *req;
    int rc = 0;
    if (!buffer || length > FSA_CLIP_MAX) return -1;
    clip = OpenClipboard(PRIMARY_CLIP);
    if (!clip) return -2;
    req = &clip->cbh_Req;
    req->io_ClipID = 0;
    req->io_Offset = 0;
    req->io_Command = CMD_WRITE;
    req->io_Data = (STRPTR)buffer;
    req->io_Length = length;
    if (DoIO((struct IORequest *)req) != 0 || req->io_Actual != length) rc = -6;
    if (req->io_ClipID > 0) {
        /* Finish even a failed/short write: do not strand the device writer.
         * Its incomplete stream is rejected by our strict parser, never
         * reported as a successful file copy. */
        req->io_Command = CMD_UPDATE;
        req->io_Data = NULL;
        req->io_Length = 0;
        if (DoIO((struct IORequest *)req) != 0 && !rc) rc = -7;
    } else if (!rc) rc = -7;
    CloseClipboard(clip);
    return rc;
}

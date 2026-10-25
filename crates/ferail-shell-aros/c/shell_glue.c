/* SPDX-License-Identifier: MIT */
/* Copyright (c) 2026 John Knipper */

/*
 * shell_glue.c - the AROS shell integrations behind ferail-shell-aros, as
 * flat C over the system libraries: Workbench (open an object with its
 * default tool, open a drawer), openurl.library, asl.library (drawer
 * requester), clipboard.device through iffparse (IFF FTXT text) and DOS (a
 * shell in a directory). Every entry point returns 0 on success and a
 * negative value on failure; nothing here reports success it did not get.
 */

#include <proto/exec.h>
#include <proto/dos.h>
#include <proto/intuition.h>
#include <proto/workbench.h>
#include <proto/asl.h>
#include <proto/iffparse.h>
#include <proto/openurl.h>

#include <dos/dostags.h>
#include <exec/memory.h>
#include <libraries/asl.h>
#include <libraries/iffparse.h>
#include <libraries/openurl.h>
#include <workbench/workbench.h>

#include <string.h>

struct Library *OpenURLBase;

#define ID_FTXT MAKE_ID('F', 'T', 'X', 'T')
#define ID_CHRS MAKE_ID('C', 'H', 'R', 'S')

/* Open `path` the way a double-click in Wanderer does: a drawer opens as a
 * window, a project through its default tool, a tool runs. */
int fsa_open_wb_object(const char *path)
{
    if (!path || !*path)
        return -1;
    return OpenWorkbenchObject((STRPTR)path, TAG_DONE) ? 0 : -2;
}

/* Hand `url` to openurl.library, which runs the configured browser or mailer. */
int fsa_open_url(const char *url)
{
    ULONG ok;

    if (!url || !*url)
        return -1;
    if (!OpenURLBase) {
        OpenURLBase = OpenLibrary((CONST_STRPTR)"openurl.library", 0);
        if (!OpenURLBase)
            return -3;
    }
    ok = URL_OpenA((STRPTR)url, NULL);
    return ok ? 0 : -2;
}

/* Run `command` as its own process with `dir` as its current directory,
 * detached from the caller (its console is whatever the command opens).
 * `command` is a DOS command line, e.g. "NewShell". */
int fsa_run_in_dir(const char *command, const char *dir)
{
    BPTR lock = BNULL, old = BNULL, in = BNULL, out = BNULL;
    LONG rc;

    if (!command || !*command)
        return -1;
    if (dir && *dir) {
        lock = Lock((CONST_STRPTR)dir, SHARED_LOCK);
        if (!lock)
            return -2;
        old = CurrentDir(lock);
    }
    in = Open((CONST_STRPTR)"NIL:", MODE_OLDFILE);
    out = Open((CONST_STRPTR)"NIL:", MODE_NEWFILE);
    /* The new process duplicates the current directory; asynchronous
     * System() owns and closes the handles it is given. */
    rc = SystemTags((CONST_STRPTR)command,
                    SYS_Asynch, TRUE,
                    SYS_Input, (IPTR)in,
                    SYS_Output, (IPTR)out,
                    TAG_DONE);
    if (rc != 0) {
        if (in)
            Close(in);
        if (out)
            Close(out);
    }
    if (lock) {
        CurrentDir(old);
        UnLock(lock);
    }
    return rc == 0 ? 0 : -3;
}

/* An ASL drawer requester. Returns the chosen drawer, to free with
 * fsa_free_string(), or NULL when cancelled or unavailable. */
char *fsa_pick_drawer(const char *title, const char *initial)
{
    struct FileRequester *fr;
    char *path = NULL;

    fr = AllocAslRequestTags(ASL_FileRequest,
                             title ? ASLFR_TitleText : TAG_IGNORE, (IPTR)title,
                             initial ? ASLFR_InitialDrawer : TAG_IGNORE, (IPTR)initial,
                             ASLFR_DrawersOnly, TRUE,
                             TAG_DONE);
    if (!fr)
        return NULL;
    if (AslRequestTags(fr, TAG_DONE)) {
        ULONG len = strlen((const char *)fr->fr_Drawer) + 1;
        path = AllocVec(len, MEMF_ANY);
        if (path)
            strcpy(path, (const char *)fr->fr_Drawer);
    }
    FreeAslRequest(fr);
    return path;
}

void fsa_free_string(char *s)
{
    FreeVec(s);
}

/* Put `len` bytes of text on clipboard unit 0 as IFF FTXT (one CHRS chunk,
 * ISO-8859-1 as the clipboard convention). */
int fsa_clip_write_text(const unsigned char *text, int len)
{
    struct IFFHandle *iff;
    int rc = -1;

    if (!text || len < 0)
        return -1;
    iff = AllocIFF();
    if (!iff)
        return -2;
    iff->iff_Stream = (IPTR)OpenClipboard(PRIMARY_CLIP);
    if (iff->iff_Stream) {
        InitIFFasClip(iff);
        if (OpenIFF(iff, IFFF_WRITE) == 0) {
            if (PushChunk(iff, ID_FTXT, ID_FORM, IFFSIZE_UNKNOWN) == 0 &&
                PushChunk(iff, ID_FTXT, ID_CHRS, len) == 0 &&
                WriteChunkBytes(iff, (APTR)text, len) == len &&
                PopChunk(iff) == 0 && PopChunk(iff) == 0)
                rc = 0;
            CloseIFF(iff);
        }
        CloseClipboard((struct ClipboardHandle *)iff->iff_Stream);
    }
    FreeIFF(iff);
    return rc;
}

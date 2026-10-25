/* SPDX-License-Identifier: MIT */
/* Copyright (c) 2026 John Knipper */

/*
 * aros_volumes.c - the mounted volumes of a native AROS system for
 * ferail-fs-native: the DOS volume list, with each mounted volume's size,
 * free space, write protection and filesystem type from Info().
 */

#include <proto/exec.h>
#include <proto/dos.h>
#include <dos/dos.h>
#include <dos/dosextens.h>
#include <string.h>

struct fsn_volume {
    char name[64];          /* "Work" */
    unsigned long long total_bytes;
    unsigned long long free_bytes;
    unsigned int dos_type;  /* id_DiskType, e.g. 'DOS\1' */
    int read_only;
    int sizes_known;        /* Info() succeeded */
};

/* Fill `out` with up to `cap` mounted volumes. Returns the count. A volume
 * on the list without a handler task is not mounted (its medium is out) and
 * is skipped, so nothing here raises a "please insert volume" requester. */
int fsn_aros_volumes(struct fsn_volume *out, int cap)
{
    struct DosList *dl;
    int n = 0;

    if (!out || cap <= 0)
        return 0;
    dl = LockDosList(LDF_VOLUMES | LDF_READ);
    while (n < cap && (dl = NextDosEntry(dl, LDF_VOLUMES))) {
        const char *name = (const char *)AROS_BSTR_ADDR(dl->dol_Name);
        int len = AROS_BSTR_strlen(dl->dol_Name);
        if (!dl->dol_Task || len <= 0 || len >= (int)sizeof(out[n].name))
            continue;
        memset(&out[n], 0, sizeof(out[n]));
        memcpy(out[n].name, name, len);
        out[n].name[len] = 0;
        n++;
    }
    UnLockDosList(LDF_VOLUMES | LDF_READ);

    for (int i = 0; i < n; i++) {
        char root[72];
        struct InfoData id;
        BPTR lock;
        struct Process *me = (struct Process *)FindTask(NULL);
        APTR old_window = me->pr_WindowPtr;

        strcpy(root, out[i].name);
        strcat(root, ":");
        /* Never let DOS pop a requester over a file manager's probe. */
        me->pr_WindowPtr = (APTR)-1;
        lock = Lock((CONST_STRPTR)root, SHARED_LOCK);
        if (lock) {
            if (Info(lock, &id)) {
                out[i].total_bytes = (unsigned long long)id.id_NumBlocks * (unsigned long long)id.id_BytesPerBlock;
                out[i].free_bytes = (unsigned long long)(id.id_NumBlocks - id.id_NumBlocksUsed) * (unsigned long long)id.id_BytesPerBlock;
                out[i].dos_type = (unsigned int)id.id_DiskType;
                out[i].read_only = id.id_DiskState == ID_WRITE_PROTECTED;
                out[i].sizes_known = 1;
            }
            UnLock(lock);
        }
        me->pr_WindowPtr = old_window;
    }
    return n;
}

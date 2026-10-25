// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

#include <errno.h>
#include <aros/stdc/errno.h>
#include <proto/dos.h>

/* Unlike C rename(), DOS Rename never deletes an existing destination.
 * The caller supplies absolute native paths, preserving their raw bytes.
 * Return errno explicitly: Rust and stdc need not share errno storage. */
int fsn_aros_rename_noreplace(const char *source, const char *target)
{
    if (Rename(source, target))
        return 0;
    const int error = __stdc_ioerr2errno(IoErr());
    return error != 0 ? error : EIO;
}

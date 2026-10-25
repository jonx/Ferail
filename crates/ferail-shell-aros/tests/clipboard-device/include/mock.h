/* SPDX-License-Identifier: MIT */
/* Copyright (c) 2026 John Knipper */
#ifndef MOCK_H
#define MOCK_H
#include <stdint.h>
#include <stddef.h>
typedef uint32_t ULONG;
typedef char *STRPTR;
#define PRIMARY_CLIP 0
#define CMD_READ 1
#define CMD_WRITE 2
#define CMD_UPDATE 3
struct IOClipReq { int io_Command; STRPTR io_Data; ULONG io_Offset, io_Length, io_Actual; int32_t io_ClipID; };
struct ClipboardHandle { struct IOClipReq cbh_Req; };
struct IORequest;
struct ClipboardHandle *OpenClipboard(int);
void CloseClipboard(struct ClipboardHandle *);
int DoIO(struct IORequest *);
#endif

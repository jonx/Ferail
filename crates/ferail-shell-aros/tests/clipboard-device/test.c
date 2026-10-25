// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper
#include "mock.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
static struct ClipboardHandle handle;
static unsigned char storage[2097153];
static unsigned used, opened, closed, reads, writes, updates, ends;
static int mode;
struct ClipboardHandle *OpenClipboard(int unit) { assert(unit==0); if(mode==1)return NULL; memset(&handle,0,sizeof(handle));opened++;return &handle; }
void CloseClipboard(struct ClipboardHandle *h) {assert(h==&handle);assert(h->cbh_Req.io_ClipID<=0);closed++;}
int DoIO(struct IORequest *raw) {
 struct IOClipReq *r=(void*)raw;
 if(r->io_Command==CMD_READ) {
   if(r->io_Offset==UINT32_MAX) {assert(r->io_Data==NULL);assert(r->io_Length==1); ends++;r->io_ClipID=-1;r->io_Actual=0;return 0;}
   reads++;r->io_ClipID=7;
   if(mode==2)return -1;
   if(mode==3){r->io_Actual=r->io_Length+1;return 0;}
   if(mode==4){r->io_Actual=1;r->io_Offset++;return 0;}
   unsigned n=used-r->io_Offset;if(n>r->io_Length)n=r->io_Length;
   if(n)memcpy(r->io_Data,storage+r->io_Offset,n);r->io_Actual=n;r->io_Offset+=n;
   if(!n)r->io_ClipID=-1;return 0;
 }
 if(r->io_Command==CMD_WRITE){writes++;r->io_ClipID=9;r->io_Actual=mode==5?r->io_Length-1:r->io_Length;used=r->io_Actual;memcpy(storage,r->io_Data,used);return mode==6?-1:0;}
 assert(r->io_Command==CMD_UPDATE);updates++;r->io_ClipID=-1;return mode==7?-1:0;
}
#include "../../c/file_clipboard.c"
int main(void){
 unsigned char *buf=malloc(2097153);unsigned length=99;unsigned char sample[20]={0};
 assert(fsa_clip_write_blob(sample,20)==0);assert(writes==1&&updates==1&&opened==closed);
 assert(fsa_clip_read_blob(buf,2097153,&length)==0&&length==20);assert(opened==closed);
 for(mode=1;mode<=4;mode++){ unsigned before=ends;length=99;assert(fsa_clip_read_blob(buf,2097153,&length)<0);assert(length==0);assert(opened==closed);if(mode>1)assert(ends==before+1); }
 for(mode=5;mode<=7;mode++){unsigned before=updates;assert(fsa_clip_write_blob(sample,20)<0);assert(updates==before+1);assert(opened==closed);}
 mode=0;assert(fsa_clip_write_blob(sample,0)==0);assert(fsa_clip_read_blob(buf,2097153,&length)==0&&length==0);assert(opened==closed);used=2097153;unsigned before=ends;assert(fsa_clip_read_blob(buf,2097153,&length)==-4);assert(ends==before+1);assert(opened==closed);
 assert(fsa_clip_read_blob(buf,10,&length)==-1);assert(fsa_clip_write_blob(NULL,20)==-1);
 free(buf);puts("clipboard transport production source: PASS (ownership, bounded errors, read-end, short write, update)");return 0;
}

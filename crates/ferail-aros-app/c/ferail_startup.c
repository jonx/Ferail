/* SPDX-License-Identifier: MIT */
/* Copyright (c) 2026 John Knipper */

/*
 * ferail_startup.c - what the Ferail process needs from DOS before the GUI
 * boots, called first thing from the Rust `main`.
 *
 * Stack: GPUI's dispatch and layout recursion needs megabytes of stack; a
 * shell hands a command tens of kilobytes, and in AROS's single address
 * space an overflow corrupts neighbouring tasks (emul-handler and
 * graphics.library faults that point nowhere near Ferail). Launch scripts
 * set `Stack 16000000`, but that is not trusted: when the current stack is
 * small, the command re-runs its own segment list through RunCommand() on a
 * DOS-allocated 16 MB stack before any deep code runs. The C start-up runs
 * again in the same process and the re-entered main() sees the big stack.
 * CLI-only by construction (cli_Module is this seglist while it runs);
 * Workbench sizes the stack from the icon's Stack tooltype.
 *
 * Requesters: a file manager probes paths and volumes as routine business
 * (and embedded code, sqlite's VFS path walk, probes names like "/System:");
 * with pr_WindowPtr = -1 DOS returns ERROR_DEVICE_NOT_MOUNTED instead of
 * blocking the task on a "Please insert volume" requester. The convention of
 * every Amiga file manager.
 */

#include <proto/dos.h>
#include <proto/exec.h>
#include <dos/dosextens.h>
#include <exec/tasks.h>

#define FERAIL_STACK_MIN (8ul << 20)
#define FERAIL_STACK_RUN (16ul << 20)

static unsigned long current_stack_size(void)
{
    struct Task *me = FindTask(NULL);
    return (unsigned long)((char *)me->tc_SPUpper - (char *)me->tc_SPLower);
}

/* Returns -1 when the stack is large enough and the program should go on;
 * otherwise the return code of the re-entered run (or 20 when no CLI can
 * relaunch), which the caller returns as the process's result. */
int ferail_aros_relaunch_if_small_stack(void)
{
    struct CommandLineInterface *cli;
    CONST_STRPTR args;
    LONG len = 0;

    if (current_stack_size() >= FERAIL_STACK_MIN)
        return -1;
    cli = Cli();
    if (!cli || !cli->cli_Module) {
        PutStr("Ferail: stack too small and no CLI to relaunch from; "
               "run from a shell or set a Stack tooltype\n");
        return 20;
    }
    args = GetArgStr();
    if (args)
        while (args[len])
            len++;
    return (int)RunCommand(cli->cli_Module, FERAIL_STACK_RUN,
                           (STRPTR)(len ? args : (CONST_STRPTR)"\n"), len ? len : 1);
}

void ferail_aros_prepare_process(void)
{
    struct Process *me = (struct Process *)FindTask(NULL);
    me->pr_WindowPtr = (APTR)-1;
}

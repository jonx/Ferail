/* ferail_main.c -- C harness for Ferail on AROS (mirrors the proven
 * gpui_aros_smoke harness): C owns AROS startup, hands argc/argv to the
 * rust-aros std, and calls the Rust entry that boots the app.
 *
 * Stack self-guard: gpui's dispatch/layout recursion needs megabytes of
 * stack; the shell default (tens of KB) overflows and -- single address
 * space -- corrupts *neighboring* tasks (emul-handler, graphics.library
 * faults that point nowhere near Ferail). Launch scripts set
 * `Stack 16000000`, but do not trust them: if the current stack is small,
 * re-run our own seglist through RunCommand() on a DOS-allocated big one
 * before any deep code runs.
 */
#include <proto/dos.h> /* PutStr -- no <stdio.h> */
#include <proto/exec.h>
#include <dos/dosextens.h>
#include <exec/tasks.h>

#define FERAIL_MAGIC 0x46455241u /* "FERA" */

/* 16 MB is the proven-live value; anything under a few MB overflows. */
#define FERAIL_STACK_MIN (8ul << 20)
#define FERAIL_STACK_RUN (16ul << 20)

extern unsigned int ferail_aros_main(void);

/* Read by std sys/args/aros.rs so std::env::args() works (C owns main). */
int aros_argc = 0;
char **aros_argv = 0;

static unsigned long current_stack_size(void)
{
    struct Task *me = FindTask(0);
    return (unsigned long)((char *)me->tc_SPUpper - (char *)me->tc_SPLower);
}

int main(int argc, char **argv)
{
    unsigned int rc;

    if (current_stack_size() < FERAIL_STACK_MIN) {
        /* Re-enter our own seglist on a big stack. RunCommand swaps the
         * stack and calls the segment entry, so the C startup runs again
         * in this same process (posixc re-inits) and the re-entered main()
         * sees a big stack and takes the direct branch. CLI-only by
         * construction (cli_Module is our seglist while we run); Workbench
         * launches size the stack from the icon's Stack tooltype instead. */
        struct CommandLineInterface *cli = Cli();
        if (cli && cli->cli_Module) {
            CONST_STRPTR args = GetArgStr();
            LONG len = 0;
            if (args)
                while (args[len])
                    len++;
            return RunCommand(cli->cli_Module, FERAIL_STACK_RUN,
                              len ? args : (CONST_STRPTR)"\n",
                              len ? len : 1);
        }
        PutStr("[FERAIL] FAIL: stack too small and no CLI to relaunch "
               "from; run from a shell or set a Stack tooltype\n");
        return 20;
    }

    /* File-manager rule: never let DOS pop "Please insert volume ..."
     * requesters over the app. A file manager probes paths and volumes as
     * routine business (and third-party code we embed — sqlite's VFS path
     * walk — probes garbage like "/System:"); with pr_WindowPtr = -1 DOS
     * returns ERROR_DEVICE_NOT_MOUNTED instead of blocking the calling
     * task on a system requester. Same convention as Directory Opus &
     * every Amiga file manager. */
    {
        struct Process *me = (struct Process *)FindTask(0);
        me->pr_WindowPtr = (APTR)-1;
    }

    aros_argc = argc;
    aros_argv = argv;
    PutStr("[FERAIL] booting (close the last window / Cmd+Q to exit)\n");
    rc = ferail_aros_main();
    if (rc == FERAIL_MAGIC) {
        PutStr("[FERAIL] clean exit\n");
        return 0;
    }
    PutStr("[FERAIL] FAIL\n");
    return 20;
}

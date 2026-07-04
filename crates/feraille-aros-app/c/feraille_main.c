/* feraille_main.c -- C harness for Feraille on AROS (mirrors the proven
 * gpui_aros_smoke harness): C owns AROS startup, hands argc/argv to the
 * rust-aros std, and calls the Rust entry that boots the app.
 */
#include <proto/dos.h> /* PutStr -- no <stdio.h> */

#define FERAILLE_MAGIC 0x46455241u /* "FERA" */

extern unsigned int feraille_aros_main(void);

/* Read by std sys/args/aros.rs so std::env::args() works (C owns main). */
int aros_argc = 0;
char **aros_argv = 0;

int main(int argc, char **argv)
{
    unsigned int rc;
    aros_argc = argc;
    aros_argv = argv;
    PutStr("[FERAILLE] booting (close the last window / Cmd+Q to exit)\n");
    rc = feraille_aros_main();
    if (rc == FERAILLE_MAGIC) {
        PutStr("[FERAILLE] clean exit\n");
        return 0;
    }
    PutStr("[FERAILLE] FAIL\n");
    return 20;
}

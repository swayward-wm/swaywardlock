/* A PAM module for the real-PAM unit tests in src/auth.rs (compiled by the tests themselves).
 * Arguments: mode=<mode> [out=<file>]
 *   auth mode=password  ECHO_OFF "Password: " must be answered "right-pw"
 *   auth mode=otp     ECHO_OFF "Password: " must be answered "right-pw", then ECHO_OFF "OTP: "
 *                     must be answered "123456" (a password-then-one-time-code stack)
 *   auth mode=code    ECHO_OFF "OTP: " must be answered "123456" (stacked after pam_unix for a
 *                     manual real-PAM check)
 *   auth mode=twice   ECHO_OFF "Password: " twice in one call of the module; both answers must
 *                     be "right-pw" (two modules that each ask for the password)
 *   auth mode=thrice  the same, three times
 *   auth mode=echoon  ECHO_ON "Token code: " must be answered "right-pw"
 *   auth mode=codepw  ECHO_OFF "Password: " must be answered "right-pw", then a second ECHO_OFF
 *                     "Password: " must be answered "123456" (a second factor whose prompt
 *                     text is the same as the password's, as pam_radius_auth's default)
 *   auth mode=locked file=<f>  like pam_faillock preauth: decides at the start of the module
 *                     whether the account is locked (<f> exists), then asks ECHO_OFF
 *                     "Password: "; succeeds only for "right-pw" on an account that was not
 *                     locked at the start
 *   auth mode=finger file=<f>  a pam_fprintd stand-in: appends "finger start" to out, sends
 *                     TEXT_INFO "Place your finger on the fingerprint reader", then waits up to
 *                     5 s for <f> (a touch); a touch is consumed (<f> removed) and succeeds,
 *                     no touch returns PAM_AUTHINFO_UNAVAIL
 *   auth mode=sh script=<f>  runs `sh <f>` as a child process (fork + exec, as pam_exec and
 *                     unix_chkpwd are run), waits for it, then succeeds: a test script can look at
 *                     the signal mask and descriptors it inherited
 *   setcred           appends "setcred <flags>" to out, so a test can see it was called
 */
#include <security/pam_modules.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *arg(int argc, const char **argv, const char *key) {
    size_t n = strlen(key);
    for (int i = 0; i < argc; i++)
        if (!strncmp(argv[i], key, n) && argv[i][n] == '=') return argv[i] + n + 1;
    return "";
}

/* Ask one question; returns 1 if the answer equals want. */
static int ask(pam_handle_t *h, int style, const char *prompt, const char *want) {
    const struct pam_conv *conv;
    if (pam_get_item(h, PAM_CONV, (const void **)&conv) != PAM_SUCCESS || !conv) return 0;
    struct pam_message m = {style, prompt};
    const struct pam_message *msgs[1] = {&m};
    struct pam_response *r = NULL;
    if (conv->conv(1, msgs, &r, conv->appdata_ptr) != PAM_SUCCESS || !r) return 0;
    int ok = r[0].resp && !strcmp(r[0].resp, want);
    free(r[0].resp);
    free(r);
    return ok;
}

static void say(pam_handle_t *h, const char *text) {
    const struct pam_conv *conv;
    if (pam_get_item(h, PAM_CONV, (const void **)&conv) != PAM_SUCCESS || !conv) return;
    struct pam_message m = {PAM_TEXT_INFO, text};
    const struct pam_message *msgs[1] = {&m};
    struct pam_response *r = NULL;
    conv->conv(1, msgs, &r, conv->appdata_ptr);
    if (r) {
        free(r[0].resp);
        free(r);
    }
}

static void note(int argc, const char **argv, const char *line) {
    FILE *f = fopen(arg(argc, argv, "out"), "a");
    if (f) {
        fprintf(f, "%s\n", line);
        fclose(f);
    }
}

PAM_EXTERN int pam_sm_authenticate(pam_handle_t *h, int flags, int argc, const char **argv) {
    (void)flags;
    const char *mode = arg(argc, argv, "mode");
    if (!strcmp(mode, "password"))
        return ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw") ? PAM_SUCCESS : PAM_AUTH_ERR;
    if (!strcmp(mode, "otp"))
        return ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw") &&
                       ask(h, PAM_PROMPT_ECHO_OFF, "OTP: ", "123456")
                   ? PAM_SUCCESS
                   : PAM_AUTH_ERR;
    if (!strcmp(mode, "code"))
        return ask(h, PAM_PROMPT_ECHO_OFF, "OTP: ", "123456") ? PAM_SUCCESS : PAM_AUTH_ERR;
    if (!strcmp(mode, "twice"))
        return ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw") &&
                       ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw")
                   ? PAM_SUCCESS
                   : PAM_AUTH_ERR;
    if (!strcmp(mode, "thrice"))
        return ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw") &&
                       ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw") &&
                       ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw")
                   ? PAM_SUCCESS
                   : PAM_AUTH_ERR;
    if (!strcmp(mode, "codepw"))
        return ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw") &&
                       ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "123456")
                   ? PAM_SUCCESS
                   : PAM_AUTH_ERR;
    if (!strcmp(mode, "locked")) {
        int locked = access(arg(argc, argv, "file"), F_OK) == 0;
        int ok = ask(h, PAM_PROMPT_ECHO_OFF, "Password: ", "right-pw");
        return ok && !locked ? PAM_SUCCESS : PAM_AUTH_ERR;
    }
    if (!strcmp(mode, "finger")) {
        note(argc, argv, "finger start");
        say(h, "Place your finger on the fingerprint reader");
        for (int i = 0; i < 500; i++) {
            if (unlink(arg(argc, argv, "file")) == 0) return PAM_SUCCESS;
            usleep(10000);
        }
        return PAM_AUTHINFO_UNAVAIL;
    }
    if (!strcmp(mode, "sh")) {
        pid_t pid = fork();
        if (pid == 0) {
            execl("/bin/sh", "sh", arg(argc, argv, "script"), (char *)NULL);
            _exit(127);
        }
        int status = 0;
        if (pid < 0 || waitpid(pid, &status, 0) < 0) return PAM_AUTH_ERR;
        return PAM_SUCCESS;
    }
    if (!strcmp(mode, "echoon"))
        return ask(h, PAM_PROMPT_ECHO_ON, "Token code: ", "right-pw") ? PAM_SUCCESS : PAM_AUTH_ERR;
    return PAM_IGNORE;
}

PAM_EXTERN int pam_sm_setcred(pam_handle_t *h, int flags, int argc, const char **argv) {
    (void)h;
    char line[32];
    snprintf(line, sizeof line, "setcred %d", flags);
    note(argc, argv, line);
    return PAM_SUCCESS;
}

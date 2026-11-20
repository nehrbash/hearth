#define _GNU_SOURCE
#include <security/pam_modules.h>
#include <security/pam_appl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *option(int argc, const char **argv, const char *key) {
    const size_t length = strlen(key);
    for (int i = 0; i < argc; i++) {
        if (strncmp(argv[i], key, length) == 0) return argv[i] + length;
    }
    return "";
}
static void release(struct pam_response *responses, int count) {
    if (!responses) return;
    for (int i = 0; i < count; i++) {
        if (responses[i].resp) {
            const size_t length = strlen(responses[i].resp);
            volatile char *bytes = responses[i].resp;
            for (size_t j = 0; j < length; j++) bytes[j] = 0;
            free(responses[i].resp);
        }
    }
    free(responses);
}
PAM_EXTERN int pam_sm_authenticate(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)flags;
    const char *mode = option(argc, argv, "mode=");
    const char *pidfile = option(argc, argv, "pidfile=");
    if (*pidfile) {
        FILE *file = fopen(pidfile, "w");
        if (!file) return PAM_SYSTEM_ERR;
        fprintf(file, "%ld\n", (long)getpid());
        fclose(file);
    }
    if (strcmp(mode, "crash") == 0) _exit(73);
    if (strcmp(mode, "hang") == 0) { for (;;) pause(); }
    if (strcmp(mode, "maxtries") == 0) return PAM_MAXTRIES;
    if (strcmp(mode, "permit") == 0) return PAM_SUCCESS;
    if (strcmp(mode, "fork") == 0) {
        pid_t child = fork();
        if (child < 0) return PAM_SYSTEM_ERR;
        if (child == 0) { close(3); for (;;) pause(); }
        char path[4096];
        if (snprintf(path, sizeof(path), "%s.descendant", pidfile) >= (int)sizeof(path)) return PAM_SYSTEM_ERR;
        FILE *file = fopen(path, "w");
        if (!file) return PAM_SYSTEM_ERR;
        fprintf(file, "%ld\n", (long)child);
        fclose(file);
        return PAM_SUCCESS;
    }
    if (strcmp(mode, "switch-user") == 0) return pam_set_item(pamh, PAM_USER, "unrelated-fixture-user");
    const struct pam_conv *conversation = NULL;
    if (pam_get_item(pamh, PAM_CONV, (const void **)&conversation) != PAM_SUCCESS || !conversation) return PAM_SYSTEM_ERR;
    const struct pam_message messages[] = {
        { PAM_TEXT_INFO, "Private fixture information" },
        { PAM_ERROR_MSG, "Private fixture warning" },
        { PAM_PROMPT_ECHO_ON, "Test identity:" },
        { PAM_PROMPT_ECHO_OFF, "Test secret:" }
    };
    const struct pam_message *pointers[] = { &messages[0], &messages[1], &messages[2], &messages[3] };
    struct pam_response *responses = NULL;
    int result = conversation->conv(4, pointers, &responses, conversation->appdata_ptr);
    if (result != PAM_SUCCESS) { release(responses, 4); return result; }
    int correct = responses && !responses[0].resp && !responses[1].resp
        && responses[2].resp && strcmp(responses[2].resp, "fixture-user") == 0
        && responses[3].resp && strcmp(responses[3].resp, "hearth-test-secret") == 0;
    if (strcmp(mode, "noisy") == 0 && responses && responses[3].resp) {
        fprintf(stdout, "%s\n", responses[3].resp);
        fprintf(stderr, "%s\n", responses[3].resp);
        fflush(stdout);
        fflush(stderr);
    }
    release(responses, 4);
    if (!correct) return PAM_AUTH_ERR;
    const struct pam_message otp = { PAM_PROMPT_ECHO_OFF, "Test code:" };
    const struct pam_message *otp_pointer = &otp;
    responses = NULL;
    result = conversation->conv(1, &otp_pointer, &responses, conversation->appdata_ptr);
    if (result != PAM_SUCCESS) { release(responses, 1); return result; }
    correct = responses && responses[0].resp && strcmp(responses[0].resp, "654321") == 0;
    release(responses, 1);
    if (strcmp(mode, "delay") == 0) usleep(500000);
    return correct ? PAM_SUCCESS : PAM_AUTH_ERR;
}
PAM_EXTERN int pam_sm_setcred(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)pamh; (void)flags; (void)argc; (void)argv;
    return PAM_SUCCESS;
}
PAM_EXTERN int pam_sm_acct_mgmt(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)flags;
    if (strcmp(option(argc, argv, "mode="), "account-switch-user") == 0) return pam_set_item(pamh, PAM_USER, "unrelated-fixture-user");
    return strcmp(option(argc, argv, "mode="), "account-denied") == 0 ? PAM_ACCT_EXPIRED : PAM_SUCCESS;
}

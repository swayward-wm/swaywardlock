// LD_PRELOAD shim for e2e tests: pretend to be started setuid/setgid.
//   FAKE_EUID=n    geteuid() returns n
//   FAKE_EGID=n    getegid() returns n
//   FAKE_SECURE=1  getauxval(AT_SECURE) returns 1 (as for file capabilities)
//   cc -shared -fPIC -o euid-shim.so contrib/euid-shim.c
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <sys/auxv.h>
#include <sys/types.h>
#include <unistd.h>

uid_t geteuid(void) {
    const char *v = getenv("FAKE_EUID");
    return v ? (uid_t)atoi(v) : getuid();
}

gid_t getegid(void) {
    const char *v = getenv("FAKE_EGID");
    return v ? (gid_t)atoi(v) : getgid();
}

unsigned long getauxval(unsigned long type) {
    if (type == AT_SECURE && getenv("FAKE_SECURE")) return 1;
    unsigned long (*real)(unsigned long) = (unsigned long (*)(unsigned long))dlsym(RTLD_NEXT, "getauxval");
    return real ? real(type) : 0;
}

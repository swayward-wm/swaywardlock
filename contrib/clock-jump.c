// LD_PRELOAD shim for e2e tests: simulate a suspend for the clocks. Once the file named by
// $CLOCK_JUMP_TRIGGER exists, CLOCK_REALTIME and CLOCK_BOOTTIME (and time/gettimeofday) read
// 3600 s later; CLOCK_MONOTONIC is untouched, as after a real one-hour suspend. With
// $CLOCK_BOOTTIME_FAIL set, CLOCK_BOOTTIME fails with EINVAL instead (as in a sandbox without it).
//   cc -shared -fPIC -o clock-jump.so contrib/clock-jump.c -ldl
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdlib.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static int (*real_clock_gettime)(clockid_t, struct timespec *);

static int jumped(void) {
    const char *t = getenv("CLOCK_JUMP_TRIGGER");
    return t && access(t, F_OK) == 0;
}

int clock_gettime(clockid_t id, struct timespec *ts) {
    if (!real_clock_gettime)
        real_clock_gettime = dlsym(RTLD_NEXT, "clock_gettime");
    if (id == CLOCK_BOOTTIME && getenv("CLOCK_BOOTTIME_FAIL")) {
        errno = EINVAL;
        return -1;
    }
    int r = real_clock_gettime(id, ts);
    if (r == 0 && jumped() &&
        (id == CLOCK_REALTIME || id == CLOCK_REALTIME_COARSE || id == CLOCK_BOOTTIME))
        ts->tv_sec += 3600;
    return r;
}

time_t time(time_t *out) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    if (out)
        *out = ts.tv_sec;
    return ts.tv_sec;
}

int gettimeofday(struct timeval *tv, void *tz) {
    (void)tz;
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    if (tv) {
        tv->tv_sec = ts.tv_sec;
        tv->tv_usec = ts.tv_nsec / 1000;
    }
    return 0;
}

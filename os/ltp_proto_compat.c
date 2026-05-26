#include <netdb.h>
#include <errno.h>
#include <stdint.h>
#include <sched.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <unistd.h>

#ifndef SYS_sched_setscheduler
#define SYS_sched_setscheduler 119
#endif

#ifndef SYS_sched_setparam
#define SYS_sched_setparam 118
#endif

#ifndef SYS_sched_getscheduler
#define SYS_sched_getscheduler 120
#endif

#ifndef SYS_sched_getparam
#define SYS_sched_getparam 121
#endif

struct proto_entry {
    const char *name;
    int proto;
};

static const struct proto_entry proto_table[] = {
    {"ip", 0},
    {"hopopt", 0},
    {"icmp", 1},
    {"igmp", 2},
    {"ggp", 3},
    {"ipv4", 4},
    {"ipencap", 4},
    {"tcp", 6},
    {"udp", 17},
    {"ipv6", 41},
    {"ipv6-route", 43},
    {"ipv6-frag", 44},
    {"esp", 50},
    {"ah", 51},
    {"ipv6-icmp", 58},
    {"ipv6-nonxt", 59},
    {"ipv6-opts", 60},
    {"raw", 255},
};

static char *empty_aliases[] = { NULL };
static struct protoent proto;

static int brk_to(void *addr)
{
    void *ret = (void *)syscall(SYS_brk, addr);

    if (ret != addr) {
        errno = ENOMEM;
        return -1;
    }
    return 0;
}

static struct protoent *make_protoent(const struct proto_entry *entry)
{
    proto.p_name = (char *)entry->name;
    proto.p_aliases = empty_aliases;
    proto.p_proto = entry->proto;
    return &proto;
}

struct protoent *getprotobyname(const char *name)
{
    size_t i;

    for (i = 0; i < sizeof(proto_table) / sizeof(proto_table[0]); i++) {
        if (!strcmp(name, proto_table[i].name)) {
            return make_protoent(&proto_table[i]);
        }
    }
    return NULL;
}

struct protoent *getprotobynumber(int number)
{
    size_t i;

    for (i = 0; i < sizeof(proto_table) / sizeof(proto_table[0]); i++) {
        if (number == proto_table[i].proto) {
            return make_protoent(&proto_table[i]);
        }
    }
    return NULL;
}

int gethostname(char *name, size_t len)
{
    struct utsname uts;
    size_t actual_len;

    if (uname(&uts) < 0) {
        return -1;
    }

    actual_len = strlen(uts.nodename);
    if (actual_len >= len) {
        if (len > 0) {
            memcpy(name, uts.nodename, len);
        }
        errno = ENAMETOOLONG;
        return -1;
    }

    memcpy(name, uts.nodename, actual_len + 1);
    return 0;
}

char *getcwd(char *buf, size_t size)
{
    char *target = buf;
    long ret;

    if (target == NULL) {
        if (size == 0) {
            size = 4096;
        }
        target = malloc(size);
        if (target == NULL) {
            errno = ENOMEM;
            return NULL;
        }
    }

    ret = syscall(SYS_getcwd, target, size);
    if (ret < 0) {
        if (target != buf) {
            free(target);
        }
        return NULL;
    }

    return target;
}

int brk(void *addr)
{
    return brk_to(addr);
}

void *sbrk(intptr_t increment)
{
    uintptr_t old_addr;
    uintptr_t target;
    void *old_brk = (void *)syscall(SYS_brk, 0);

    if (increment == 0) {
        return old_brk;
    }

    old_addr = (uintptr_t)old_brk;
    if (increment > 0) {
        if (old_addr > UINTPTR_MAX - (uintptr_t)increment) {
            errno = ENOMEM;
            return (void *)-1;
        }
        target = old_addr + (uintptr_t)increment;
    } else {
        uintptr_t decrement;

        if (increment == INTPTR_MIN) {
            errno = ENOMEM;
            return (void *)-1;
        }
        decrement = (uintptr_t)(-increment);
        if (old_addr < decrement) {
            errno = ENOMEM;
            return (void *)-1;
        }
        target = old_addr - decrement;
    }

    if (brk_to((void *)target) < 0) {
        return (void *)-1;
    }
    return old_brk;
}

int sched_setscheduler(pid_t pid, int policy, const struct sched_param *param)
{
    return syscall(SYS_sched_setscheduler, pid, policy, param);
}

int sched_setparam(pid_t pid, const struct sched_param *param)
{
    return syscall(SYS_sched_setparam, pid, param);
}

int sched_getscheduler(pid_t pid)
{
    return syscall(SYS_sched_getscheduler, pid);
}

int sched_getparam(pid_t pid, struct sched_param *param)
{
    return syscall(SYS_sched_getparam, pid, param);
}

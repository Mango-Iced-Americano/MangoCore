#include <netdb.h>
#include <sched.h>
#include <stddef.h>
#include <string.h>
#include <sys/syscall.h>
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

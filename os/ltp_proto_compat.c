#include <netdb.h>
#include <stddef.h>
#include <string.h>

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

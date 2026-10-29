#ifndef SCARLET_NATIVE_NETWORK_H
#define SCARLET_NATIVE_NETWORK_H
#include <stdint.h>
#include <stddef.h>
#include <sys/types.h>

struct scarlet_network_link_v1 {
    uint64_t id, generation;
    char name[32];
    uint8_t mac_address[6], kind, state;
    uint32_t mtu, reserved;
};
struct scarlet_network_ipv4_v2 {
    char name[32];
    uint8_t address[4], netmask[4], gateway[4], mac_address[6];
    uint8_t has_address, has_gateway, is_default, reserved[3];
    uint32_t metric;
};
#define SCARLET_NETWORK_LINK_UP 2
#ifdef __cplusplus
extern "C" {
#endif
/* Returns required count without writes if capacity is insufficient; NULL/0 probes. */
ssize_t scarlet_network_list_links_v1(struct scarlet_network_link_v1 *, size_t);
/* Returns records written, up to capacity. Unlike the link API, NULL/0 is invalid. */
ssize_t scarlet_network_list_ipv4_v2(struct scarlet_network_ipv4_v2 *, size_t);
#ifdef __cplusplus
}
#endif
#endif

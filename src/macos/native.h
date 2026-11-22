#ifndef PROCFS_MACOS_NATIVE_H
#define PROCFS_MACOS_NATIVE_H
#include <stddef.h>
#include <stdint.h>

int procfs_os_cwd(uint32_t pid, char *buffer, size_t size);
int procfs_os_owner(uint32_t pid, uint32_t *uid, uint32_t *gid);
int procfs_os_fdtarget(uint32_t pid, int fd, uint32_t type, char *buffer, size_t size);
int procfs_os_io(uint32_t pid, uint64_t counters[2]);
struct procfs_region {
    uint64_t start, end, offset, inode;
    uint32_t major, minor, protection, shared;
};
typedef void (*procfs_region_fn)(void *, const struct procfs_region *, const char *);
int procfs_os_maps(uint32_t pid, void *context, procfs_region_fn fill);

struct procfs_memory {
    uint64_t total, free, inactive, active, wired, file_backed, swap_total, swap_free;
};
int procfs_os_memory(struct procfs_memory *memory);
/* Four counters per CPU, in Linux order: user, nice, system, idle. */
int procfs_os_cpu(uint64_t *states, size_t slots);
typedef void (*procfs_mount_fn)(void *, const char *, const char *, const char *, uint32_t);
int procfs_os_mounts(void *context, procfs_mount_fn fill);
typedef void (*procfs_net_fn)(void *, const char *, const uint64_t *);
int procfs_os_net(void *context, procfs_net_fn fill);
typedef void (*procfs_disk_fn)(void *, const char *, uint32_t, uint32_t, const uint64_t *);
int procfs_os_disks(void *context, procfs_disk_fn fill);
#endif

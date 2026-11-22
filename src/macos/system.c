/* SDK-dependent system layouts and queries; Rust owns rendering. */
#include "native.h"
#include <sys/sysctl.h>
#include <sys/mount.h>
#include <mach/mach.h>
#include <mach/processor_info.h>
#include <net/if.h>
#include <net/route.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/IOBSD.h>
#include <CoreFoundation/CoreFoundation.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>

_Static_assert(sizeof(struct procfs_memory) == 64, "Rust/C memory ABI mismatch");

int procfs_os_memory(struct procfs_memory *memory) {
    memset(memory, 0, sizeof(*memory));
    size_t size = sizeof(memory->total);
    if (sysctlbyname("hw.memsize", &memory->total, &size, NULL, 0) != 0) return -errno;
    if (size != sizeof(memory->total)) return -EIO;
    vm_statistics64_data_t vm;
    mach_msg_type_number_t count = HOST_VM_INFO64_COUNT;
    mach_port_t host = mach_host_self();
    kern_return_t result = host_statistics64(host, HOST_VM_INFO64, (host_info64_t)&vm, &count);
    mach_port_deallocate(mach_task_self(), host);
    if (result != KERN_SUCCESS) return -EIO;
    uint64_t page = vm_kernel_page_size;
    memory->free = (uint64_t)vm.free_count * page;
    memory->inactive = (uint64_t)vm.inactive_count * page;
    memory->active = (uint64_t)vm.active_count * page;
    memory->wired = (uint64_t)vm.wire_count * page;
    memory->file_backed = (uint64_t)vm.external_page_count * page;
    struct xsw_usage swap;
    size = sizeof(swap);
    if (sysctlbyname("vm.swapusage", &swap, &size, NULL, 0) != 0) return -errno;
    if (size != sizeof(swap)) return -EIO;
    memory->swap_total = swap.xsu_total;
    memory->swap_free = swap.xsu_avail;
    return 0;
}

int procfs_os_cpu(uint64_t *states, size_t slots) {
    natural_t processors = 0;
    processor_info_array_t info = NULL;
    mach_msg_type_number_t count = 0;
    mach_port_t host = mach_host_self();
    kern_return_t result = host_processor_info(host, PROCESSOR_CPU_LOAD_INFO, &processors, &info, &count);
    mach_port_deallocate(mach_task_self(), host);
    if (result != KERN_SUCCESS) return -EIO;
    int status = (int)processors;
    if (count < processors * CPU_STATE_MAX) status = -EIO;
    else if (states != NULL && slots < (size_t)processors * 4) status = -ENOSPC;
    else if (states != NULL) {
        for (natural_t cpu = 0; cpu < processors; cpu++) {
            const integer_t *ticks = info + cpu * CPU_STATE_MAX;
            states[cpu * 4] = (uint32_t)ticks[CPU_STATE_USER];
            states[cpu * 4 + 1] = (uint32_t)ticks[CPU_STATE_NICE];
            states[cpu * 4 + 2] = (uint32_t)ticks[CPU_STATE_SYSTEM];
            states[cpu * 4 + 3] = (uint32_t)ticks[CPU_STATE_IDLE];
        }
    }
    vm_deallocate(mach_task_self(), (vm_address_t)info, (vm_size_t)count * sizeof(integer_t));
    return status;
}

int procfs_os_mounts(void *context, procfs_mount_fn fill) {
    int count = getfsstat(NULL, 0, MNT_NOWAIT);
    if (count < 0) return -errno;
    size_t capacity = (size_t)count + 16;
    struct statfs *mounts = calloc(capacity, sizeof(*mounts));
    if (mounts == NULL) return -ENOMEM;
    count = getfsstat(mounts, (int)(capacity * sizeof(*mounts)), MNT_NOWAIT);
    if (count < 0) { int error = errno; free(mounts); return -error; }
    if ((size_t)count > capacity) { free(mounts); return -EOVERFLOW; }
    for (int i = 0; i < count; i++) {
        uint64_t native = mounts[i].f_flags;
        uint32_t flags = (native & MNT_RDONLY ? 1 : 0) |
            (native & MNT_NOSUID ? 2 : 0) | (native & MNT_NODEV ? 4 : 0) |
            (native & MNT_NOEXEC ? 8 : 0) | (native & MNT_NOATIME ? 16 : 0) |
            (native & MNT_SYNCHRONOUS ? 32 : 0) | (native & MNT_ASYNC ? 64 : 0);
        fill(context, mounts[i].f_mntfromname, mounts[i].f_mntonname, mounts[i].f_fstypename, flags);
    }
    free(mounts);
    return 0;
}

int procfs_os_net(void *context, procfs_net_fn fill) {
    int mib[] = {CTL_NET, PF_ROUTE, 0, 0, NET_RT_IFLIST2, 0};
    /* Interface creation can grow the table between sizing and fetching. */
    for (int attempt = 0; attempt < 4; attempt++) {
        size_t size = 0;
        if (sysctl(mib, 6, NULL, &size, NULL, 0) != 0) return -errno;
        if (!size) return 0;
        char *data = malloc(size);
        if (!data) return -ENOMEM;
        if (sysctl(mib, 6, data, &size, NULL, 0) != 0) {
            int error = errno;
            free(data);
            if (error == ENOMEM) continue;
            return -error;
        }
        int status = 0;
        for (size_t offset = 0; offset < size;) {
            /* Copy instead of casting potentially unaligned routing messages. */
            struct { uint16_t length; uint8_t version, type; } header;
            if (size - offset < sizeof(header)) { status = -EIO; break; }
            memcpy(&header, data + offset, sizeof(header));
            if (header.length < sizeof(header) || header.length > size - offset) {
                status = -EIO; break;
            }
            if (header.version == RTM_VERSION && header.type == RTM_IFINFO2) {
                struct if_msghdr2 m;
                if (header.length < sizeof(m)) { status = -EIO; break; }
                memcpy(&m, data + offset, sizeof(m));
                char name[IF_NAMESIZE];
                if (if_indextoname(m.ifm_index, name)) {
                    const struct if_data64 *d = &m.ifm_data;
                    uint64_t counters[16] = {
                        d->ifi_ibytes, d->ifi_ipackets, d->ifi_ierrors, d->ifi_iqdrops,
                        0, 0, 0, d->ifi_imcasts,
                        d->ifi_obytes, d->ifi_opackets, d->ifi_oerrors, (uint32_t)m.ifm_snd_drops,
                        0, d->ifi_collisions, 0, 0,
                    };
                    fill(context, name, counters);
                } else if (errno != ENXIO && errno != ENODEV) {
                    status = -errno; break;
                }
            }
            offset += header.length;
        }
        free(data);
        return status;
    }
    return -EAGAIN;
}

static int number(CFDictionaryRef dict, CFStringRef key, uint64_t *out) {
    CFTypeRef value = CFDictionaryGetValue(dict, key);
    int64_t n;
    if (!value || CFGetTypeID(value) != CFNumberGetTypeID() ||
        !CFNumberGetValue(value, kCFNumberSInt64Type, &n) || n < 0) return -EIO;
    *out = (uint64_t)n;
    return 0;
}

int procfs_os_disks(void *context, procfs_disk_fn fill) {
    io_iterator_t iterator = IO_OBJECT_NULL;
    kern_return_t result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOMedia"), &iterator);
    if (result != KERN_SUCCESS) return -EIO;
    io_object_t disk;
    int status = 0;
    while ((disk = IOIteratorNext(iterator))) {
        io_registry_entry_t parent = IO_OBJECT_NULL;
        CFMutableDictionaryRef props = NULL;
        CFTypeRef stats = NULL;
        if (IORegistryEntryGetParentEntry(disk, kIOServicePlane, &parent) != KERN_SUCCESS) {
            status = -EIO;
        } else if (IOObjectConformsTo(parent, "IOBlockStorageDriver")) {
            if (IORegistryEntryCreateCFProperties(disk, &props, kCFAllocatorDefault, 0) != KERN_SUCCESS)
                status = -EIO;
            else {
                stats = IORegistryEntryCreateCFProperty(parent, CFSTR("Statistics"), kCFAllocatorDefault, 0);
                CFTypeRef name = CFDictionaryGetValue(props, CFSTR(kIOBSDNameKey));
                uint64_t maj = 0, min = 0, counters[6] = {0};
                char text[128];
                if (!name || CFGetTypeID(name) != CFStringGetTypeID() ||
                    !CFStringGetCString(name, text, sizeof(text), kCFStringEncodingUTF8) ||
                    !stats || CFGetTypeID(stats) != CFDictionaryGetTypeID() ||
                    number(props, CFSTR(kIOBSDMajorKey), &maj) || number(props, CFSTR(kIOBSDMinorKey), &min))
                    status = -EIO;
                else {
                    CFStringRef keys[] = {CFSTR("Operations (Read)"), CFSTR("Operations (Write)"),
                        CFSTR("Bytes (Read)"), CFSTR("Bytes (Write)"),
                        CFSTR("Total Time (Read)"), CFSTR("Total Time (Write)")};
                    for (int i = 0; i < 6; i++) {
                        if (number((CFDictionaryRef)stats, keys[i], &counters[i])) status = -EIO;
                    }
                    if (!status) fill(context, text, (uint32_t)maj, (uint32_t)min, counters);
                }
            }
        }
        if (stats) CFRelease(stats);
        if (props) CFRelease(props);
        if (parent) IOObjectRelease(parent);
        IOObjectRelease(disk);
        if (status) break;
    }
    IOObjectRelease(iterator);
    return status;
}

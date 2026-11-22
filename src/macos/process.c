/* SDK-dependent process layouts and queries; Rust owns rendering. */
#include "native.h"
#include <libproc.h>
#include <sys/proc_info.h>
#include <sys/resource.h>
#include <sys/sysctl.h>
#include <sys/types.h>
#include <errno.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

_Static_assert(sizeof(struct procfs_region) == 48, "Rust/C region ABI mismatch");
_Static_assert(sizeof(struct proc_fdinfo) == 8, "Rust/C descriptor ABI mismatch");
_Static_assert(sizeof(struct proc_threadinfo) == 112, "Rust/C thread ABI mismatch");

int procfs_os_owner(uint32_t pid, uint32_t *uid, uint32_t *gid) {
    struct kinfo_proc info;
    int mib[] = { CTL_KERN, KERN_PROC, KERN_PROC_PID, (int)pid };
    size_t size = sizeof(info);
    // KERN_PROC_PID exposes public credentials even for processes whose
    // detailed libproc task information the mounting user cannot read.
    if (sysctl(mib, 4, &info, &size, NULL, 0) != 0) return -errno;
    if (size == 0) return -ENOENT;
    if (size != sizeof(info) || info.kp_eproc.e_ucred.cr_ngroups < 1) return -EIO;
    *uid = info.kp_eproc.e_ucred.cr_uid;
    *gid = info.kp_eproc.e_ucred.cr_groups[0];
    return 0;
}

int procfs_os_cwd(uint32_t pid, char *buffer, size_t size) {
    struct proc_vnodepathinfo info;
    int count = proc_pidinfo((int)pid, PROC_PIDVNODEPATHINFO, 0, &info, sizeof(info));
    if (count <= 0) return -(errno ? errno : EIO);
    if ((size_t)count != sizeof(info)) return -EIO;
    size_t length = strnlen(info.pvi_cdir.vip_path, sizeof(info.pvi_cdir.vip_path));
    if (length == sizeof(info.pvi_cdir.vip_path)) return -EIO;
    if (length == 0) return -ENOENT;
    if (length >= size) return -ENAMETOOLONG;
    memcpy(buffer, info.pvi_cdir.vip_path, length);
    buffer[length] = '\0';
    return (int)length;
}

static int fdquery(uint32_t pid, int fd, int flavor, void *data, int size) {
    int result = proc_pidfdinfo((int)pid, fd, flavor, data, size);
    if (result <= 0) return -(errno == EBADF ? ENOENT : (errno ? errno : EIO));
    return result == size ? 0 : -EIO;
}

int procfs_os_fdtarget(uint32_t pid, int fd, uint32_t type, char *buffer, size_t size) {
    int result;
    if (type == PROX_FDTYPE_VNODE) {
        struct vnode_fdinfowithpath info = {0};
        result = fdquery(pid, fd, PROC_PIDFDVNODEPATHINFO, &info, sizeof(info));
        if (result < 0) return result;
        size_t length = strnlen(info.pvip.vip_path, sizeof(info.pvip.vip_path));
        if (length == sizeof(info.pvip.vip_path)) return -EIO;
        if (!length) return -ENOENT;
        if (length >= size) return -ENAMETOOLONG;
        memcpy(buffer, info.pvip.vip_path, length + 1);
        return (int)length;
    }
    if (type == PROX_FDTYPE_SOCKET) {
        struct socket_fdinfo info = {0};
        result = fdquery(pid, fd, PROC_PIDFDSOCKETINFO, &info, sizeof(info));
        if (result < 0) return result;
        result = snprintf(buffer, size, "socket:[%" PRIu64 "]", info.psi.soi_so);
    } else if (type == PROX_FDTYPE_PIPE) {
        struct pipe_fdinfo info = {0};
        result = fdquery(pid, fd, PROC_PIDFDPIPEINFO, &info, sizeof(info));
        if (result < 0) return result;
        result = snprintf(buffer, size, "pipe:[%" PRIu64 "]", info.pipeinfo.pipe_handle);
    } else {
        const char *name = NULL;
        switch (type) {
            case PROX_FDTYPE_KQUEUE: name = "kqueue"; break;
            case PROX_FDTYPE_PSHM: name = "pshm"; break;
            case PROX_FDTYPE_PSEM: name = "psem"; break;
            case PROX_FDTYPE_FSEVENTS: name = "fsevents"; break;
            default: break;
        }
        result = name ? snprintf(buffer, size, "anon_inode:[%s]", name)
                      : snprintf(buffer, size, "anon_inode:[darwin:%u]", type);
    }
    if (result < 0) return -EIO;
    return (size_t)result < size ? result : -ENAMETOOLONG;
}

int procfs_os_io(uint32_t pid, uint64_t counters[2]) {
    struct rusage_info_v2 usage = {0};
    if (proc_pid_rusage((int)pid, RUSAGE_INFO_V2, (rusage_info_t *)&usage) != 0) return -(errno ? errno : EIO);
    counters[0] = usage.ri_diskio_bytesread;
    counters[1] = usage.ri_diskio_byteswritten;
    return 0;
}

int procfs_os_maps(uint32_t pid, void *context, procfs_region_fn fill) {
    uint64_t address = 0;
    /* Bound a racing or pathological address space, never return a truncated sample. */
    for (size_t count = 0; count < 1000000; count++) {
        struct proc_regionwithpathinfo info = {0};
        int result = proc_pidinfo((int)pid, PROC_PIDREGIONPATHINFO, address, &info, sizeof(info));
        if (result <= 0) {
            /* The kernel reports EINVAL at the end of the address space. */
            if (count && errno == EINVAL) return 0;
            return -(errno ? errno : EIO);
        }
        if (result != sizeof(info)) return -EIO;
        struct proc_regioninfo *r = &info.prp_prinfo;
        if (!r->pri_size || r->pri_address < address || UINT64_MAX - r->pri_address < r->pri_size)
            return -EIO;
        struct vinfo_stat *s = &info.prp_vip.vip_vi.vi_stat;
        struct procfs_region region = {
            .start = r->pri_address, .end = r->pri_address + r->pri_size,
            .offset = r->pri_offset, .inode = s->vst_ino,
            .major = major(s->vst_dev), .minor = minor(s->vst_dev),
            .protection = r->pri_protection, .shared = !!(r->pri_flags & PROC_REGION_SHARED),
        };
        info.prp_vip.vip_path[sizeof(info.prp_vip.vip_path) - 1] = 0;
        fill(context, &region, info.prp_vip.vip_path);
        address = region.end;
        if (address == UINT64_MAX) return 0;
    }
    return -E2BIG;
}


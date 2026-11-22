#ifndef PROCFS_BRIDGE_H
#define PROCFS_BRIDGE_H
#include <stddef.h>
#include <stdint.h>

struct procfs_attr {
    uint64_t ino;
    uint64_t size;
    uint64_t mtime_sec;
    uint32_t mtime_nsec;
    uint32_t kind; /* 0 = regular file, 1 = directory, 2 = symlink */
    uint32_t uid;
    uint32_t gid;
};

typedef int (*procfs_fill_fn)(void *context, const char *name, int64_t next_offset);
int procfs_getattr(const char *path, struct procfs_attr *attr);
int procfs_readlink(const char *path, char *buffer, size_t size);
int procfs_open_file(const char *path, void **handle, struct procfs_attr *attr);
int procfs_file_attr(const void *handle, struct procfs_attr *attr);
int procfs_read_file(const void *handle, char *buffer, size_t size, int64_t offset);
void procfs_close_file(void *handle);
int procfs_open_directory(const char *path, void **handle);
int procfs_read_directory(const void *handle, int64_t offset, void *context, procfs_fill_fn fill);
void procfs_close_directory(void *handle);
int procfs_fuse_main(int argc, char **argv);
#endif

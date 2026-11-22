#define FUSE_USE_VERSION 26
#include <fuse.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <string.h>
#include <unistd.h>
#include "fuse.h"

_Static_assert(sizeof(struct procfs_attr) == 40, "Rust/C attribute ABI mismatch");
static void fill_attr(struct stat *st, const struct procfs_attr *attr) {
    memset(st, 0, sizeof(*st));
    st->st_ino = attr->ino;
    st->st_mode = attr->kind == 1 ? (S_IFDIR | 0555) :
                  attr->kind == 2 ? (S_IFLNK | 0777) : (S_IFREG | 0444);
    st->st_nlink = attr->kind == 1 ? 2 : 1;
    st->st_uid = attr->uid;
    st->st_gid = attr->gid;
    st->st_size = attr->size;
    st->st_blksize = 4096;
    st->st_blocks = (attr->size + 511) / 512;
    st->st_mtimespec.tv_sec = attr->mtime_sec;
    st->st_mtimespec.tv_nsec = attr->mtime_nsec;
    st->st_atimespec = st->st_ctimespec = st->st_mtimespec;
}

static int fs_getattr(const char *path, struct stat *st) {
    struct procfs_attr attr;
    int result = procfs_getattr(path, &attr);
    if (result == 0) fill_attr(st, &attr);
    return result;
}

static int fs_fgetattr(const char *path, struct stat *st, struct fuse_file_info *fi) {
    if (!fi || !fi->fh) return fs_getattr(path, st);
    struct procfs_attr attr;
    int result = procfs_file_attr((const void *)(uintptr_t)fi->fh, &attr);
    if (result == 0) fill_attr(st, &attr);
    return result;
}

static int fs_open(const char *path, struct fuse_file_info *fi) {
    if ((fi->flags & O_ACCMODE) != O_RDONLY || (fi->flags & O_TRUNC)) return -EROFS;
    void *handle = NULL;
    struct procfs_attr attr;
    int result = procfs_open_file(path, &handle, &attr);
    if (result == 0) {
        fi->fh = (uint64_t)(uintptr_t)handle;
        fi->direct_io = 1;
        fi->keep_cache = 0;
    }
    return result;
}

static int fs_read(const char *path, char *buffer, size_t size, off_t offset,
                   struct fuse_file_info *fi) {
    (void)path;
    if (size > INT_MAX) size = INT_MAX;
    return procfs_read_file((const void *)(uintptr_t)fi->fh, buffer, size, offset);
}

static int fs_release(const char *path, struct fuse_file_info *fi) {
    (void)path;
    procfs_close_file((void *)(uintptr_t)fi->fh);
    fi->fh = 0;
    return 0;
}

static int fs_opendir(const char *path, struct fuse_file_info *fi) {
    void *handle = NULL;
    int result = procfs_open_directory(path, &handle);
    if (result == 0) fi->fh = (uint64_t)(uintptr_t)handle;
    return result;
}

struct directory_context { void *buffer; fuse_fill_dir_t filler; };

static int fill_directory(void *opaque, const char *name, int64_t offset) {
    struct directory_context *context = opaque;
    return context->filler(context->buffer, name, NULL, offset);
}

static int fs_readdir(const char *path, void *buffer, fuse_fill_dir_t filler,
                      off_t offset, struct fuse_file_info *fi) {
    (void)path;
    struct directory_context context = { .buffer = buffer, .filler = filler };
    return procfs_read_directory((const void *)(uintptr_t)fi->fh, offset,
                                 &context, fill_directory);
}

static int fs_releasedir(const char *path, struct fuse_file_info *fi) {
    (void)path;
    procfs_close_directory((void *)(uintptr_t)fi->fh);
    fi->fh = 0;
    return 0;
}

static int fs_access(const char *path, int mask) {
    struct stat st;
    int result = fs_getattr(path, &st);
    if (result != 0) return result;
    if (mask & W_OK) return -EROFS;
    if ((mask & X_OK) && !S_ISDIR(st.st_mode)) return -EACCES;
    return 0;
}

static int fs_write(const char *path, const char *buffer, size_t size, off_t offset,
                    struct fuse_file_info *fi) {
    (void)path; (void)buffer; (void)size; (void)offset; (void)fi;
    return -EROFS;
}

static int fs_flush(const char *path, struct fuse_file_info *fi) {
    (void)path; (void)fi;
    return 0;
}

static void *fs_init(struct fuse_conn_info *connection) {
    (void)connection;
    int *initialized = fuse_get_context()->private_data;
    *initialized = 1;
    return initialized;
}

int procfs_fuse_main(int argc, char **argv) {
    const struct fuse_operations operations = {
        .getattr = fs_getattr,
        .readlink = procfs_readlink,
        .fgetattr = fs_fgetattr,
        .open = fs_open,
        .read = fs_read,
        .release = fs_release,
        .opendir = fs_opendir,
        .readdir = fs_readdir,
        .releasedir = fs_releasedir,
        .access = fs_access,
        .write = fs_write,
        .flush = fs_flush,
        .init = fs_init,
        .flag_nullpath_ok = 1,
    };
    // Unlike a raw /dev/fuse loop, libfuse supports MFMount's FSKit channel
    // and the transport-provided reply buffers used by large reads.
    int initialized = 0;
    int result = fuse_main(argc, argv, &operations, &initialized);
    // macFUSE can return zero after a failed asynchronous FSKit mount. Only
    // report success if the filesystem actually received FUSE_INIT.
    return result == 0 && initialized ? 0 : 1;
}

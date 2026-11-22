//! Narrow ABI boundary. C owns libfuse structs; Rust owns samples and listings.
use crate::procfs::{self, Attr, Directory, OpenFile};
use libc::{c_char, c_int, c_void};
use std::ffi::CStr;

unsafe extern "C" {
    pub(crate) fn procfs_fuse_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

// Both files and directories can reach libfuse's fgetattr callback.
enum Handle {
    File(OpenFile),
    Directory(Directory),
}

// The C adapter passes valid NUL-terminated paths, writable output storage,
// and opaque pointers returned by the matching open callback. Every handle
// is released exactly once by its matching release callback. libfuse runs
// single-threaded (-s), so callbacks cannot race handle destruction.
unsafe fn path<'a>(value: *const c_char) -> Result<&'a CStr, i32> {
    if value.is_null() {
        Err(libc::EINVAL)
    } else {
        Ok(unsafe { CStr::from_ptr(value) })
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_getattr(name: *const c_char, attr: *mut Attr) -> c_int {
    match unsafe { path(name) }.and_then(procfs::getattr) {
        Ok(value) => {
            unsafe { attr.write(value) };
            0
        }
        Err(error) => -error,
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_readlink(name: *const c_char, buffer: *mut u8, size: usize) -> c_int {
    if size == 0 {
        return -libc::EINVAL;
    }
    match unsafe { path(name) }.and_then(procfs::readlink) {
        Ok(target) => {
            let len = target.len().min(size - 1);
            unsafe {
                std::ptr::copy_nonoverlapping(target.as_ptr(), buffer, len);
                buffer.add(len).write(0);
            }
            0
        }
        Err(error) => -error,
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_open_file(
    name: *const c_char,
    handle: *mut *mut c_void,
    attr: *mut Attr,
) -> c_int {
    match unsafe { path(name) }.and_then(OpenFile::open) {
        Ok(file) => {
            unsafe {
                attr.write(file.attr);
                handle.write(Box::into_raw(Box::new(Handle::File(file))).cast());
            }
            0
        }
        Err(error) => -error,
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_file_attr(handle: *const c_void, attr: *mut Attr) -> c_int {
    let Some(handle) = (unsafe { handle.cast::<Handle>().as_ref() }) else {
        return -libc::EBADF;
    };
    let value = match handle {
        Handle::File(file) => file.attr,
        Handle::Directory(directory) => directory.attr,
    };
    unsafe { attr.write(value) };
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_read_file(
    handle: *const c_void,
    buffer: *mut u8,
    size: usize,
    offset: i64,
) -> c_int {
    let Some(handle) = (unsafe { handle.cast::<Handle>().as_ref() }) else {
        return -libc::EBADF;
    };
    let Handle::File(file) = handle else {
        return -libc::EISDIR;
    };
    match file.read(offset, size.min(c_int::MAX as usize)) {
        Ok(bytes) => {
            if !bytes.is_empty() {
                unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
            }
            bytes.len() as c_int
        }
        Err(error) => -error,
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_close_file(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Handle>()) });
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_open_directory(name: *const c_char, handle: *mut *mut c_void) -> c_int {
    match unsafe { path(name) }.and_then(Directory::open) {
        Ok(directory) => {
            unsafe { handle.write(Box::into_raw(Box::new(Handle::Directory(directory))).cast()) };
            0
        }
        Err(error) => -error,
    }
}

type Fill = unsafe extern "C" fn(*mut c_void, *const c_char, i64) -> c_int;

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_read_directory(
    handle: *const c_void,
    offset: i64,
    context: *mut c_void,
    fill: Fill,
) -> c_int {
    let Some(handle) = (unsafe { handle.cast::<Handle>().as_ref() }) else {
        return -libc::EBADF;
    };
    let Handle::Directory(directory) = handle else {
        return -libc::ENOTDIR;
    };
    let Ok(offset) = usize::try_from(offset) else {
        return -libc::EINVAL;
    };
    for (index, name) in directory.entries.iter().enumerate().skip(offset) {
        if unsafe { fill(context, name.as_ptr(), (index + 1) as i64) } != 0 {
            break;
        }
    }
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn procfs_close_directory(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Handle>()) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    struct Page {
        names: Vec<CString>,
        next: i64,
    }
    unsafe extern "C" fn fill(context: *mut c_void, name: *const c_char, next: i64) -> c_int {
        let page = unsafe { &mut *context.cast::<Page>() };
        if page.names.len() == 3 {
            return 1;
        }
        page.names.push(unsafe { CStr::from_ptr(name) }.to_owned());
        page.next = next;
        0
    }

    #[test]
    fn directory_handles_support_attributes_but_reject_file_reads() {
        let mut handle = std::ptr::null_mut();
        assert_eq!(
            unsafe { procfs_open_directory(c"/".as_ptr(), &mut handle) },
            0
        );
        let mut attr = std::mem::MaybeUninit::<Attr>::uninit();
        assert_eq!(unsafe { procfs_file_attr(handle, attr.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { attr.assume_init() }.kind, 1);
        assert_eq!(
            unsafe { procfs_read_file(handle, std::ptr::null_mut(), 0, 0) },
            -libc::EISDIR
        );
        unsafe { procfs_close_directory(handle) };
    }

    #[test]
    fn directory_pagination_does_not_drop_entries() {
        let directory = Directory {
            attr: procfs::getattr(c"/").unwrap(),
            entries: (0..2048)
                .map(|i| CString::new(i.to_string()).unwrap())
                .collect(),
        };
        let owned = Handle::Directory(directory);
        let Handle::Directory(directory) = &owned else {
            unreachable!()
        };
        let handle = (&owned as *const Handle).cast();
        let mut offset = 0;
        let mut names = Vec::new();
        loop {
            let mut page = Page {
                names: Vec::new(),
                next: offset,
            };
            assert_eq!(
                unsafe {
                    procfs_read_directory(handle, offset, (&mut page as *mut Page).cast(), fill)
                },
                0
            );
            if page.names.is_empty() {
                break;
            }
            offset = page.next;
            names.extend(page.names);
        }
        assert_eq!(names, directory.entries);
        assert_eq!(std::mem::size_of::<Attr>(), 40);
        assert_eq!(
            unsafe { procfs_read_directory(handle, -1, std::ptr::null_mut(), fill) },
            -libc::EINVAL
        );
    }
}

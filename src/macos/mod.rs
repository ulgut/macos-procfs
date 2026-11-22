//! Darwin data collection. No filesystem paths or FUSE types belong here.
pub(crate) mod process;
pub(crate) mod system;

use libc::c_int;
use std::io;

fn checked(result: c_int) -> io::Result<usize> {
    if result < 0 {
        Err(io::Error::from_raw_os_error(-result))
    } else {
        Ok(result as usize)
    }
}

pub(crate) fn boot_time() -> io::Result<libc::timeval> {
    let mut value: libc::timeval = unsafe { std::mem::zeroed() };
    let mut size = size_of::<libc::timeval>();
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            (&mut value as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if size != size_of::<libc::timeval>() || value.tv_sec <= 0 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(value)
}

pub(crate) fn sysconf_positive(name: c_int) -> io::Result<u64> {
    let value = unsafe { libc::sysconf(name) };
    if value > 0 {
        Ok(value as u64)
    } else {
        Err(io::Error::from_raw_os_error(libc::EIO))
    }
}

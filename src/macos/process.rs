//! Process enumeration, identity, threads, descriptors, and per-process data.
use super::{boot_time, checked, sysconf_positive};
use libc::{c_char, c_int, c_void, size_t};
use std::ffi::CStr;
use std::io;

const PROC_PIDTASKALLINFO: c_int = 2;
const MAXCOMLEN: usize = 16;

#[repr(C)]
#[derive(Default)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32, // process status: 1=idle 2=run 3=sleep 4=stop 5=zombie
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [u8; MAXCOMLEN],
    pbi_name: [u8; 2 * MAXCOMLEN],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

#[repr(C)]
#[derive(Default)]
struct ProcTaskInfo {
    pti_virtual_size: u64,
    pti_resident_size: u64,
    pti_total_user: u64,   // Mach ticks
    pti_total_system: u64, // Mach ticks
    pti_threads_user: u64,
    pti_threads_system: u64,
    pti_policy: i32,
    pti_faults: i32,
    pti_pageins: i32,
    pti_cow_faults: i32,
    pti_messages_sent: i32,
    pti_messages_received: i32,
    pti_syscalls_mach: i32,
    pti_syscalls_unix: i32,
    pti_csw: i32,
    pti_threadnum: i32,
    pti_numrunning: i32,
    pti_priority: i32,
}

#[repr(C)]
#[derive(Default)]
struct ProcTaskAllInfo {
    pbsd: ProcBsdInfo,
    ptinfo: ProcTaskInfo,
}

#[link(name = "proc")]
unsafe extern "C" {
    /// Returns number of PIDs written; pass null+0 to get the count first.
    fn proc_listallpids(buffer: *mut i32, buffersize: i32) -> i32;

    /// Returns bytes written on success, ≤0 on error.
    fn proc_pidinfo(
        pid: i32,
        flavor: c_int,
        arg: u64,
        buffer: *mut c_void,
        buffersize: c_int,
    ) -> c_int;
}

pub(crate) fn list_pids() -> io::Result<Vec<u32>> {
    unsafe {
        // First call: get count.
        let count = proc_listallpids(std::ptr::null_mut(), 0);
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        // Add headroom for processes spawned between calls.
        let cap = (count as usize) + 16;
        let mut buf: Vec<i32> = vec![0; cap];
        let n = proc_listallpids(buf.as_mut_ptr(), (cap * size_of::<i32>()) as i32);
        if n <= 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf[..(n as usize).min(buf.len())]
            .iter()
            .filter(|&&pid| pid > 0)
            .map(|&pid| pid as u32)
            .collect())
    }
}

pub(crate) fn pid_exists(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    unsafe {
        let ret = libc::kill(pid as libc::pid_t, 0);
        ret == 0 || *libc::__error() == libc::EPERM
    }
}

fn query<T: Default>(pid: u32, flavor: c_int) -> io::Result<T> {
    let mut info = T::default();
    let ret = unsafe {
        proc_pidinfo(
            pid as i32,
            flavor,
            0,
            (&mut info as *mut T).cast(),
            size_of::<T>() as c_int,
        )
    };
    if ret <= 0 {
        Err(io::Error::last_os_error())
    } else if ret as usize != size_of::<T>() {
        Err(io::Error::from_raw_os_error(libc::EIO))
    } else {
        Ok(info)
    }
}

pub(crate) fn owner(pid: u32) -> io::Result<(u32, u32)> {
    let (mut uid, mut gid) = (0, 0);
    let result = unsafe { procfs_os_owner(pid, &mut uid, &mut gid) };
    if result < 0 {
        return Err(io::Error::from_raw_os_error(-result));
    }
    Ok((uid, gid))
}

fn comm(info: &ProcBsdInfo) -> &[u8] {
    // Match Linux TASK_COMM_LEN: 15 bytes of name plus a terminating NUL.
    let end = info
        .pbi_comm
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(MAXCOMLEN)
        .min(15);
    &info.pbi_comm[..end]
}

fn state(info: &ProcBsdInfo) -> (char, &'static str) {
    match info.pbi_status {
        1 => ('I', "idle"),
        2 => ('R', "running"),
        3 => ('S', "sleeping"),
        4 => ('T', "stopped"),
        5 => ('Z', "zombie"),
        _ => ('?', "unknown"),
    }
}

pub(crate) fn get_comm(pid: u32) -> io::Result<Vec<u8>> {
    let info: ProcBsdInfo = query(pid, 3)?;
    let mut name = comm(&info).to_vec();
    name.push(b'\n');
    Ok(name)
}

pub(crate) fn get_status(pid: u32) -> io::Result<Vec<u8>> {
    let info: ProcTaskAllInfo = query(pid, PROC_PIDTASKALLINFO)?;
    Ok(render_status(&info))
}

fn render_status(info: &ProcTaskAllInfo) -> Vec<u8> {
    render_status_for(info, u64::from(info.pbsd.pbi_pid))
}

fn render_status_for(info: &ProcTaskAllInfo, tid: u64) -> Vec<u8> {
    let b = &info.pbsd;
    let t = &info.ptinfo;
    let (state, description) = state(b);
    let mut output = b"Name:\t".to_vec();
    for &byte in comm(b) {
        match byte {
            b'\\' => output.extend_from_slice(b"\\\\"),
            b'\n' => output.extend_from_slice(b"\\n"),
            b'\t' => output.extend_from_slice(b"\\t"),
            _ => output.push(byte),
        }
    }
    // Unsupported Linux-only keys are omitted instead of inventing values.
    output.extend_from_slice(
        format!(
            "\nState:\t{state} ({description})\nTgid:\t{pid}\nPid:\t{tid}\nPPid:\t{ppid}\n\
         Uid:\t{ruid}\t{uid}\t{svuid}\t{uid}\nGid:\t{rgid}\t{gid}\t{svgid}\t{gid}\n\
         VmSize:\t{vsize} kB\nVmRSS:\t{rss} kB\nThreads:\t{threads}\n",
            pid = b.pbi_pid,
            ppid = b.pbi_ppid,
            ruid = b.pbi_ruid,
            uid = b.pbi_uid,
            svuid = b.pbi_svuid,
            rgid = b.pbi_rgid,
            gid = b.pbi_gid,
            svgid = b.pbi_svgid,
            vsize = t.pti_virtual_size / 1024,
            rss = t.pti_resident_size / 1024,
            threads = t.pti_threadnum,
        )
        .as_bytes(),
    );
    output
}

pub(crate) fn get_statm(pid: u32) -> io::Result<Vec<u8>> {
    let info: ProcTaskAllInfo = query(pid, PROC_PIDTASKALLINFO)?;
    let page_size = sysconf_positive(libc::_SC_PAGESIZE)?;
    Ok(format!(
        "{} {} 0 0 0 0 0\n",
        info.ptinfo.pti_virtual_size / page_size,
        info.ptinfo.pti_resident_size / page_size
    )
    .into_bytes())
}

/// Linux's 52-field stat layout; unavailable platform-specific fields are zero.
pub(crate) fn get_stat(pid: u32) -> io::Result<Vec<u8>> {
    let info: ProcTaskAllInfo = query(pid, PROC_PIDTASKALLINFO)?;
    let hz = sysconf_positive(libc::_SC_CLK_TCK)?;
    let page_size = sysconf_positive(libc::_SC_PAGESIZE)?;
    let frequency = timebase_frequency()?;
    let boot = boot_time()?;
    let session = unsafe { libc::getsid(pid as libc::pid_t) };
    Ok(render_stat(
        &info,
        hz,
        page_size,
        frequency,
        boot,
        session.max(0),
    ))
}

fn render_stat(
    info: &ProcTaskAllInfo,
    hz: u64,
    page_size: u64,
    frequency: u64,
    boot: libc::timeval,
    session: i32,
) -> Vec<u8> {
    let b = &info.pbsd;
    let t = &info.ptinfo;
    // Index 0 is field 4 (ppid); keeping all 49 numeric fields explicit in
    // one fixed-size array prevents later additions from shifting positions.
    let mut fields = [0i128; 49];
    fields[0] = b.pbi_ppid.into();
    fields[1] = b.pbi_pgid.into();
    fields[2] = session.into();
    fields[6] = i128::from(t.pti_faults)
        .saturating_sub(i128::from(t.pti_pageins))
        .max(0);
    fields[8] = i128::from(t.pti_pageins).max(0);
    fields[10] = cpu_ticks(t.pti_total_user, frequency, hz).into();
    fields[11] = cpu_ticks(t.pti_total_system, frequency, hz).into();
    // Approximate normal Linux scheduling priority from nice; Darwin's raw
    // priority numbers belong to a different scheduler and aren't compatible.
    fields[14] = i128::from(b.pbi_nice) + 20;
    fields[15] = b.pbi_nice.into();
    fields[16] = t.pti_threadnum.into();
    fields[18] = start_ticks(b.pbi_start_tvsec, b.pbi_start_tvusec, boot, hz).into();
    fields[19] = t.pti_virtual_size.into();
    fields[20] = (t.pti_resident_size / page_size).into();
    let mut output = format!("{} (", b.pbi_pid).into_bytes();
    output.extend_from_slice(comm(b));
    output.extend_from_slice(format!(") {}", state(b).0).as_bytes());
    for field in fields {
        output.extend_from_slice(format!(" {field}").as_bytes());
    }
    output.push(b'\n');
    output
}

fn start_ticks(seconds: u64, micros: u64, boot: libc::timeval, hz: u64) -> u64 {
    let started = i128::from(seconds) * 1_000_000 + i128::from(micros);
    let booted = i128::from(boot.tv_sec) * 1_000_000 + i128::from(boot.tv_usec);
    (((started - booted).max(0) as u128) * u128::from(hz) / 1_000_000) as u64
}

unsafe extern "C" {
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
    // Implemented with the SDK's vnode structs in the C adapter to avoid
    // duplicating their large, platform-specific layout in Rust.
    fn procfs_os_cwd(pid: u32, buffer: *mut u8, size: usize) -> c_int;
    fn procfs_os_owner(pid: u32, uid: *mut u32, gid: *mut u32) -> c_int;
}

pub(crate) fn get_exe(pid: u32) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; 4096]; // PROC_PIDPATHINFO_MAXSIZE
    let result =
        unsafe { proc_pidpath(pid as i32, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if result <= 0 {
        return Err(io::Error::last_os_error());
    }
    let end = buffer
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
    buffer.truncate(end);
    Ok(buffer)
}

pub(crate) fn get_cwd(pid: u32) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    let result = unsafe { procfs_os_cwd(pid, buffer.as_mut_ptr(), buffer.len()) };
    if result < 0 {
        return Err(io::Error::from_raw_os_error(-result));
    }
    buffer.truncate(result as usize);
    Ok(buffer)
}

/// Build /<pid>/cmdline content (null-separated argv, Linux convention).
///
/// sysctl KERN_PROCARGS2 layout:
///   [4-byte argc LE][exec_path\0][null padding][arg0\0][arg1\0]...
fn procargs(pid: u32) -> io::Result<Vec<u8>> {
    unsafe {
        let mut mib: [c_int; 3] = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as c_int];
        let mut size: size_t = 0;
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut buf: Vec<u8> = vec![0u8; size];
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(size);

        Ok(buf)
    }
}

fn split_procargs(buf: &[u8]) -> io::Result<(&[u8], &[u8])> {
    let invalid = || io::Error::from_raw_os_error(libc::EIO);
    let argc = i32::from_ne_bytes(buf.get(..4).ok_or_else(invalid)?.try_into().unwrap());
    if argc < 0 {
        return Err(invalid());
    }
    let rest = &buf[4..];
    let mut pos = rest.iter().position(|&b| b == 0).ok_or_else(invalid)? + 1;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    let start = pos;
    for _ in 0..argc {
        let length = rest
            .get(pos..)
            .ok_or_else(invalid)?
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(invalid)?;
        pos += length + 1;
    }
    let argv = &rest[start..pos];
    let start = pos;
    while pos < rest.len() && rest[pos] != 0 {
        let length = rest[pos..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(invalid)?;
        pos += length + 1;
    }
    Ok((argv, &rest[start..pos]))
}

pub(crate) fn get_cmdline(pid: u32) -> io::Result<Vec<u8>> {
    Ok(split_procargs(&procargs(pid)?)?.0.to_vec())
}

pub(crate) fn get_environ(pid: u32) -> io::Result<Vec<u8>> {
    Ok(split_procargs(&procargs(pid)?)?.1.to_vec())
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct FdInfo {
    pub fd: i32,
    pub kind: u32,
}

// LISTTHREADIDS is in XNU's proc_info_private.h, not this host's older SDK.
// Unlike LISTTHREADS (pthread handles), it returns pthread_threadid_np IDs.
const PROC_PIDLISTTHREADIDS: c_int = 28;

fn list_info<T: Copy + Default>(pid: u32, flavor: c_int, initial: usize) -> io::Result<Vec<T>> {
    let mut capacity = initial.max(16);
    for _ in 0..12 {
        let bytes = capacity
            .checked_mul(size_of::<T>())
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| io::Error::from_raw_os_error(libc::E2BIG))?;
        let mut entries = vec![T::default(); capacity];
        let count = unsafe {
            *libc::__error() = 0;
            proc_pidinfo(pid as i32, flavor, 0, entries.as_mut_ptr().cast(), bytes)
        };
        if count < 0 || (count == 0 && unsafe { *libc::__error() } != 0) {
            return Err(io::Error::last_os_error());
        }
        if count as usize > bytes as usize || !(count as usize).is_multiple_of(size_of::<T>()) {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        if count < bytes {
            entries.truncate(count as usize / size_of::<T>());
            return Ok(entries);
        }
        capacity *= 2;
    }
    Err(io::Error::from_raw_os_error(libc::EAGAIN))
}

pub(crate) fn list_fds(pid: u32) -> io::Result<Vec<FdInfo>> {
    // Check permissions separately: zero bytes can also mean an empty table.
    let info: ProcBsdInfo = query(pid, 3)?;
    let mut entries: Vec<FdInfo> = list_info(pid, 1, info.pbi_nfiles as usize + 16)?;
    entries.retain(|entry| entry.fd >= 0);
    entries.sort_by_key(|entry| entry.fd);
    entries.dedup_by_key(|entry| entry.fd);
    Ok(entries)
}

pub(crate) fn list_threads(pid: u32) -> io::Result<Vec<u64>> {
    let info: ProcTaskInfo = query(pid, 4)?;
    let mut entries: Vec<u64> = list_info(
        pid,
        PROC_PIDLISTTHREADIDS,
        info.pti_threadnum.max(0) as usize + 16,
    )?;
    entries.retain(|&tid| tid != 0);
    entries.sort_unstable();
    entries.dedup();
    Ok(entries)
}

#[repr(C)]
struct ThreadInfo {
    user: u64,
    system: u64,
    cpu_usage: i32,
    policy: i32,
    state: i32,
    flags: i32,
    sleep_time: i32,
    curpri: i32,
    priority: i32,
    maxpriority: i32,
    name: [u8; 64],
}

fn thread_info(pid: u32, tid: u64) -> io::Result<ThreadInfo> {
    let mut info: ThreadInfo = unsafe { std::mem::zeroed() };
    let result = unsafe {
        proc_pidinfo(
            pid as i32,
            15,
            tid,
            (&mut info as *mut ThreadInfo).cast(),
            size_of::<ThreadInfo>() as i32,
        )
    };
    if result <= 0 {
        let error = io::Error::last_os_error();
        // XNU returns ESRCH for a vanished thread, EINVAL on some versions.
        if error.raw_os_error() == Some(libc::EINVAL) && !list_threads(pid)?.contains(&tid) {
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        }
        return Err(error);
    }
    if result as usize != size_of::<ThreadInfo>() {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(info)
}

pub(crate) fn check_thread(pid: u32, tid: u64) -> io::Result<()> {
    thread_info(pid, tid).map(|_| ())
}

pub(crate) fn get_thread_file(pid: u32, tid: u64, name: &str) -> io::Result<Vec<u8>> {
    let thread = thread_info(pid, tid)?;
    let mut process: ProcTaskAllInfo = query(pid, PROC_PIDTASKALLINFO)?;
    let end = thread
        .name
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(64)
        .min(15);
    if end != 0 {
        process.pbsd.pbi_comm.fill(0);
        process.pbsd.pbi_comm[..end].copy_from_slice(&thread.name[..end]);
    }
    process.pbsd.pbi_status = match thread.state {
        1 => 2,
        2 | 5 => 4,
        3 | 4 => 3,
        _ => 0,
    };
    match name {
        "comm" => {
            let mut output = comm(&process.pbsd).to_vec();
            output.push(b'\n');
            Ok(output)
        }
        "status" => Ok(render_status_for(&process, tid)),
        "stat" => {
            let hz = sysconf_positive(libc::_SC_CLK_TCK)?;
            let mut fields = [0u64; 49];
            fields[0] = u64::from(process.pbsd.pbi_ppid);
            fields[1] = u64::from(process.pbsd.pbi_pgid);
            fields[2] = unsafe { libc::getsid(pid as i32) }.max(0) as u64;
            // Unlike task totals, proc_threadinfo CPU times are nanoseconds.
            fields[10] = cpu_ticks(thread.user, 1_000_000_000, hz);
            fields[11] = cpu_ticks(thread.system, 1_000_000_000, hz);
            fields[16] = process.ptinfo.pti_threadnum.max(0) as u64;
            fields[19] = process.ptinfo.pti_virtual_size;
            fields[20] = process.ptinfo.pti_resident_size / sysconf_positive(libc::_SC_PAGESIZE)?;
            let mut output = format!("{tid} (").into_bytes();
            output.extend_from_slice(comm(&process.pbsd));
            output.extend_from_slice(format!(") {}", state(&process.pbsd).0).as_bytes());
            for field in fields {
                output.extend_from_slice(format!(" {field}").as_bytes());
            }
            output.push(b'\n');
            Ok(output)
        }
        _ => Err(io::Error::from_raw_os_error(libc::ENOENT)),
    }
}

pub(crate) fn get_limits(pid: u32) -> io::Result<Vec<u8>> {
    if pid != std::process::id() {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    let mut output = format!(
        "{:<26}{:<21}{:<21}Units\n",
        "Limit", "Soft Limit", "Hard Limit"
    );
    for (name, resource, unit) in [
        ("Max cpu time", libc::RLIMIT_CPU, "seconds"),
        ("Max file size", libc::RLIMIT_FSIZE, "bytes"),
        ("Max data size", libc::RLIMIT_DATA, "bytes"),
        ("Max stack size", libc::RLIMIT_STACK, "bytes"),
        ("Max core file size", libc::RLIMIT_CORE, "bytes"),
        ("Max resident set", libc::RLIMIT_RSS, "bytes"),
        ("Max processes", libc::RLIMIT_NPROC, "processes"),
        ("Max open files", libc::RLIMIT_NOFILE, "files"),
        ("Max locked memory", libc::RLIMIT_MEMLOCK, "bytes"),
    ] {
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrlimit(resource, &mut limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let value = |n: libc::rlim_t| {
            if n == libc::RLIM_INFINITY {
                "unlimited".to_owned()
            } else {
                n.to_string()
            }
        };
        output.push_str(&format!(
            "{name:<26}{:<21}{:<21}{unit}\n",
            value(limit.rlim_cur),
            value(limit.rlim_max)
        ));
    }
    Ok(output.into_bytes())
}

fn timebase_frequency() -> io::Result<u64> {
    let mut frequency: u64 = 0;
    let mut size = size_of::<u64>();
    let result = unsafe {
        libc::sysctlbyname(
            c"hw.tbfrequency".as_ptr(),
            (&mut frequency as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if size != size_of::<u64>() || frequency == 0 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(frequency)
}

fn cpu_ticks(mach_ticks: u64, frequency: u64, hz: u64) -> u64 {
    (u128::from(mach_ticks) * u128::from(hz) / u128::from(frequency)) as u64
}

#[repr(C)]
struct Region {
    start: u64,
    end: u64,
    offset: u64,
    inode: u64,
    major: u32,
    minor: u32,
    protection: u32,
    shared: u32,
}

type RegionCallback = unsafe extern "C" fn(*mut c_void, *const Region, *const c_char);
unsafe extern "C" {
    fn procfs_os_fdtarget(pid: u32, fd: c_int, kind: u32, buffer: *mut u8, size: usize) -> c_int;
    fn procfs_os_io(pid: u32, counters: *mut u64) -> c_int;
    fn procfs_os_maps(pid: u32, context: *mut c_void, fill: RegionCallback) -> c_int;
}

pub(crate) fn fd_target(pid: u32, fd: u32) -> io::Result<Vec<u8>> {
    let entry = list_fds(pid)?
        .into_iter()
        .find(|entry| entry.fd == fd as i32)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
    let mut bytes = vec![0; libc::PATH_MAX as usize];
    let size = checked(unsafe {
        procfs_os_fdtarget(pid, entry.fd, entry.kind, bytes.as_mut_ptr(), bytes.len())
    })?;
    bytes.truncate(size);
    Ok(bytes)
}

pub(crate) fn get_io(pid: u32) -> io::Result<Vec<u8>> {
    let mut counters = [0; 2];
    checked(unsafe { procfs_os_io(pid, counters.as_mut_ptr()) })?;
    // Darwin does not expose Linux rchar/wchar/syscr/syscw/cancelled_write_bytes.
    Ok(format!(
        "read_bytes: {}\nwrite_bytes: {}\n",
        counters[0], counters[1]
    )
    .into_bytes())
}

fn map_line(region: &Region, path: &[u8], output: &mut Vec<u8>) {
    output.extend_from_slice(
        format!(
            "{:x}-{:x} {}{}{}{} {:08x} {:02x}:{:02x} {}",
            region.start,
            region.end,
            if region.protection & 1 != 0 { 'r' } else { '-' },
            if region.protection & 2 != 0 { 'w' } else { '-' },
            if region.protection & 4 != 0 { 'x' } else { '-' },
            if region.shared != 0 { 's' } else { 'p' },
            region.offset,
            region.major,
            region.minor,
            region.inode,
        )
        .as_bytes(),
    );
    if !path.is_empty() {
        output.push(b' ');
        for &byte in path {
            if byte == b'\n' {
                output.extend_from_slice(br"\012");
            } else {
                output.push(byte);
            }
        }
    }
    output.push(b'\n');
}

unsafe extern "C" fn region_entry(
    context: *mut c_void,
    region: *const Region,
    path: *const c_char,
) {
    let output = unsafe { &mut *context.cast::<Vec<u8>>() };
    map_line(
        unsafe { &*region },
        unsafe { CStr::from_ptr(path) }.to_bytes(),
        output,
    );
}

pub(crate) fn get_maps(pid: u32) -> io::Result<Vec<u8>> {
    let mut output = Vec::<u8>::new();
    checked(unsafe { procfs_os_maps(pid, (&mut output as *mut Vec<u8>).cast(), region_entry) })?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procargs_separates_empty_arguments_from_environment_and_rejects_truncation() {
        let mut bytes = 3i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/example\0\0\0example\0\0last\0MARKER=a=b\0EMPTY=\0\0\0");
        let (argv, env) = split_procargs(&bytes).unwrap();
        assert_eq!(argv, b"example\0\0last\0");
        assert_eq!(env, b"MARKER=a=b\0EMPTY=\0");
        for data in [&b""[..], &bytes[..6], &bytes[..25]] {
            assert!(split_procargs(data).is_err());
        }
    }

    #[test]
    fn cpu_time_uses_hardware_ticks_on_apple_silicon_and_intel() {
        assert_eq!(cpu_ticks(48_000_000, 24_000_000, 100), 200);
        assert_eq!(cpu_ticks(2_000_000_000, 1_000_000_000, 100), 200);
        assert_eq!(cpu_ticks(24_000_000, 24_000_000, 100), 100);
    }

    #[test]
    fn stat_layout_preserves_linux_positions_and_native_units() {
        let mut info = ProcTaskAllInfo::default();
        let b = &mut info.pbsd;
        b.pbi_pid = 42;
        b.pbi_ppid = 7;
        b.pbi_pgid = 8;
        b.pbi_status = 3;
        b.pbi_nice = -5;
        b.pbi_start_tvsec = 1002;
        b.pbi_start_tvusec = 100_000;
        b.pbi_comm[..13].copy_from_slice(b"weird )\nname!");
        let t = &mut info.ptinfo;
        t.pti_total_user = 24_000_000;
        t.pti_total_system = 12_000_000;
        t.pti_virtual_size = 32768;
        t.pti_resident_size = 16384;
        t.pti_faults = 20;
        t.pti_pageins = 3;
        t.pti_threadnum = 4;
        let boot = libc::timeval {
            tv_sec: 1000,
            tv_usec: 900_000,
        };
        let output = render_stat(&info, 100, 16384, 24_000_000, boot, 9);
        let line = String::from_utf8(output).unwrap();
        assert!(line.starts_with("42 (weird )\nname!) S "));
        let fields: Vec<_> = line
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        assert_eq!(fields.len() + 2, 52);
        for (number, expected) in [
            (4, "7"),
            (5, "8"),
            (6, "9"),
            (10, "17"),
            (12, "3"),
            (14, "100"),
            (15, "50"),
            (18, "15"),
            (19, "-5"),
            (20, "4"),
            (22, "120"),
            (23, "32768"),
            (24, "1"),
            (52, "0"),
        ] {
            assert_eq!(fields[number - 3], expected, "stat field {number}");
        }
        assert_eq!(start_ticks(999, 0, boot, 100), 0);
    }

    #[test]
    fn status_escapes_names_and_preserves_identity_columns() {
        let mut info = ProcTaskAllInfo::default();
        info.pbsd.pbi_comm[..5].copy_from_slice(b"a\n\\\tb");
        info.pbsd.pbi_pid = 42;
        info.pbsd.pbi_uid = 502;
        info.pbsd.pbi_ruid = 501;
        info.pbsd.pbi_svuid = 503;
        info.pbsd.pbi_gid = 21;
        info.pbsd.pbi_rgid = 20;
        info.pbsd.pbi_svgid = 22;
        info.ptinfo.pti_virtual_size = 32768;
        info.ptinfo.pti_resident_size = 16384;
        let text = String::from_utf8(render_status(&info)).unwrap();
        assert!(text.starts_with("Name:\ta\\n\\\\\\tb\n"));
        assert!(text.contains("Uid:\t501\t502\t503\t502\n"));
        assert!(text.contains("Gid:\t20\t21\t22\t21\n"));
        assert!(text.contains("VmSize:\t32 kB\nVmRSS:\t16 kB\n"));
    }
    #[test]
    fn maps_preserve_spaces_and_escape_newlines() {
        let mut row = Vec::new();
        map_line(
            &Region {
                start: 0x1000,
                end: 0x2000,
                offset: 0x3000,
                inode: 42,
                major: 1,
                minor: 2,
                protection: 3,
                shared: 0,
            },
            b"/a b\nc",
            &mut row,
        );
        assert_eq!(row, b"1000-2000 rw-p 00003000 01:02 42 /a b\\012c\n");
        assert_eq!(size_of::<Region>(), 48);
    }
}

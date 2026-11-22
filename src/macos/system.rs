//! Linux-shaped system files backed by Darwin counters; see COMPATIBILITY.md.
use super::checked;
use libc::{c_char, c_int, c_void};
use std::ffi::CStr;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u64)]
pub(crate) enum SystemFile {
    Stat = 10,
    Meminfo,
    Uptime,
    Loadavg,
    Cpuinfo,
    Version,
    Mounts,
    Diskstats,
}

pub(crate) const FILES: &[(&str, SystemFile)] = &[
    ("stat", SystemFile::Stat),
    ("meminfo", SystemFile::Meminfo),
    ("uptime", SystemFile::Uptime),
    ("loadavg", SystemFile::Loadavg),
    ("cpuinfo", SystemFile::Cpuinfo),
    ("version", SystemFile::Version),
    ("mounts", SystemFile::Mounts),
    ("diskstats", SystemFile::Diskstats),
];

#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    inactive: u64,
    active: u64,
    wired: u64,
    file_backed: u64,
    swap_total: u64,
    swap_free: u64,
}

type MountCallback =
    unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char, u32);
unsafe extern "C" {
    fn procfs_os_memory(memory: *mut Memory) -> c_int;
    fn procfs_os_cpu(states: *mut u64, slots: usize) -> c_int;
    fn procfs_os_mounts(context: *mut c_void, fill: MountCallback) -> c_int;
}

fn cpus() -> io::Result<Vec<[u64; 4]>> {
    let count = checked(unsafe { procfs_os_cpu(std::ptr::null_mut(), 0) })?;
    if count == 0 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    let mut data = vec![[0; 4]; count];
    let actual = checked(unsafe { procfs_os_cpu(data.as_mut_ptr().cast(), count * 4) })?;
    data.truncate(actual);
    Ok(data)
}

fn cpu_line(name: &str, values: [u64; 4]) -> String {
    format!(
        "{name} {} {} {} {} 0 0 0 0 0 0\n",
        values[0], values[1], values[2], values[3]
    )
}

fn stat() -> io::Result<Vec<u8>> {
    let cpus = cpus()?;
    let mut sum = [0; 4];
    for cpu in &cpus {
        for i in 0..4 {
            sum[i] += cpu[i];
        }
    }
    let mut output = cpu_line("cpu", sum);
    for (i, cpu) in cpus.into_iter().enumerate() {
        output.push_str(&cpu_line(&format!("cpu{i}"), cpu));
    }
    output.push_str(&format!("btime {}\n", super::boot_time()?.tv_sec));
    Ok(output.into_bytes())
}

fn meminfo() -> io::Result<Vec<u8>> {
    let mut memory = Memory::default();
    checked(unsafe { procfs_os_memory(&mut memory) })?;
    Ok(render_memory(&memory))
}

fn render_memory(m: &Memory) -> Vec<u8> {
    let fields = [
        ("MemTotal", m.total),
        ("MemFree", m.free),
        (
            "MemAvailable",
            m.free.saturating_add(m.inactive).min(m.total),
        ),
        ("Cached", m.file_backed),
        ("Active", m.active),
        ("Inactive", m.inactive),
        ("Unevictable", m.wired),
        ("SwapTotal", m.swap_total),
        ("SwapFree", m.swap_free),
    ];
    let mut output = String::new();
    for (name, value) in fields {
        output.push_str(&format!("{name}:\t{} kB\n", value / 1024));
    }
    output.into_bytes()
}

fn uptime() -> io::Result<Vec<u8>> {
    let boot = super::boot_time()?;
    let boot_seconds = boot.tv_sec as f64 + boot.tv_usec as f64 / 1_000_000.;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs_f64();
    let idle_ticks: u64 = cpus()?.iter().map(|cpu| cpu[3]).sum();
    let hz = super::sysconf_positive(libc::_SC_CLK_TCK)?;
    Ok(format!(
        "{:.2} {:.2}\n",
        (now - boot_seconds).max(0.),
        idle_ticks as f64 / hz as f64
    )
    .into_bytes())
}

fn loadavg() -> io::Result<Vec<u8>> {
    let mut averages = [0f64; 3];
    if unsafe { libc::getloadavg(averages.as_mut_ptr(), 3) } != 3 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    // The Linux runnable-task counts and last-assigned PID have no equivalent
    // in getloadavg. Preserve their positions with documented zero placeholders.
    Ok(format!(
        "{:.2} {:.2} {:.2} 0/0 0\n",
        averages[0], averages[1], averages[2]
    )
    .into_bytes())
}

fn sysctl_text(name: &CStr) -> io::Result<Vec<u8>> {
    let mut size = 0;
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut bytes = vec![0u8; size];
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    bytes.truncate(size);
    if let Some(end) = bytes.iter().position(|&b| b == 0) {
        bytes.truncate(end);
    }
    Ok(bytes)
}

fn cpuinfo() -> io::Result<Vec<u8>> {
    let model = sysctl_text(c"machdep.cpu.brand_string").or_else(|_| sysctl_text(c"hw.model"))?;
    let mut output = Vec::new();
    for cpu in 0..super::sysconf_positive(libc::_SC_NPROCESSORS_ONLN)? {
        output.extend_from_slice(format!("processor\t: {cpu}\nmodel name\t: ").as_bytes());
        output.extend_from_slice(&model);
        output.extend_from_slice(b"\n\n");
    }
    Ok(output)
}

fn escape_mount(bytes: &[u8], output: &mut Vec<u8>) {
    for &byte in bytes {
        match byte {
            b' ' => output.extend_from_slice(b"\\040"),
            b'\t' => output.extend_from_slice(b"\\011"),
            b'\n' => output.extend_from_slice(b"\\012"),
            b'\\' => output.extend_from_slice(b"\\134"),
            _ => output.push(byte),
        }
    }
}

unsafe extern "C" fn mount_entry(
    context: *mut c_void,
    source: *const c_char,
    target: *const c_char,
    kind: *const c_char,
    flags: u32,
) {
    let output = unsafe { &mut *context.cast::<Vec<u8>>() };
    for value in [source, target, kind] {
        escape_mount(unsafe { CStr::from_ptr(value) }.to_bytes(), output);
        output.push(b' ');
    }
    output.extend_from_slice(if flags & 1 != 0 { b"ro" } else { b"rw" });
    for (bit, option) in [
        (2, ",nosuid"),
        (4, ",nodev"),
        (8, ",noexec"),
        (16, ",noatime"),
        (32, ",sync"),
        (64, ",async"),
    ] {
        if flags & bit != 0 {
            output.extend_from_slice(option.as_bytes());
        }
    }
    output.extend_from_slice(b" 0 0\n");
}

fn mounts() -> io::Result<Vec<u8>> {
    let mut output = Vec::<u8>::new();
    checked(unsafe { procfs_os_mounts((&mut output as *mut Vec<u8>).cast(), mount_entry) })?;
    Ok(output)
}

pub(crate) fn read(file: SystemFile) -> io::Result<Vec<u8>> {
    match file {
        SystemFile::Stat => stat(),
        SystemFile::Meminfo => meminfo(),
        SystemFile::Uptime => uptime(),
        SystemFile::Loadavg => loadavg(),
        SystemFile::Cpuinfo => cpuinfo(),
        SystemFile::Mounts => mounts(),
        SystemFile::Diskstats => diskstats(),
        SystemFile::Version => {
            let mut version = sysctl_text(c"kern.version")?;
            version.push(b'\n');
            Ok(version)
        }
    }
}

type NetCallback = unsafe extern "C" fn(*mut c_void, *const c_char, *const u64);
type DiskCallback = unsafe extern "C" fn(*mut c_void, *const c_char, u32, u32, *const u64);
unsafe extern "C" {
    fn procfs_os_net(context: *mut c_void, fill: NetCallback) -> c_int;
    fn procfs_os_disks(context: *mut c_void, fill: DiskCallback) -> c_int;
}

unsafe extern "C" fn net_entry(context: *mut c_void, name: *const c_char, counters: *const u64) {
    let rows = unsafe { &mut *context.cast::<Vec<Vec<u8>>>() };
    let mut row = unsafe { CStr::from_ptr(name) }.to_bytes().to_vec();
    row.push(b':');
    for counter in unsafe { std::slice::from_raw_parts(counters, 16) } {
        row.extend_from_slice(format!(" {counter}").as_bytes());
    }
    row.push(b'\n');
    rows.push(row);
}

pub(crate) fn net_dev() -> io::Result<Vec<u8>> {
    let mut rows = Vec::<Vec<u8>>::new();
    checked(unsafe { procfs_os_net((&mut rows as *mut Vec<Vec<u8>>).cast(), net_entry) })?;
    rows.sort();
    let mut output = b"Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n".to_vec();
    output.extend(rows.into_iter().flatten());
    Ok(output)
}

fn disk_line(name: &[u8], major: u32, minor: u32, counters: &[u64]) -> Vec<u8> {
    let mut row = format!("{major} {minor} ").into_bytes();
    row.extend_from_slice(name);
    // Classic Linux diskstats: sectors are always 512 bytes; times are ms.
    row.extend_from_slice(
        format!(
            " {} 0 {} {} {} 0 {} {} 0 0 0\n",
            counters[0],
            counters[2] / 512,
            counters[4] / 1_000_000,
            counters[1],
            counters[3] / 512,
            counters[5] / 1_000_000,
        )
        .as_bytes(),
    );
    row
}

unsafe extern "C" fn disk_entry(
    context: *mut c_void,
    name: *const c_char,
    major: u32,
    minor: u32,
    counters: *const u64,
) {
    let rows = unsafe { &mut *context.cast::<Vec<Vec<u8>>>() };
    rows.push(disk_line(
        unsafe { CStr::from_ptr(name) }.to_bytes(),
        major,
        minor,
        unsafe { std::slice::from_raw_parts(counters, 6) },
    ));
}

pub(crate) fn diskstats() -> io::Result<Vec<u8>> {
    let mut rows = Vec::<Vec<u8>>::new();
    checked(unsafe { procfs_os_disks((&mut rows as *mut Vec<Vec<u8>>).cast(), disk_entry) })?;
    rows.sort();
    Ok(rows.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_uses_kib_and_caps_available_at_total() {
        let memory = Memory {
            total: 32768,
            free: 16384,
            inactive: 32768,
            ..Memory::default()
        };
        let text = String::from_utf8(render_memory(&memory)).unwrap();
        assert!(text.contains("MemTotal:\t32 kB\n"));
        assert!(text.contains("MemAvailable:\t32 kB\n"));
        assert_eq!(std::mem::size_of::<Memory>(), 64);
    }

    #[test]
    fn mounts_escape_whitespace_and_backslashes() {
        let mut output = Vec::new();
        escape_mount(b"/a b\tc\nd\\e", &mut output);
        assert_eq!(output, br"/a\040b\011c\012d\134e");
    }
    #[test]
    fn diskstats_preserves_units_and_field_positions() {
        let row = disk_line(b"disk0", 1, 2, &[3, 4, 4096, 8192, 2_000_000, 3_000_000]);
        assert_eq!(row, b"1 2 disk0 3 0 8 2 4 0 16 3 0 0 0\n");
    }
}

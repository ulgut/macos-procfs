//! Filesystem data independent of the libfuse transport.
use crate::macos::{
    process,
    system::{self, SystemFile},
};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, i32>;
const FILE: u32 = 0;
const DIRECTORY: u32 = 1;
const SYMLINK: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u64)]
enum ProcessFile {
    Stat = 1,
    Cmdline,
    Comm,
    Status,
    Statm,
    Exe,
    Cwd,
    Environ,
    Limits,
    Io,
    Maps,
}

const PROCESS_FILES: &[(&str, ProcessFile)] = &[
    ("stat", ProcessFile::Stat),
    ("cmdline", ProcessFile::Cmdline),
    ("comm", ProcessFile::Comm),
    ("status", ProcessFile::Status),
    ("statm", ProcessFile::Statm),
    ("exe", ProcessFile::Exe),
    ("cwd", ProcessFile::Cwd),
    ("environ", ProcessFile::Environ),
    ("limits", ProcessFile::Limits),
    ("io", ProcessFile::Io),
    ("maps", ProcessFile::Maps),
];

impl ProcessFile {
    fn is_link(self) -> bool {
        matches!(self, Self::Exe | Self::Cwd)
    }

    fn query(self, pid: u32) -> io::Result<Vec<u8>> {
        match self {
            Self::Stat => process::get_stat(pid),
            Self::Cmdline => process::get_cmdline(pid),
            Self::Comm => process::get_comm(pid),
            Self::Status => process::get_status(pid),
            Self::Statm => process::get_statm(pid),
            Self::Exe => process::get_exe(pid),
            Self::Cwd => process::get_cwd(pid),
            Self::Environ => process::get_environ(pid),
            Self::Limits => process::get_limits(pid),
            Self::Io => process::get_io(pid),
            Self::Maps => process::get_maps(pid),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Node {
    Root,
    SelfLink(bool),
    Net,
    NetDev,
    System(SystemFile),
    Process(u32),
    File(u32, ProcessFile),
    Fds(u32),
    Fd(u32, u32),
    Tasks(u32),
    Thread(u32, u64),
    ThreadFile(u32, u64, ProcessFile),
}

impl Node {
    fn parse(path: &CStr) -> Result<Self> {
        let path = path.to_str().map_err(|_| libc::ENOENT)?;
        if path == "/" {
            return Ok(Self::Root);
        }
        match path {
            "/self" => return Ok(Self::SelfLink(false)),
            "/thread-self" => return Ok(Self::SelfLink(true)),
            "/net" => return Ok(Self::Net),
            "/net/dev" => return Ok(Self::NetDev),
            _ => (),
        }
        if let Some((_, file)) = system::FILES
            .iter()
            .find(|(name, _)| path.strip_prefix('/') == Some(*name))
        {
            return Ok(Self::System(*file));
        }
        let mut parts = path.strip_prefix('/').ok_or(libc::ENOENT)?.split('/');
        let name = parts.next().ok_or(libc::ENOENT)?;
        let pid = name.parse::<u32>().map_err(|_| libc::ENOENT)?;
        if pid == 0 || pid > i32::MAX as u32 || name != pid.to_string() {
            return Err(libc::ENOENT);
        }
        let node = match parts.next() {
            None => Self::Process(pid),
            Some("fd") => match parts.next() {
                None => Self::Fds(pid),
                Some(fd) => Self::Fd(pid, canonical_number(fd, i32::MAX as u64)? as u32),
            },
            Some("task") => match parts.next() {
                None => Self::Tasks(pid),
                Some(tid) => {
                    let tid = canonical_number(tid, u64::MAX)?;
                    if tid == 0 {
                        return Err(libc::ENOENT);
                    }
                    match parts.next() {
                        None => Self::Thread(pid, tid),
                        Some("stat") => Self::ThreadFile(pid, tid, ProcessFile::Stat),
                        Some("status") => Self::ThreadFile(pid, tid, ProcessFile::Status),
                        Some("comm") => Self::ThreadFile(pid, tid, ProcessFile::Comm),
                        _ => return Err(libc::ENOENT),
                    }
                }
            },
            Some(name) => {
                let kind = PROCESS_FILES
                    .iter()
                    .find(|(entry, _)| *entry == name)
                    .map(|(_, kind)| *kind)
                    .ok_or(libc::ENOENT)?;
                Self::File(pid, kind)
            }
        };
        if parts.next().is_some() {
            return Err(libc::ENOENT);
        }
        Ok(node)
    }

    fn ino(self) -> u64 {
        if self == Self::Root {
            return 1;
        }
        // Intern full keys: 64-bit native TIDs cannot safely be packed together
        // with PIDs and file kinds into a 64-bit inode using fixed multipliers.
        static INODES: OnceLock<Mutex<HashMap<Node, u64>>> = OnceLock::new();
        let mut ids = INODES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let next = ids.len() as u64 + 2;
        *ids.entry(self).or_insert(next)
    }

    fn pid(self) -> Option<u32> {
        match self {
            Self::Process(pid)
            | Self::File(pid, _)
            | Self::Fds(pid)
            | Self::Fd(pid, _)
            | Self::Tasks(pid)
            | Self::Thread(pid, _)
            | Self::ThreadFile(pid, _, _) => Some(pid),
            _ => None,
        }
    }

    fn kind(self) -> u32 {
        match self {
            Self::Root
            | Self::Process(_)
            | Self::Fds(_)
            | Self::Tasks(_)
            | Self::Thread(_, _)
            | Self::Net => DIRECTORY,
            Self::SelfLink(_) | Self::Fd(_, _) => SYMLINK,
            Self::File(_, kind) if kind.is_link() => SYMLINK,
            _ => FILE,
        }
    }

    fn data(self) -> Result<Vec<u8>> {
        if self.kind() == SYMLINK {
            return Err(libc::ELOOP);
        }
        if let Self::System(file) = self {
            return system::read(file).map_err(errno);
        }
        if self == Self::NetDev {
            return system::net_dev().map_err(errno);
        }
        if let Self::ThreadFile(pid, tid, kind) = self {
            let name = match kind {
                ProcessFile::Stat => "stat",
                ProcessFile::Comm => "comm",
                _ => "status",
            };
            return process_query(pid, || process::get_thread_file(pid, tid, name));
        }
        let Self::File(pid, kind) = self else {
            return Err(libc::EISDIR);
        };
        if kind.is_link() {
            return Err(libc::ELOOP);
        }
        process_query(pid, || kind.query(pid))
    }
}

fn canonical_number(name: &str, maximum: u64) -> Result<u64> {
    let number = name.parse::<u64>().map_err(|_| libc::ENOENT)?;
    if number > maximum || name != number.to_string() {
        return Err(libc::ENOENT);
    }
    Ok(number)
}

fn process_query<T>(pid: u32, query: impl FnOnce() -> io::Result<T>) -> Result<T> {
    if !process::pid_exists(pid) {
        return Err(libc::ENOENT);
    }
    // KERN_PROCARGS2 may report EINVAL if a process disappears during a query.
    query().map_err(|error| {
        if process::pid_exists(pid) {
            errno(error)
        } else {
            libc::ENOENT
        }
    })
}

fn errno(err: io::Error) -> i32 {
    match err.raw_os_error().unwrap_or(libc::EIO) {
        value if value <= 0 => libc::EIO,
        libc::ESRCH => libc::ENOENT,
        value => value,
    }
}

/// Fixed-width C ABI shared with bridge/fuse.h.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Attr {
    pub ino: u64,
    pub size: u64,
    pub mtime_sec: u64,
    pub mtime_nsec: u32,
    pub kind: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Attr {
    fn new(node: Node, size: usize, kind: u32) -> Result<Self> {
        let (uid, gid) = match node.pid() {
            None => unsafe { (libc::geteuid(), libc::getegid()) },
            Some(pid) => process_query(pid, || process::owner(pid))?,
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Ok(Self {
            ino: node.ino(),
            size: size as u64,
            mtime_sec: now.as_secs(),
            mtime_nsec: now.subsec_nanos(),
            kind,
            uid,
            gid,
        })
    }
}

pub(crate) fn getattr(path: &CStr) -> Result<Attr> {
    let node = Node::parse(path)?;
    if let Node::Thread(pid, tid) = node {
        process_query(pid, || process::check_thread(pid, tid))?;
    }
    match node.kind() {
        DIRECTORY => Attr::new(node, 0, DIRECTORY),
        // A zero-length symlink is returned by FSKit as an empty target without
        // asking READLINK. Propagate unavailable/denied targets at lookup.
        SYMLINK => Attr::new(node, readlink(path)?.len(), SYMLINK),
        _ => Attr::new(node, metadata_size(node.data())?, FILE),
    }
}

fn metadata_size(data: Result<Vec<u8>>) -> Result<usize> {
    match data {
        Ok(data) => Ok(data.len()),
        // File names and public ownership remain visible when Darwin denies
        // their contents. Open/readlink still return the real permission error.
        Err(libc::EACCES | libc::EPERM | libc::ENOTSUP) => Ok(0),
        Err(error) => Err(error),
    }
}

pub(crate) fn readlink(path: &CStr) -> Result<Vec<u8>> {
    match Node::parse(path)? {
        Node::SelfLink(_) => Err(libc::ENOTSUP),
        Node::Fd(pid, fd) => process_query(pid, || process::fd_target(pid, fd)),
        Node::File(pid, kind) if kind.is_link() => process_query(pid, || kind.query(pid)),
        _ => Err(libc::EINVAL),
    }
}

/// One FUSE file handle retains one immutable sample. Reopening collects a new
/// sample, so partial/out-of-order reads cannot mix data from different samples.
#[derive(Debug)]
pub(crate) struct OpenFile {
    pub attr: Attr,
    data: Vec<u8>,
}

impl OpenFile {
    pub fn open(path: &CStr) -> Result<Self> {
        let node = Node::parse(path)?;
        let data = node.data()?;
        Ok(Self {
            attr: Attr::new(node, data.len(), FILE)?,
            data,
        })
    }

    pub fn read(&self, offset: i64, size: usize) -> Result<&[u8]> {
        let offset = usize::try_from(offset).map_err(|_| libc::EINVAL)?;
        let start = offset.min(self.data.len());
        let end = start.saturating_add(size).min(self.data.len());
        Ok(&self.data[start..end])
    }
}

#[derive(Debug)]
pub(crate) struct Directory {
    pub attr: Attr,
    pub entries: Vec<CString>,
}

impl Directory {
    pub fn open(path: &CStr) -> Result<Self> {
        let mut entries = vec![CString::new(".").unwrap(), CString::new("..").unwrap()];
        let node = Node::parse(path)?;
        match node {
            Node::Root => {
                entries.push(c"net".to_owned());
                entries.extend(
                    system::FILES
                        .iter()
                        .map(|(name, _)| CString::new(*name).unwrap()),
                );
                let mut pids = process::list_pids().map_err(errno)?;
                pids.sort_unstable();
                pids.dedup();
                entries.extend(
                    pids.into_iter()
                        .map(|pid| CString::new(pid.to_string()).unwrap()),
                );
            }
            Node::Process(pid) if process::pid_exists(pid) => {
                entries.extend([c"fd".to_owned(), c"task".to_owned()]);
                entries.extend(
                    PROCESS_FILES
                        .iter()
                        .map(|(name, _)| CString::new(*name).unwrap()),
                );
            }
            Node::Process(_) => return Err(libc::ENOENT),
            Node::Net => entries.push(c"dev".to_owned()),
            Node::Fds(pid) => {
                entries.extend(
                    process_query(pid, || process::list_fds(pid))?
                        .into_iter()
                        .map(|entry| CString::new(entry.fd.to_string()).unwrap()),
                );
            }
            Node::Tasks(pid) => {
                entries.extend(
                    process_query(pid, || process::list_threads(pid))?
                        .into_iter()
                        .map(|tid| CString::new(tid.to_string()).unwrap()),
                );
            }
            Node::Thread(pid, tid) => {
                process_query(pid, || process::check_thread(pid, tid))?;
                entries.extend([c"stat".to_owned(), c"status".to_owned(), c"comm".to_owned()]);
            }
            _ => return Err(libc::ENOTDIR),
        }
        Ok(Self {
            attr: Attr::new(node, 0, DIRECTORY)?,
            entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_pids_cannot_become_process_group_queries() {
        for path in [
            c"/0",
            c"/4294967295",
            c"/+1",
            c"/01",
            c"/1/stat/extra",
            c"/1/unknown",
            c"/1/fd/-1",
            c"/1/fd/01",
            c"/1/fd/2147483648",
            c"/1/fd/0/extra",
            c"/1/task/0",
            c"/1/task/01",
            c"/1/task/1/stat/extra",
        ] {
            assert_eq!(Node::parse(path), Err(libc::ENOENT));
        }
    }

    #[test]
    fn extended_process_queries_use_native_resources() {
        let pid = std::process::id();
        let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        use std::os::fd::AsRawFd;
        let fdpath = CString::new(format!("/{pid}/fd/{}", file.as_raw_fd())).unwrap();
        let target = readlink(&fdpath).unwrap();
        assert_eq!(target, process::get_exe(pid).unwrap());
        let tasks = Directory::open(&CString::new(format!("/{pid}/task")).unwrap()).unwrap();
        let mut tid = 0;
        assert_eq!(unsafe { libc::pthread_threadid_np(0, &mut tid) }, 0);
        assert!(
            tasks
                .entries
                .contains(&CString::new(tid.to_string()).unwrap())
        );
        let status =
            OpenFile::open(&CString::new(format!("/{pid}/task/{tid}/status")).unwrap()).unwrap();
        assert!(
            status
                .data
                .windows(format!("Pid:\t{tid}\n").len())
                .any(|x| x == format!("Pid:\t{tid}\n").as_bytes())
        );
        for name in ["environ", "limits", "io", "maps"] {
            let file = OpenFile::open(&CString::new(format!("/{pid}/{name}")).unwrap()).unwrap();
            assert!(!file.data.is_empty(), "{name} returned no data");
        }
        for alias in [c"/self", c"/thread-self"] {
            assert_eq!(readlink(alias), Err(libc::ENOTSUP));
        }
        let keys = [
            Node::Fds(pid),
            Node::Fd(pid, 0),
            Node::Tasks(pid),
            Node::Thread(pid, tid),
            Node::ThreadFile(pid, tid, ProcessFile::Stat),
            Node::Thread(pid, u64::MAX),
        ];
        let ids: std::collections::HashSet<_> = keys.iter().map(|node| node.ino()).collect();
        assert_eq!(ids.len(), keys.len());
    }

    #[test]
    fn partial_reads_and_eof_use_the_same_sample() {
        let file = OpenFile {
            attr: Attr::new(Node::Root, 6, FILE).unwrap(),
            data: b"abcdef".to_vec(),
        };
        assert_eq!(file.read(3, 2), Ok(&b"de"[..]));
        assert_eq!(file.read(0, 3), Ok(&b"abc"[..]));
        assert_eq!(file.read(6, 100), Ok(&b""[..]));
        assert_eq!(file.read(i64::MAX, usize::MAX), Ok(&b""[..]));
        assert_eq!(file.read(-1, 2), Err(libc::EINVAL));
    }

    #[test]
    fn process_queries_work_without_mounting() {
        let pid = std::process::id();
        let path = CString::new(format!("/{pid}")).unwrap();
        let dir = Directory::open(&path).unwrap();
        assert_eq!(dir.entries.len(), PROCESS_FILES.len() + 4);
        for (name, _) in PROCESS_FILES {
            assert!(dir.entries.contains(&CString::new(*name).unwrap()));
        }
        let stat = OpenFile::open(&CString::new(format!("/{pid}/stat")).unwrap()).unwrap();
        let data = stat.read(0, usize::MAX).unwrap();
        assert!(data.starts_with(pid.to_string().as_bytes()));
        assert_eq!(stat.attr.size, data.len() as u64);
        let cmdline = OpenFile::open(&CString::new(format!("/{pid}/cmdline")).unwrap()).unwrap();
        let expected: Vec<u8> = std::env::args_os()
            .flat_map(|arg| {
                use std::os::unix::ffi::OsStrExt;
                let mut bytes = arg.as_bytes().to_vec();
                bytes.push(0);
                bytes
            })
            .collect();
        assert_eq!(cmdline.data, expected);
        let comm = OpenFile::open(&CString::new(format!("/{pid}/comm")).unwrap()).unwrap();
        assert!(comm.data.ends_with(b"\n"));
        assert!(comm.data.len() <= 16);
        let status = OpenFile::open(&CString::new(format!("/{pid}/status")).unwrap()).unwrap();
        assert!(status.data.windows(5).any(|part| part == b"Uid:\t"));
        assert_eq!(status.attr.uid, unsafe { libc::geteuid() });
        assert_eq!(status.attr.gid, unsafe { libc::getegid() });
        // FSKit may remap displayed stat ownership. Verify the backend itself
        // retains public credentials for root-owned processes.
        assert_eq!(getattr(c"/1").unwrap().uid, 0);
        let exe_path = CString::new(format!("/{pid}/exe")).unwrap();
        assert_eq!(getattr(&exe_path).unwrap().kind, SYMLINK);
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            readlink(&exe_path).unwrap(),
            std::env::current_exe().unwrap().as_os_str().as_bytes()
        );
        let cwd_path = CString::new(format!("/{pid}/cwd")).unwrap();
        assert_eq!(
            readlink(&cwd_path).unwrap(),
            std::env::current_dir().unwrap().as_os_str().as_bytes()
        );
        assert_eq!(OpenFile::open(&cwd_path).unwrap_err(), libc::ELOOP);
        let root = Directory::open(c"/").unwrap();
        assert!(
            root.entries
                .contains(&CString::new(pid.to_string()).unwrap())
        );
    }
}

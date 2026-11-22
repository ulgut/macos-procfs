use clap::Parser;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;

mod bridge;
mod macos;
mod procfs;

#[derive(Parser)]
#[command(
    version,
    about = "Read-only process information through macFUSE's FSKit backend"
)]
struct Args {
    /// Existing, empty directory at which to mount procfs
    mount_point: PathBuf,

    /// Allow root to access the volume in addition to the mounting user
    #[arg(long)]
    allow_root: bool,

    /// Log FUSE requests in the foreground
    #[arg(long)]
    debug: bool,
}

fn run(args: Args) -> Result<i32, Box<dyn std::error::Error>> {
    let mountpoint = args.mount_point.canonicalize()?;
    if !mountpoint.is_dir() || mountpoint.read_dir()?.next().is_some() {
        return Err("mount point must be an existing, empty directory".into());
    }

    let mut options = "backend=fskit,ro,fsname=mac_procfs,volname=mac_procfs,\
        attr_timeout=0,entry_timeout=0,negative_timeout=0"
        .to_owned();
    if args.allow_root {
        options.push_str(",allow_root");
    }
    let mut argv = vec![
        CString::new("mac_procfs")?,
        CString::new("-f")?,
        CString::new("-s")?,
        CString::new("-o")?,
        CString::new(options)?,
    ];
    if args.debug {
        argv.push(CString::new("-d")?);
    }
    argv.push(CString::new(mountpoint.as_os_str().as_bytes())?);
    let mut pointers: Vec<_> = argv.iter().map(|s| s.as_ptr().cast_mut()).collect();
    let argc = pointers.len() as i32;
    pointers.push(std::ptr::null_mut());

    eprintln!("Mounting procfs at {} using FSKit", mountpoint.display());
    // All argv storage stays alive until libfuse's foreground loop returns.
    Ok(unsafe { bridge::procfs_fuse_main(argc, pointers.as_mut_ptr()) })
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => {
            eprintln!(
                "FSKit mount failed. Install macFUSE 5.5 or later and enable its file system \
                 extension in System Settings > General > Login Items & Extensions."
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("mac_procfs: {err}");
            ExitCode::FAILURE
        }
    }
}

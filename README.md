# mac_procfs

A read-only, Linux-style process and system-information filesystem for macOS.

| System files | Per-process entries |
| --- | --- |
| `stat`, `meminfo`, `uptime`, `loadavg`, `cpuinfo`, `version`, `mounts`, `diskstats`, `net/dev` | `stat`, `cmdline`, `comm`, `status`, `statm`, `exe`, `cwd`, `environ`, `io`, `maps`, `fd/`, `task/`, `limits` (daemon only) |

Process `stat` has all 52 field positions and a boot-relative start time. `exe`
and `cwd` are symlinks. See [COMPATIBILITY.md](COMPATIBILITY.md) for supported
fields, placeholders, approximations, and access behavior.

Mounting uses **macFUSE's FSKit backend**, which means you don't need install a kernel extension (:)). I'm aware there's a far cleaner native MacOS FSKit integration [option](https://docs.rs/objc2-fs-kit/latest/objc2_fs_kit/) to eliminate the macFUSE backend but that involved either a) disabling SIP (no) or 2) the classic Apple developer licensing fee (also no). 

## Setup

- macOS **26.4 or newer** for this read-only FSKit configuration.
- [macFUSE **5.5 or newer**](https://github.com/macfuse/macfuse/releases), including development headers and libraries.
- Rust supporting edition 2024, a C compiler, and `pkg-config`.

Install the official macFUSE package, then enable its **File System Extension** in System Settings → General → Login Items & Extensions. As stated above, Kernel extension approval is unnecessary for this backend.

Check the development library and build:

```sh
pkg-config --modversion fuse
cargo build --locked
```

The pkg-config result is the **libfuse API version** (normally `2.9.9`), not the
macFUSE package version. If it cannot find `fuse.pc`, check the development files
and add `/usr/local/lib/pkgconfig` to `PKG_CONFIG_PATH` if necessary.

## Mount

```sh
mkdir -p /tmp/mac-procfs
cargo run --locked -- /tmp/mac-procfs
```

In another terminal:

```sh
ls /tmp/mac-procfs
cat /tmp/mac-procfs/meminfo
cat /tmp/mac-procfs/$$/status
readlink /tmp/mac-procfs/$$/cwd
tr '\000' '\n' < /tmp/mac-procfs/$$/cmdline
ls /tmp/mac-procfs/$$/fd /tmp/mac-procfs/$$/task
cat /tmp/mac-procfs/$$/maps
cat /tmp/mac-procfs/diskstats /tmp/mac-procfs/net/dev
umount /tmp/mac-procfs
```

The server runs in the foreground; libfuse handles shutdown signals and unmounts.
Use `--debug` for FUSE request logging. `--allow-root` additionally permits root
access to the volume.

## Read behavior

A FUSE open handle contains one immutable sample. Offset reads and `fstat` use that
same sample; reopening collects a fresh one. Directory handles retain a PID
listing and support continuation offsets. Attribute and entry cache timeouts
are zero, and the adapter requests direct I/O without keeping cached data.

FSKit's handling of dynamic sizes and caching still needs live validation on
each supported macOS version. On macOS 26.6.2 with macFUSE 5.5.0, reopening
process files returns fresh samples, but `stat()` can retain a PID directory's
metadata after that process exits. Fresh root listings omit exited processes,
and reopening their files must fail. Zero cache timeouts do not eliminate
FSKit's separate metadata cache. See the [maintainer's explanation](https://github.com/macfuse/macfuse/issues/1207).

The expanded end-to-end test passed on macOS 26.6.2 / macFUSE 5.5.0 on November 21,
2026, including checks that no FUSE kernel extension was loaded before or after
mounting. Immediate disappearance of cached PID metadata remains unsupported.
FSKit also remaps displayed `stat()` ownership to the mounting user; process
`status` retains native credential values.

For the pytest E2E suite, setup, and individual case selection, see
[tests/README.md](tests/README.md).

## Backend references

- [macFUSE backends](https://github.com/macfuse/macfuse/wiki/FUSE-Backends)
- [FSKit transport and MFMount](https://github.com/macfuse/macfuse/wiki/Getting-Started-%28Developer%29-%E2%80%90-MFMount.framework)
- [Read-only mounts and short-read caveats in macFUSE 5.5](https://macfuse.github.io/2026/10/17/macfuse-5.5.0.html)
- [Apple FSContext](https://developer.apple.com/documentation/fskit/fscontext)
- [Unresponsive FSKit toggles on macOS 26.6.2 and a backed-up workaround](https://github.com/macfuse/macfuse/issues/1194)

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
proc=/tmp/mac-procfs
pid=$$

# Discover system files and running process IDs.
ls "$proc"

# Read the process name, credentials, memory usage, and thread count.
cat "$proc/$pid/comm" "$proc/$pid/status"

# Locate its executable and current working directory.
readlink "$proc/$pid/exe"
readlink "$proc/$pid/cwd"

# Display one command-line argument per line (the file is NUL-separated).
tr '\000' '\n' < "$proc/$pid/cmdline"
```

Explore open files, threads, and memory mappings:

```sh
# Show descriptor numbers and their file, pipe, socket, or kqueue targets.
ls -l "$proc/$pid/fd"

# List native thread IDs and show each thread's name.
for thread in "$proc/$pid"/task/*; do
    printf '%s: ' "${thread##*/}"
    cat "$thread/comm"
done

# Inspect the first mappings, then list only executable regions.
head -n 15 "$proc/$pid/maps"
awk '$2 ~ /x/ { print }' "$proc/$pid/maps"

# Read cumulative disk bytes attributed to this process.
cat "$proc/$pid/io"
```

## Backend references

- [macFUSE backends](https://github.com/macfuse/macfuse/wiki/FUSE-Backends)
- [FSKit transport and MFMount](https://github.com/macfuse/macfuse/wiki/Getting-Started-%28Developer%29-%E2%80%90-MFMount.framework)
- [Read-only mounts and short-read caveats in macFUSE 5.5](https://macfuse.github.io/2026/10/17/macfuse-5.5.0.html)
- [Apple FSContext](https://developer.apple.com/documentation/fskit/fscontext)
- [Unresponsive FSKit toggles on macOS 26.6.2 and a backed-up workaround](https://github.com/macfuse/macfuse/issues/1194)

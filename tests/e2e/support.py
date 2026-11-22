"""Mount supervision, child-process protocol, and small proc-format parsers."""
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
PROCESS_ENTRIES = {
    "stat", "cmdline", "comm", "status", "statm", "exe", "cwd",
    "environ", "limits", "io", "maps", "fd", "task",
}
REGULAR_PROCESS_FILES = (
    "stat", "cmdline", "comm", "status", "statm", "environ", "io", "maps", "limits",
)
SYSTEM_ENTRIES = {
    "stat", "meminfo", "uptime", "loadavg", "cpuinfo", "version", "mounts", "diskstats", "net",
}


def mounted_line(mountpoint):
    mounts = subprocess.run(
        ["/sbin/mount"], capture_output=True, text=True, check=True, timeout=10,
    ).stdout
    return next((line for line in mounts.splitlines() if f" on {mountpoint} (" in line), "")


def check_no_fuse_kernel_extension():
    result = subprocess.run(
        ["/usr/bin/kmutil", "showloaded", "--list-only"],
        capture_output=True, text=True, check=True, timeout=10,
    )
    assert not any("macfuse" in line.lower() or "osxfuse" in line.lower()
                   for line in result.stdout.splitlines()), "a FUSE kernel extension is loaded"


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def build_binary(binary=None):
    if binary:
        return Path(binary).resolve()
    subprocess.run(["cargo", "build", "--locked"], cwd=ROOT, check=True, timeout=120)
    return ROOT / "target/debug/mac_procfs"


class ProcfsMount:
    """Own the daemon and temporary mount; retain logs when checks fail."""

    def __init__(self, binary):
        self.binary = binary
        self.work = Path(tempfile.mkdtemp(prefix="mac-procfs-e2e-")).resolve()
        self.path = self.work / "mount"
        self.path.mkdir()
        self.log_path = self.work / "mount.log"
        self.log = None
        self.daemon = None
        self.keep_artifacts = False

    def __enter__(self):
        try:
            check_no_fuse_kernel_extension()
            self.log = self.log_path.open("w")
            self.daemon = subprocess.Popen(
                [str(self.binary), "--debug", str(self.path)], stdout=self.log, stderr=self.log,
            )
            deadline = time.monotonic() + 30
            while not (line := mounted_line(self.path)):
                if self.daemon.poll() is not None:
                    raise RuntimeError(f"mount exited with {self.daemon.returncode}; see {self.log_path}")
                if time.monotonic() > deadline:
                    raise TimeoutError(f"FSKit mount timed out; see {self.log_path}")
                time.sleep(0.1)
            assert "fskit" in line.lower(), f"mount is not using FSKit: {line}"
            print(f"Backend confirmed: {line}", flush=True)
            return self
        except BaseException:
            self.__exit__(*sys.exc_info())
            raise

    def __exit__(self, exc_type, exc, traceback):
        success = exc_type is None and not self.keep_artifacts
        try:
            if mounted_line(self.path):
                subprocess.run(["/sbin/umount", str(self.path)], check=True, timeout=15)
            if self.daemon is not None and exc_type is None:
                assert self.daemon.wait(timeout=10) == 0, "daemon did not exit cleanly"
            assert not mounted_line(self.path), "volume remained mounted"
            check_no_fuse_kernel_extension()
        except BaseException:
            success = False
            if exc_type is None:
                raise
        finally:
            stop(self.daemon)
            if self.log is not None:
                self.log.close()
            if success and not mounted_line(self.path):
                shutil.rmtree(self.work)
            else:
                print(f"E2E mount artifacts retained at {self.work}", file=sys.stderr)


class Actor:
    """One fresh process per test; commands mutate only the requested resource."""

    def __init__(self, mountpoint, cwd):
        self.cwd = cwd.resolve()
        self.argv = [sys.executable, str(Path(__file__).with_name("actor.py")),
                     "argument with spaces", "", "last"]
        self.error_path = self.cwd / "actor.stderr"
        self.stderr = self.error_path.open("w")
        self.child = subprocess.Popen(
            self.argv, cwd=self.cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.stderr, text=True,
            env=dict(os.environ, PROCFS_SMOKE_MARKER="a value=with equals", PROCFS_SMOKE_EMPTY=""),
        )
        self.pid = self.child.pid
        self.directory = mountpoint / str(self.pid)

    def ready(self):
        self.info = self.response()
        # Framework Python launchers may replace argv[0] when starting Python.app.
        assert self.info["argv"][1:] == self.argv[1:], "launcher changed child arguments"
        return self

    def response(self):
        ready, _, _ = select.select([self.child.stdout], [], [], 10)
        assert ready, f"actor did not respond; see {self.error_path}"
        line = self.child.stdout.readline()
        assert line, f"actor exited unexpectedly; see {self.error_path}"
        return json.loads(line)

    def command(self, command):
        self.child.stdin.write(command + "\n")
        self.child.stdin.flush()
        assert self.response() == {"done": command}

    def terminate(self):
        stop(self.child)

    def close(self):
        self.terminate()
        self.child.stdin.close()
        self.child.stdout.close()
        self.stderr.close()


def stat_fields(data):
    """Return fields 3..52; names can contain whitespace and parentheses."""
    return data.rsplit(b") ", 1)[1].split()


def cpu_ticks(data):
    fields = stat_fields(data)
    return int(fields[11]) + int(fields[12])


def keyed_file(path):
    text = path.read_text()
    lines = text.splitlines()
    assert all(":" in line for line in lines), f"malformed {path.name} record: {text!r}"
    return dict(line.split(":", 1) for line in lines)


def network_counters(mountpoint):
    lines = (mountpoint / "net/dev").read_text().splitlines()
    assert "Receive" in lines[0] and "Transmit" in lines[0]
    return {name.strip(): list(map(int, values.split()))
            for name, values in (row.split(":", 1) for row in lines[2:])}


def disk_counters(mountpoint):
    return [line.split() for line in (mountpoint / "diskstats").read_text().splitlines()]

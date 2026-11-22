"""Descriptor/thread lifecycle, mapped files, and I/O counter activity."""
import os

import pytest

from .support import cpu_ticks, disk_counters, keyed_file, network_counters, stat_fields


@pytest.mark.parametrize("kind,prefix", [
    ("file_fd", None), ("pipe_fd", "pipe:["),
    ("socket_fd", "socket:["), ("queue_fd", "anon_inode:[kqueue]"),
], ids=["file", "pipe", "socket", "kqueue"])
def test_descriptor_is_listed_and_has_a_native_target(actor, kind, prefix):
    # Given a descriptor held open by the child.
    fd = str(actor.info[kind])
    directory = actor.directory / "fd"
    # When listing the descriptor directory and resolving its target.
    entries, target = os.listdir(directory), os.readlink(directory / fd)
    # Then its native number and path/anonymous-object label are exposed.
    assert fd in entries
    if prefix is None:
        assert target == actor.info["mapped_path"]
    else:
        assert target.startswith(prefix)


def test_closed_descriptor_disappears_from_fresh_listing(actor):
    # Given a pipe descriptor whose target has already been looked up.
    directory = actor.directory / "fd"
    fd = str(actor.info["pipe_fd"])
    os.readlink(directory / fd)
    # When the child closes the descriptor.
    actor.command("close_pipe")
    # Then a new directory sample omits it despite FSKit's metadata cache.
    assert fd not in os.listdir(directory)


@pytest.mark.parametrize("kind", ["main_tid", "worker_tid"], ids=["main", "worker"])
def test_task_uses_native_thread_ids_and_process_group_identity(actor, kind):
    # Given IDs reported by pthread_threadid_np through Python.
    tid = actor.info[kind]
    directory = actor.directory / "task"
    # When reading the thread's supported files.
    thread = directory / str(tid)
    status, stat = keyed_file(thread / "status"), (thread / "stat").read_bytes()
    # Then task paths use native TIDs and distinguish thread ID from process Tgid.
    assert str(tid) in os.listdir(directory)
    assert set(os.listdir(thread)) == {"stat", "status", "comm"}
    assert int(status["Tgid"]) == actor.pid and int(status["Pid"]) == tid
    assert stat.startswith(f"{tid} (".encode())
    assert len(stat_fields(stat)) + 2 == 52


def test_worker_comm_preserves_its_native_name(actor):
    # Given the worker's explicitly assigned native thread name.
    thread = actor.directory / "task" / str(actor.info["worker_tid"])
    # When reading the thread comm file.
    name = (thread / "comm").read_bytes()
    # Then the name is reported with the proc newline convention.
    assert name == b"procfs-worker\n"


def test_thread_cpu_time_uses_host_clock_ticks(actor):
    # Given a worker that waits until asked to consume 0.25 seconds of thread CPU.
    path = actor.directory / "task" / str(actor.info["worker_tid"]) / "stat"
    before = cpu_ticks(path.read_bytes())
    # When it burns CPU and we reopen its stat file.
    actor.command("burn_thread")
    delta = cpu_ticks(path.read_bytes()) - before
    # Then the converted counters agree with host clock ticks, not Mach tick units.
    hz = os.sysconf("SC_CLK_TCK")
    assert 0.2 * hz <= delta <= 0.5 * hz


def test_exited_thread_is_unlisted_and_fresh_reads_fail(actor):
    # Given a thread whose stat file has been read before exit.
    directory = actor.directory / "task"
    tid = str(actor.info["worker_tid"])
    path = directory / tid / "stat"
    path.read_bytes()
    # When the worker exits while its process remains alive.
    actor.command("retire_thread")
    # Then new directory samples and file opens observe that thread's exit.
    assert tid not in os.listdir(directory)
    with pytest.raises(FileNotFoundError):
        path.read_bytes()


def test_maps_contains_the_known_file_with_native_metadata(actor):
    # Given a writable mapping of a path with spaces, Unicode, and a newline.
    native = os.stat(actor.info["mapped_path"])
    expected_path = os.fsencode(actor.info["mapped_path"]).replace(b"\n", b"\\012")
    # When reading ordered memory-map records.
    rows = (actor.directory / "maps").read_bytes().splitlines()
    previous_end, found = 0, False
    for row in rows:
        fields = row.split(None, 5)
        start, end = (int(n, 16) for n in fields[0].split(b"-"))
        # Then regions are ordered, nonempty, and preserve native metadata and escaping.
        assert previous_end <= start < end
        previous_end = end
        assert len(fields[1]) == 4 and fields[1][3:] in (b"p", b"s")
        int(fields[2], 16)
        major, minor = (int(n, 16) for n in fields[3].split(b":"))
        inode = int(fields[4])
        if len(fields) == 6 and fields[5] == expected_path:
            assert inode == native.st_ino
            assert (major, minor) == (os.major(native.st_dev), os.minor(native.st_dev))
            assert fields[1][:3] == b"rw-"
            found = True
    assert found, "known file mapping missing"


def test_process_io_accounts_for_disk_writes(actor):
    # Given a child whose I/O file has not yet been cached by FSKit.
    # When it writes and fsyncs eight MiB before the first sample.
    actor.command("write_disk")
    counters = keyed_file(actor.directory / "io")
    # Then the supported fields account for those bytes without testing size-cache behavior.
    assert set(counters) == {"read_bytes", "write_bytes"}
    assert int(counters["read_bytes"]) >= 0
    assert int(counters["write_bytes"]) >= 8 * 1024 * 1024


def test_growing_io_file_is_complete_without_metadata_refresh(actor, strict_metadata):
    # Given an I/O file whose shorter initial length has been cached.
    path = actor.directory / "io"
    before = path.read_bytes()
    # When counters grow and the file is reopened without a separate stat call.
    actor.command("write_disk")
    sample = path.read_bytes()
    # Then a read should contain the full record, not a prefix clipped to the old size.
    assert before.endswith(b"\n") and b"write_bytes:" in before
    clipped_to_old_length = (len(sample) == len(before) and not sample.endswith(b"\n")
                             and sample.startswith(b"read_bytes: "))
    if clipped_to_old_length and not strict_metadata:
        pytest.xfail("FSKit clips a growing I/O file to its previously cached byte length")
    assert sample.endswith(b"\n"), f"incomplete I/O record: {sample!r}"
    after = dict(line.split(b":", 1) for line in sample.splitlines())
    initial = dict(line.split(b":", 1) for line in before.splitlines())
    assert all(int(after[key]) >= int(initial[key]) for key in initial)
    assert int(after[b"write_bytes"]) > int(initial[b"write_bytes"])


def test_network_counters_account_for_loopback_traffic(mountpoint, actor):
    # Given native interface counters before any test-generated traffic.
    before = network_counters(mountpoint)
    # When the child sends and receives 128 KiB on loopback.
    actor.command("send_loopback")
    after = network_counters(mountpoint)
    # Then all records have 16 nonnegative fields and loopback byte counts advance.
    assert "lo0" in before and "lo0" in after
    assert all(len(values) == 16 and all(n >= 0 for n in values) for values in after.values())
    assert after["lo0"][0] >= before["lo0"][0] + 131072
    assert after["lo0"][8] >= before["lo0"][8] + 131072


def test_diskstats_preserves_native_devices_and_monotonic_counters(mountpoint, actor):
    # Given a sample of physical block-device counters.
    before = {row[2]: list(map(int, row[3:])) for row in disk_counters(mountpoint)}
    # When disk writes occur and the system counters are reopened.
    actor.command("write_disk")
    rows = disk_counters(mountpoint)
    # Then device IDs match /dev and measured counters do not go backwards.
    assert before and rows, "no native block storage counters found"
    for row in rows:
        assert len(row) == 14
        major, minor = map(int, row[:2])
        device = os.stat("/dev/" + row[2]).st_rdev
        assert (major, minor) == (os.major(device), os.minor(device))
        values = list(map(int, row[3:]))
        assert all(n >= 0 for n in values)
        assert [values[i] for i in (1, 5, 8, 9, 10)] == [0] * 5
        if row[2] in before:
            assert all(values[i] >= before[row[2]][i] for i in (0, 2, 3, 4, 6, 7))

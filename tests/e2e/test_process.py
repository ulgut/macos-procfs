"""Process identity, file contents, read samples, permissions, and exit."""
import errno
import os
from pathlib import Path

import pytest

from .support import PROCESS_ENTRIES, REGULAR_PROCESS_FILES, cpu_ticks, keyed_file, stat_fields


def test_process_and_its_entries_are_listed(mountpoint, actor):
    # Given a live process with known resources.
    # When enumerating procfs and its process directory.
    root, entries = set(os.listdir(mountpoint)), set(os.listdir(actor.directory))
    # Then its PID and all supported entry names are visible.
    assert str(actor.pid) in root
    assert entries == PROCESS_ENTRIES


def test_cmdline_preserves_empty_and_spaced_arguments(actor):
    # Given the arguments reported by the child after Python's launcher runs.
    expected = b"".join(os.fsencode(arg) + b"\0" for arg in actor.info["argv"])
    # When reading cmdline in three-byte chunks.
    with open(actor.directory / "cmdline", "rb", buffering=0) as stream:
        chunks = []
        while chunk := stream.read(3):
            chunks.append(chunk)
    # Then arguments retain their bytes, separators, and empty values.
    assert b"".join(chunks) == expected


def test_comm_is_a_linux_length_process_name(actor):
    # Given the native executable name.
    expected = os.fsencode(Path(actor.info["argv"][0]).name)[:15] + b"\n"
    # When reading comm.
    actual = (actor.directory / "comm").read_bytes()
    # Then it is truncated to 15 name bytes and terminated by a newline.
    assert actual == expected


def test_status_preserves_identity_credentials_and_memory(actor):
    # Given a child of this pytest process running with the same credentials.
    # When reading its status record.
    status = keyed_file(actor.directory / "status")
    # Then IDs and supported memory/thread fields match native expectations.
    assert int(status["Pid"]) == actor.pid
    assert int(status["PPid"]) == os.getpid()
    assert list(map(int, status["Uid"].split()))[:2] == [os.getuid(), os.geteuid()]
    assert list(map(int, status["Gid"].split()))[:2] == [os.getgid(), os.getegid()]
    assert int(status["Threads"]) >= 2
    assert int(status["VmSize"].split()[0]) > 0
    assert int(status["VmRSS"].split()[0]) > 0
    metadata = (actor.directory / "status").stat()
    assert (metadata.st_uid, metadata.st_gid) == (os.geteuid(), os.getegid())


def test_statm_preserves_supported_values_and_placeholders(actor):
    # Given a running process with resident and virtual memory.
    # When reading statm.
    values = list(map(int, (actor.directory / "statm").read_text().split()))
    # Then its seven fields contain native page counts and explicit placeholders.
    assert len(values) == 7 and values[0] > 0 and values[1] > 0
    assert values[2:] == [0] * 5


@pytest.mark.parametrize("name", ["exe", "cwd"])
def test_process_symlink_resolves_to_native_path(actor, name):
    # Given a known executable and a cwd containing spaces and Unicode.
    expected = Path(actor.info["argv"][0]).resolve() if name == "exe" else actor.cwd
    path = actor.directory / name
    # When querying the symlink and its metadata.
    target, metadata = os.readlink(path), path.lstat()
    # Then it points to the native path and displays the mounting credentials.
    assert path.is_symlink()
    assert target == str(expected)
    assert (metadata.st_uid, metadata.st_gid) == (os.geteuid(), os.getegid())


def test_stat_open_is_an_immutable_sample_and_reopen_is_fresh(actor):
    # Given one open stat handle sampled before CPU activity.
    with open(actor.directory / "stat", "rb", buffering=0) as stream:
        initial = stream.read()
        fields = stat_fields(initial)
        assert initial.startswith(f"{actor.pid} (".encode())
        assert len(fields) + 2 == 52
        assert int(fields[1]) == os.getpid()
        assert int(fields[19]) > 0
        assert len(initial) == os.fstat(stream.fileno()).st_size
        assert stream.read(1) == b""
        assert os.pread(stream.fileno(), 10, len(initial) + 10) == b""
        stream.seek(0)
        prefix = stream.read(7)
        # When CPU time advances while this handle remains open.
        actor.command("burn_cpu")
        same_handle = prefix + stream.read()
    fresh = (actor.directory / "stat").read_bytes()
    # Then partial reads stay consistent and reopening samples new CPU counters.
    assert same_handle == initial
    assert cpu_ticks(fresh) > cpu_ticks(initial)
    assert stat_fields(fresh)[19] == fields[19], "process start time changed"


def test_environ_preserves_values_without_leaking_into_cmdline(actor):
    # Given environment values containing spaces, equals signs, and an empty value.
    # When reading environment and arguments separately.
    environment = set((actor.directory / "environ").read_bytes().split(b"\0"))
    cmdline = (actor.directory / "cmdline").read_bytes()
    # Then environment values survive and do not become arguments.
    assert b"PROCFS_SMOKE_MARKER=a value=with equals" in environment
    assert b"PROCFS_SMOKE_EMPTY=" in environment
    assert b"PROCFS_SMOKE_MARKER=" not in cmdline


def test_other_process_limits_are_explicitly_unsupported(actor):
    # Given a process other than the filesystem daemon.
    # When requesting its resource limits.
    with pytest.raises(OSError) as error:
        (actor.directory / "limits").read_bytes()
    # Then the daemon does not substitute its own limits.
    assert error.value.errno == errno.ENOTSUP


@pytest.mark.parametrize("alias", ["self", "thread-self"])
def test_caller_dependent_aliases_are_explicitly_unsupported(mountpoint, alias):
    # Given an FSKit transport without caller PID/thread identity.
    # When resolving an alias that depends on that identity.
    with pytest.raises(OSError) as error:
        os.readlink(mountpoint / alias)
    # Then it fails explicitly and is not advertised as a working entry.
    assert error.value.errno == errno.ENOTSUP
    assert alias not in os.listdir(mountpoint)


@pytest.mark.parametrize("create", [False, True], ids=["existing-file", "new-file"])
def test_volume_rejects_writes(mountpoint, actor, create):
    # Given a read-only mount and either an existing or a proposed file.
    path = mountpoint / "new" if create else actor.directory / "stat"
    flags = os.O_WRONLY | (os.O_CREAT if create else 0)
    # When attempting to open it for writing.
    with pytest.raises(OSError) as error:
        fd = os.open(path, flags, 0o600)
        os.close(fd)
    # Then the write is rejected by the filesystem or access policy.
    assert error.value.errno in (errno.EROFS, errno.EACCES, errno.EPERM)


@pytest.mark.parametrize("name", REGULAR_PROCESS_FILES)
def test_exited_process_is_unlisted_and_fresh_reads_fail(mountpoint, actor, name):
    # Given a process whose file has already been looked up by FSKit.
    path = actor.directory / name
    path.stat()
    # When that process exits.
    actor.terminate()
    # Then fresh listings and opens consult live process state despite cached metadata.
    assert str(actor.pid) not in os.listdir(mountpoint)
    with pytest.raises(FileNotFoundError):
        path.read_bytes()


def test_open_sample_remains_readable_after_process_exit(actor):
    # Given a sampled handle to a live process.
    with open(actor.directory / "cmdline", "rb", buffering=0) as stream:
        initial = stream.read()
        stream.seek(0)
        # When the process exits before the handle is reread.
        actor.terminate()
        retained = stream.read()
    # Then the original immutable sample remains available.
    assert retained == initial


def test_exited_process_metadata_disappears(mountpoint, actor, strict_metadata):
    # Given process metadata cached before exit.
    actor.directory.stat()
    # When the process exits.
    actor.terminate()
    # Then metadata should disappear; FSKit's known retention is separately reported.
    assert str(actor.pid) not in os.listdir(mountpoint)
    try:
        actor.directory.stat()
    except FileNotFoundError:
        return
    if not strict_metadata:
        pytest.xfail("FSKit caches process metadata after exit; fresh reads are tested separately")
    pytest.fail("FSKit retained exited-process metadata")

"""System file formats, native units, mount transport, and counter freshness."""
import os
import subprocess
import time

import pytest

from .support import SYSTEM_ENTRIES, keyed_file, mounted_line


def test_mount_uses_fskit_and_lists_system_entries(mountpoint):
    # Given the mounted procfs volume.
    # When inspecting its transport and root directory.
    line, entries = mounted_line(mountpoint), set(os.listdir(mountpoint))
    # Then it uses FSKit and advertises all supported system files.
    assert "fskit" in line.lower()
    assert SYSTEM_ENTRIES <= entries


def test_meminfo_matches_native_total_and_uses_kib(mountpoint):
    # Given macOS's independently queried physical memory total.
    total = int(subprocess.run(
        ["/usr/sbin/sysctl", "-n", "hw.memsize"],
        capture_output=True, text=True, check=True, timeout=5,
    ).stdout)
    # When reading the proc memory fields.
    memory = {}
    for name, text in keyed_file(mountpoint / "meminfo").items():
        amount, unit = text.split()
        assert unit == "kB" and int(amount) >= 0
        memory[name] = int(amount)
    # Then totals use KiB and derived values stay within their native bounds.
    assert memory["MemTotal"] == total // 1024
    assert memory["MemFree"] <= memory["MemAvailable"] <= memory["MemTotal"]
    assert memory["SwapFree"] <= memory["SwapTotal"]


def test_stat_aggregate_equals_sum_of_native_cpu_columns(mountpoint):
    # Given a single system CPU sample.
    # When extracting the aggregate and per-core records.
    rows = [line.split() for line in (mountpoint / "stat").read_text().splitlines()]
    aggregate = list(map(int, rows[0][1:]))
    cores = [list(map(int, row[1:])) for row in rows if row[0].startswith("cpu") and row[0] != "cpu"]
    # Then all ten Linux positions are present and the aggregate is their exact sum.
    assert rows[0][0] == "cpu" and len(aggregate) == 10
    assert len(cores) == os.cpu_count() and all(len(core) == 10 for core in cores)
    assert aggregate == [sum(core[i] for core in cores) for i in range(10)]


def test_system_cpu_counters_advance_across_opens(mountpoint, actor):
    # Given the initial aggregate CPU counters.
    before = list(map(int, (mountpoint / "stat").read_text().splitlines()[0].split()[1:]))
    # When the child consumes CPU and the file is reopened.
    actor.command("burn_cpu")
    after = list(map(int, (mountpoint / "stat").read_text().splitlines()[0].split()[1:]))
    # Then a fresh sample includes the elapsed activity.
    assert sum(after) > sum(before)


def test_uptime_agrees_with_boot_time(mountpoint):
    # Given boot time from the system stat file.
    rows = [line.split() for line in (mountpoint / "stat").read_text().splitlines()]
    boot = int(next(row[1] for row in rows if row[0] == "btime"))
    # When reading elapsed uptime and aggregate idle seconds.
    uptime, idle = map(float, (mountpoint / "uptime").read_text().split())
    # Then the elapsed clock matches wall time and idle is nonnegative.
    assert uptime > 0 and idle >= 0
    assert abs(time.time() - boot - uptime) < 2


def test_loadavg_preserves_supported_values_and_placeholders(mountpoint):
    # Given native load averages with no equivalent Linux task/PID counters.
    # When reading loadavg.
    fields = (mountpoint / "loadavg").read_text().split()
    # Then the three averages are valid and unsupported positions stay explicit.
    assert len(fields) == 5 and all(float(value) >= 0 for value in fields[:3])
    assert fields[3:] == ["0/0", "0"]


def test_cpuinfo_lists_each_online_cpu_with_a_model(mountpoint):
    # Given the host's online logical CPU count.
    # When reading cpuinfo.
    text = (mountpoint / "cpuinfo").read_text()
    # Then each CPU has one processor/model record.
    assert sum(line.startswith("processor\t:") for line in text.splitlines()) == os.cpu_count()
    assert text.count("model name\t:") == os.cpu_count()


def test_version_identifies_the_native_kernel(mountpoint):
    # Given the native kernel's system name.
    # When reading the version record.
    text = (mountpoint / "version").read_text()
    # Then the filesystem reports that kernel.
    assert os.uname().sysname in text


def test_mounts_contains_root_and_the_read_only_procfs_volume(mountpoint):
    # Given the system mount table including this test's mount.
    # When reading the Linux-shaped mount records.
    rows = [line.split() for line in (mountpoint / "mounts").read_text().splitlines()]
    # Then all records have six fields and our volume is identified as read-only.
    assert rows and all(len(row) == 6 for row in rows)
    assert any(row[1] == "/" for row in rows)
    own_mount = next(row for row in rows if row[1] == str(mountpoint))
    assert own_mount[2] == "macfuse" and "ro" in own_mount[3].split(",")


def test_process_metadata_displays_native_ownership(mountpoint, strict_metadata):
    # Given root-owned launchd, whose native ownership is independently known.
    if "1" not in os.listdir(mountpoint):
        pytest.skip("PID 1 is not visible")
    # When asking FSKit for the displayed process ownership.
    uid = (mountpoint / "1").stat().st_uid
    # Then ownership should be native; known noowners remapping is an expected failure.
    assert uid in (0, os.geteuid()), "unexpected process ownership"
    if uid != 0 and not strict_metadata:
        pytest.xfail("FSKit noowners remaps native UID/GID to the mounting user")
    assert uid == 0

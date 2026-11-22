"""Session mount and independently owned per-test child processes."""
from pathlib import Path
import sys

import pytest

from .support import Actor, ProcfsMount, build_binary


def pytest_addoption(parser):
    group = parser.getgroup("procfs")
    group.addoption("--procfs-mountpoint", type=Path, help="Reuse a mount owned by the smoke runner")
    group.addoption("--binary", type=Path, help="Use an already-built mac_procfs executable")
    group.addoption("--strict-metadata", action="store_true",
                    help="Fail instead of xfail on known FSKit ownership/metadata/size-cache limitations")


@pytest.fixture(scope="session")
def mountpoint(pytestconfig, request):
    if sys.platform != "darwin":
        pytest.skip("macFUSE FSKit tests require macOS")
    external = pytestconfig.getoption("--procfs-mountpoint")
    if external:
        yield external.resolve()
    else:
        with ProcfsMount(build_binary(pytestconfig.getoption("--binary"))) as mount:
            yield mount.path
            mount.keep_artifacts = bool(request.session.testsfailed)


@pytest.fixture
def actor(mountpoint, tmp_path):
    cwd = tmp_path / "procfs cwd ü with spaces"
    cwd.mkdir()
    child = Actor(mountpoint, cwd)
    try:
        yield child.ready()
    finally:
        child.close()


@pytest.fixture
def strict_metadata(pytestconfig):
    return pytestconfig.getoption("--strict-metadata")

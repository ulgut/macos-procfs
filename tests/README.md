# End-to-end tests

The suite mounts the real macFUSE FSKit backend on macOS. Install the Python
test dependency into a local environment:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements-dev.txt
```

Run the supervised suite (recommended), list cases without mounting, or select
a behavior by name:

```sh
.venv/bin/python tests/smoke.py
.venv/bin/python -m pytest --collect-only -q
.venv/bin/python tests/smoke.py -k maps
.venv/bin/python tests/smoke.py -k 'descriptor or task'
```

`smoke.py` only builds, mounts, starts pytest in a separate process group, and
cleans up. Its default suite timeout is 180 seconds; `--suite-timeout` changes
that limit. If a filesystem call stalls or pytest crashes, the supervisor kills
the worker and its children before unmounting. Failures retain `mount.log` and
print its location. Normal teardown verifies unmount, daemon exit, and no loaded
macFUSE/osxfuse kernel extension. `--binary PATH` skips the Cargo build.

Tests can also run directly with `.venv/bin/python -m pytest -v`. The session
fixture then owns the mount and normal teardown; use the supervisor when an
external timeout and crash cleanup are needed.

| File | Behavior |
| --- | --- |
| `e2e/test_process.py` | Process files, credentials, read snapshots, write rejection, exit, aliases, and limits. |
| `e2e/test_resources.py` | Descriptor/thread identity and lifecycle, memory maps, I/O, disk and network counters. |
| `e2e/test_system.py` | System formats, native units, CPU freshness, mounts, and ownership. |
| `e2e/conftest.py` | Session mount and a fresh child process for each test that requests one. |
| `e2e/actor.py` | Controlled process with named commands for CPU, file I/O, loopback traffic, and resource exit. |
| `e2e/support.py` | Mount/child ownership and small file-format parsers. |

Each test uses `# Given`, `# When`, and `# Then` comments. Test cases are
independent: choosing or reordering them does not require another test to run
first. Each child uses a cwd containing spaces and Unicode, empty/spaced argv,
and a mapped filename containing a newline. Pytest's temporary directory retains
child diagnostics (`actor.stderr`) on failure.

FSKit's metadata retention, ownership remapping, and clipping of growing I/O
files to a cached byte length are separate expected failures (`XFAIL`), not
blanket exclusions. They still perform
their prerequisite checks, and fresh reads after process/thread exit must pass
their own tests. I/O byte accounting is checked on a first sample after known
disk writes; a separate growth test checks completeness across reopens. To
require the missing metadata behavior too:

```sh
.venv/bin/python tests/smoke.py --strict-metadata
```

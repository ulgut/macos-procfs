#!/usr/bin/env python3
"""Run pytest in a supervised process so stalled FSKit requests can be cleaned up."""
import argparse
import importlib.util
import os
import signal
import subprocess
import sys

from e2e.support import ProcfsMount, ROOT, build_binary, stop


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, epilog="Extra arguments are passed to pytest, e.g. -v -k maps.",
    )
    parser.add_argument("--binary", help="Use an already-built mac_procfs executable")
    parser.add_argument("--strict-metadata", action="store_true")
    parser.add_argument(
        "--suite-timeout", type=float, default=180,
        help="Maximum seconds for the pytest worker (default: 180)",
    )
    args, pytest_args = parser.parse_known_args()
    if sys.platform != "darwin":
        parser.error("the FSKit tests require macOS")
    if args.suite_timeout <= 0:
        parser.error("--suite-timeout must be positive")
    if importlib.util.find_spec("pytest") is None:
        parser.error("install test dependencies first: python -m pip install -r requirements-dev.txt")
    command = [sys.executable, "-m", "pytest", str(ROOT / "tests/e2e"), "-v", *pytest_args]
    if args.strict_metadata:
        command.append("--strict-metadata")
    worker = None
    with ProcfsMount(build_binary(args.binary)) as mount:
        print(f"E2E mount log: {mount.log_path}", flush=True)
        try:
            worker = subprocess.Popen(
                command + ["--procfs-mountpoint", str(mount.path)],
                cwd=ROOT, start_new_session=True,
            )
            code = worker.wait(timeout=args.suite_timeout)
            mount.keep_artifacts = code != 0
            return code
        except subprocess.TimeoutExpired:
            mount.keep_artifacts = True
            print(f"pytest exceeded the {args.suite_timeout:g}s suite timeout", file=sys.stderr)
            return 124
        finally:
            # Kill the worker group before unmounting, including orphaned actors
            # if pytest failed or a filesystem request stalled.
            if worker is not None:
                try:
                    os.killpg(worker.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                stop(worker)


if __name__ == "__main__":
    raise SystemExit(main())

"""Controlled child process for procfs tests; has no pytest dependency."""
import ctypes
import json
import mmap
import os
from pathlib import Path
import select
import socket
import sys
import threading
import time


def burn_cpu(seconds, clock):
    deadline = clock() + seconds
    while clock() < deadline:
        pass


def main():
    mapped_path = Path.cwd() / "mapped ü file\nwith newline"
    mapped_file = open(mapped_path, "w+b", buffering=0)
    mapped_file.truncate(mmap.PAGESIZE * 2)
    mapping = mmap.mmap(mapped_file.fileno(), mmap.PAGESIZE * 2)
    mapping[0] = 42
    pipe = os.pipe()
    sockets = socket.socketpair()
    queue = select.kqueue()
    wake, done, started, retire = (threading.Event() for _ in range(4))

    def worker():
        ctypes.CDLL(None).pthread_setname_np(b"procfs-worker")
        started.set()
        wake.wait()
        burn_cpu(0.25, time.thread_time)
        done.set()
        retire.wait()

    thread = threading.Thread(target=worker, daemon=True)
    thread.start()
    assert started.wait(5)
    print(json.dumps({
        "argv": sys.orig_argv, "main_tid": threading.get_native_id(),
        "worker_tid": thread.native_id, "file_fd": mapped_file.fileno(),
        "pipe_fd": pipe[0], "socket_fd": sockets[0].fileno(),
        "queue_fd": queue.fileno(), "mapped_path": str(mapped_path),
    }), flush=True)

    for line in sys.stdin:
        command = line.strip()
        if command == "burn_cpu":
            burn_cpu(0.4, time.process_time)
        elif command == "burn_thread":
            wake.set()
            assert done.wait(5)
        elif command == "write_disk":
            with open("io-fixture", "wb", buffering=0) as stream:
                stream.write(b"x" * (8 * 1024 * 1024))
                os.fsync(stream.fileno())
        elif command == "send_loopback":
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver, \
                    socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                receiver.bind(("127.0.0.1", 0))
                receiver.settimeout(5)
                for _ in range(16):
                    sender.sendto(b"x" * 8192, receiver.getsockname())
                    receiver.recvfrom(16384)
        elif command == "close_pipe":
            os.close(pipe[0])
        elif command == "retire_thread":
            retire.set()
            wake.set()
            thread.join(5)
            assert not thread.is_alive()
        else:
            raise ValueError(f"unknown actor command: {command}")
        print(json.dumps({"done": command}), flush=True)


if __name__ == "__main__":
    main()

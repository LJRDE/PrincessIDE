#!/usr/bin/env python3
"""PrincessIDE P5 — record QEMU monitor samples over QMP.

Connects to a running QEMU's QMP unix socket and persists, for every requested
monitor query, both the raw HMP text (via `human-monitor-command`) and — for the
structured queries — the QMP JSON reply.  The samples are the fixed fixtures the
P5-4 monitor parser is asserted against, so they are written verbatim, never
hand-edited.

Usage:  qmp_capture.py <qmp-unix-socket> <output-directory>

Exit status is 0 only when every request returned without a QMP error.
"""

import json
import os
import socket
import sys
import time


class QMPError(RuntimeError):
    pass


class QMP:
    """Minimal synchronous QMP client (no third-party dependency)."""

    def __init__(self, path, connect_timeout=10.0):
        self.sock = self._connect(path, connect_timeout)
        self.fh = self.sock.makefile("rwb")
        self.greeting = self._read_message()
        self.capabilities = self.execute("qmp_capabilities")

    @staticmethod
    def _connect(path, timeout):
        deadline = time.time() + timeout
        last = None
        while time.time() < deadline:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.settimeout(10.0)
            try:
                sock.connect(path)
                return sock
            except OSError as exc:  # not created yet / not listening yet
                last = exc
                sock.close()
                time.sleep(0.05)
        raise QMPError(f"could not connect to QMP socket {path}: {last}")

    def _read_message(self):
        line = self.fh.readline()
        if not line:
            raise QMPError("QMP connection closed while waiting for a reply")
        return json.loads(line.decode("utf-8"))

    def execute(self, command, arguments=None):
        request = {"execute": command}
        if arguments is not None:
            request["arguments"] = arguments
        self.fh.write((json.dumps(request) + "\n").encode("utf-8"))
        self.fh.flush()
        # Skip asynchronous events, if any, until the command reply arrives.
        while True:
            message = self._read_message()
            if "event" in message:
                continue
            if "error" in message:
                raise QMPError(f"{command}: {message['error']}")
            return message.get("return")

    def close(self):
        try:
            self.fh.close()
        finally:
            self.sock.close()


# HMP text samples.  These are the ones with no structured QMP equivalent and
# therefore the ones the P5-4 parser has to consume.
HMP_QUERIES = [
    ("info-registers", "info registers"),
    ("info-mem", "info mem"),
    ("info-tlb", "info tlb"),
    ("info-cpus", "info cpus"),
]

# Structured QMP samples, kept as a bonus / for regression comparison.
QMP_QUERIES = [
    "query-version",
    "query-status",
    "query-cpus-fast",
    "query-memory-size-summary",
]


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2

    socket_path, out_dir = argv[1], argv[2]
    os.makedirs(out_dir, exist_ok=True)

    qmp = QMP(socket_path)
    try:
        with open(os.path.join(out_dir, "hmp-responses.jsonl"), "w",
                  encoding="utf-8") as raw:
            for name, command in HMP_QUERIES:
                reply = qmp.execute("human-monitor-command",
                                    {"command-line": command})
                text = reply if reply is not None else ""
                with open(os.path.join(out_dir, name + ".txt"), "w",
                          encoding="utf-8") as handle:
                    handle.write(text)
                raw.write(json.dumps(
                    {"command": command, "return": text}) + "\n")
                print(f"[qmp] {name}: {len(text.splitlines())} line(s)")

        for command in QMP_QUERIES:
            reply = qmp.execute(command)
            with open(os.path.join(out_dir, "qmp-" + command + ".json"), "w",
                      encoding="utf-8") as handle:
                json.dump(reply, handle, indent=2)
                handle.write("\n")
            print(f"[qmp] {command}: saved")
    finally:
        qmp.close()

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

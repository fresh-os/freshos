"""Boot FreshOS under QEMU and observe it.

A `Boot` stages an ESP into a private temporary directory, starts QEMU through
run-arm.sh (so tests boot exactly what `./run-arm.sh` boots), captures the
serial console, and talks to the kernel's MCP bridge over its Unix socket.

Test classes subclass `FreshOSTestCase`, which boots once per class and shares
that boot between the class's tests.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any, ClassVar

if sys.version_info < (3, 14):
    raise SystemExit(
        f"FreshOS tests need Python 3.14 or newer; this is {sys.version.split()[0]}"
    )

REPO = Path(__file__).resolve().parent.parent
RUN_ARM = REPO / "run-arm.sh"

# Userbins that exist only for tests. They are built and staged for test boots
# and listed as optional in init's service table, so normal boots never run them.
TEST_ELFS: tuple[str, ...] = ("probe-bad", "probe-chan")

ANSI = re.compile(r"\x1b\[[0-9;?=]*[A-Za-z]|\x1b[()][A-Za-z0-9]")

_built = False


def build_once() -> None:
    """Build the kernel and every userbin, including test-only ones, once per run."""
    global _built
    if _built:
        return
    env = {**os.environ, "EXTRA_ELFS": " ".join(TEST_ELFS), "BUILD_ONLY": "1"}
    subprocess.run([str(RUN_ARM)], cwd=REPO, env=env, check=True)
    _built = True


class McpError(Exception):
    pass


class McpClient:
    """A minimal MCP client for the bridge's newline-delimited JSON-RPC."""

    def __init__(self, path: Path, timeout: float = 15.0) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(str(path))
        self._buf = b""
        self._next_id = 0
        self._request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "freshos-tests", "version": "1"},
            },
        )
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def view(self, name: str) -> Any:
        """Call one of the bridge's read-only tools and decode its JSON."""
        result = self._request("tools/call", {"name": name, "arguments": {}})
        return json.loads(result["content"][0]["text"])

    def close(self) -> None:
        self.sock.close()

    def _send(self, message: dict[str, Any]) -> None:
        self.sock.sendall(json.dumps(message).encode() + b"\n")

    def _request(self, method: str, params: dict[str, Any]) -> Any:
        self._next_id += 1
        request_id = self._next_id
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        while True:
            while b"\n" not in self._buf:
                chunk = self.sock.recv(65536)
                if not chunk:
                    raise McpError("MCP socket closed")
                self._buf += chunk
            line, self._buf = self._buf.split(b"\n", 1)
            reply = json.loads(line)
            if reply.get("id") != request_id:
                continue
            if "error" in reply:
                raise McpError(reply["error"])
            return reply["result"]


class Boot:
    """One QEMU boot of FreshOS with its own ESP, sockets and UEFI variables."""

    def __init__(
        self, *, omit: tuple[str, ...] = (), extra_files: dict[str, bytes] | None = None
    ) -> None:
        self.omit = omit
        self.extra_files = extra_files or {}
        # tempfile's default directory keeps "<dir>/mcp.sock" well under the
        # 104-byte limit macOS puts on Unix socket paths.
        self.dir = Path(tempfile.mkdtemp(prefix="fos-"))
        self.sock = self.dir / "mcp.sock"
        self.lines: list[str] = []
        self._cond = threading.Condition()
        self.proc: subprocess.Popen[bytes] | None = None
        self._mcp: McpClient | None = None

    def start(self) -> None:
        build_once()
        extra_dir = self.dir / "extra"
        extra_dir.mkdir()
        for name, data in self.extra_files.items():
            (extra_dir / name).write_bytes(data)
        env = {
            **os.environ,
            "SKIP_BUILD": "1",
            "ESP_DIR": str(self.dir / "esp"),
            "MCP_SOCK": str(self.sock),
            "EXTRA_ELFS": " ".join(TEST_ELFS),
            "OMIT_ELFS": " ".join(self.omit),
            "EXTRA_FILES_DIR": str(extra_dir),
            "OVMF_VARS": str(self.dir / "vars.fd"),
        }
        self.proc = subprocess.Popen(
            [str(RUN_ARM), "-display", "none"],
            cwd=REPO,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self) -> None:
        assert self.proc is not None and self.proc.stdout is not None
        pending = b""
        while chunk := self.proc.stdout.read1(4096):
            pending += chunk
            *complete, pending = pending.split(b"\n")
            with self._cond:
                for raw in complete:
                    text = ANSI.sub("", raw.decode("utf-8", "replace")).rstrip("\r")
                    self.lines.append(text)
                self._cond.notify_all()

    def wait_for_log(self, pattern: str, timeout: float = 30.0) -> re.Match[str]:
        """Wait for a serial line matching `pattern`; fail with the log tail on timeout."""
        regex = re.compile(pattern)
        deadline = time.monotonic() + timeout
        seen = 0
        with self._cond:
            while True:
                for line in self.lines[seen:]:
                    if match := regex.search(line):
                        return match
                seen = len(self.lines)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    tail = "\n".join(self.lines[-40:])
                    raise AssertionError(
                        f"timed out after {timeout}s waiting for /{pattern}/; last lines:\n{tail}"
                    )
                self._cond.wait(remaining)

    def find_logs(self, pattern: str) -> list[re.Match[str]]:
        regex = re.compile(pattern)
        with self._cond:
            return [m for line in self.lines if (m := regex.search(line))]

    def send_keys(self, text: str) -> None:
        """Type into the console UART, which the in-kernel keyboard driver reads."""
        assert self.proc is not None and self.proc.stdin is not None
        self.proc.stdin.write(text.encode())
        self.proc.stdin.flush()

    def mcp(self) -> McpClient:
        if self._mcp is None:
            deadline = time.monotonic() + 30
            while not self.sock.exists():
                if time.monotonic() > deadline:
                    raise AssertionError("MCP socket never appeared")
                time.sleep(0.1)
            self._mcp = McpClient(self.sock)
        return self._mcp

    def stop(self) -> None:
        if self._mcp is not None:
            self._mcp.close()
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        shutil.rmtree(self.dir, ignore_errors=True)


class FreshOSTestCase(unittest.TestCase):
    """Boots FreshOS once for the whole class."""

    boot_options: ClassVar[dict[str, Any]] = {}
    ready_pattern: ClassVar[str] = r"\[mcp\] listening"
    boot: ClassVar[Boot]

    @classmethod
    def setUpClass(cls) -> None:
        cls.boot = Boot(**cls.boot_options)
        cls.boot.start()
        try:
            cls.boot.wait_for_log(cls.ready_pattern, timeout=120)
        except BaseException:
            cls.boot.stop()
            raise

    @classmethod
    def tearDownClass(cls) -> None:
        cls.boot.stop()

    def services(self) -> dict[str, dict[str, Any]]:
        return {service["name"]: service for service in self.boot.mcp().view("services")}

    def wait_until(
        self, condition: Callable[[], bool], timeout: float = 10.0, message: str = "condition"
    ) -> None:
        deadline = time.monotonic() + timeout
        while not condition():
            if time.monotonic() > deadline:
                self.fail(f"timed out after {timeout}s waiting for {message}")
            time.sleep(0.2)

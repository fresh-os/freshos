---
title: "plan: EL0 isolation foundation (rung 1, first slice)"
type: plan
date: 2026-09-26
---

# EL0 Isolation Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run `init`, `ping`, `pong`, `pulse` and `fault` as isolated EL0 binaries, each in its own address space, reaching the kernel only through validated syscalls and granted channel handles, all proven by automated QEMU tests.

**Architecture:** A private top-level page table per task shares the kernel's (EL1-only) mappings and owns one 1 GiB user window at 16 GiB. Userbins are linked there and loaded page by page with W^X. Syscalls enter through one frame-based path that can switch tasks on exit, which gives direct hand-off for IPC. A per-task handle table, filled by `init` through `spawn`, decides which channels a task may use. The kernel keeps a task registry (the facts). `init`, now an ordinary EL0 binary, keeps the service table (the policy).

**Tech Stack:** Rust nightly (edition 2024, `no_std`, `build-std`), aarch64 assembly, QEMU 11.1.1 with HVF, Python 3.14 standard library for tests.

**Spec:** `docs/plans/2026-09-26-el0-isolation-design.md`. Read it before starting; this plan argues from it. Decisions 0002–0006 in `docs/decisions/` are binding.

## Global Constraints

- **Python:** 3.14, standard library only.
- **No new external crates.** `serde_json` is already present, and userbins depend only on `freshos-rt`.
- **Userbins** are `#![no_std]`, `#![no_main]`, with no allocator.
- **Rust:** nightly, edition 2024. Build through `rustup run nightly`, as `run-arm.sh` does.
- **User window:** `0x4_0000_0000..0x4_4000_0000` (slot 16 of a `T0SZ=28` top-level table). Userbins are linked at `0x4_0000_0000`.
- **User stack:** 64 KiB at the top of the window, with the page below it unmapped as a guard page.
- **Kernel stack:** 16 KiB per task.
- **Limits:** at most 16 tasks (`MAX_TASKS`), at most 16 handles per task, logs capped at 256 bytes.
- **Message layout is unchanged:** `u32` tag, `u16` sender, `u16` length, `[u64; 4]` payload. The kernel always overwrites `sender`.
- **Memory protection:**
  - W^X: a segment that is both writable and executable is refused.
  - PAN on: the kernel never dereferences user virtual addresses.
  - User pages are not-global (`nG`), and each task's ASID is its task slot number.
- **One receiving process per channel.** RECV moves on grant and returns to its grantor when the receiver exits.
- **The kernel requires only `init`,** and there are no built-in fallbacks (decision 0006).
- **Warnings:** no new build or clippy warnings. The baseline before Task 1 is 33 build warnings (aarch64 kernel) and 64 clippy warnings.
- **Every task ends with `./test.sh` passing** and a commit in the repo's style: an effect-describing title, and a body that explains why.

## Review Focus

These are the conditions the spec implies but that the feature tests don't otherwise exercise. Each has a test in the task that owns it.

1. **A malformed ELF** (a writable and executable segment, or a segment outside the window) staged as a service. The kernel refuses it with a reason, `init` logs it, and nothing else is affected. Test: Task 7, `test_malformed_elf_is_refused`.
2. **A user stack overflow** hits the guard page and terminates the task with `fault`. It never overwrites other memory. Test: Task 5, `probe-bad-stack` in `test_isolation`.
3. **A crash-looping supervised service** (`fault` restarts forever) doesn't leak frames. Free frames are identical at the same point of successive restart cycles. Test: Task 7, `test_crash_loop_does_not_leak_frames`.
4. **Hostile log input** is contained. Over-long text is truncated. Invalid UTF-8 and control characters (including an embedded newline trying to forge an `[init]` line) become `?`. A bad pointer returns `BadPointer`. Test: Task 6, `probe-chan` log cases.
5. **A full channel.** The 17th unreceived send returns `Full` immediately, without blocking or losing the other 16. Test: Task 6, `queue-full` case.

---

## File map

| File | Responsibility | Task |
|---|---|---|
| `.python-version`, `test.sh`, `tests/harness.py`, `tests/test_*.py` | Test harness and tests | 1+ |
| `run-arm.sh` | One definition of build, stage and boot, with test overrides | 1, 5, 6 |
| `lib/abi/` (`freshos-abi`) | Every type and number that crosses the kernel/user boundary | 2 |
| `lib/rt/` (`freshos-rt`) | Userbin runtime: entry, panic, syscall wrappers, `log!` | 5 |
| `kernel/src/boot_images.rs` | Every `.ELF` in `\EFI\FreshOS\`, loaded before `exit_boot_services` | 3 |
| `kernel/src/arch/aarch64/addrspace.rs` | Per-task page tables, mapping, translation, user copies | 4 |
| `kernel/src/elf.rs` | ELF validation; `load_into` maps segments into an address space | 4 |
| `kernel/src/arch/aarch64/context.rs` | Tasks, scheduler, EL0 spawn, waits, hand-off, exit | 4–7 |
| `kernel/src/arch/aarch64/paging.rs` | Firmware table setup: WXN off, PAN on, `TCR.A1=0` | 4 |
| `kernel/src/arch/aarch64/exception.s`, `syscall.rs` | Frame-based syscall entry | 5 |
| `kernel/src/syscalls.rs` | Syscall semantics and validation (portable) | 5–7 |
| `kernel/src/handles.rs` | Per-task handle tables | 6 |
| `kernel/src/ipc.rs` | Channels: stamping, receivers, user-wait delivery, delivery metric | 2, 6, 7 |
| `kernel/src/registry.rs` | Kernel task and service registry (replaces `task_names.rs`) | 7 |
| `userbins/{init,ping,pong,pulse,fault,probe-bad,probe-chan}/` | The services and test probes | 5–7 |
| Deleted: `service_abi.rs`, `init_abi.rs`, `task_names.rs`, built-in probes in `arm_tasks.rs` | – | 5–7 |

---

### Task 1: Test harness and baseline

Every later task lands with tests, so the harness comes first. This task only observes today's system.

**Files:**
- Create: `.python-version`, `test.sh`, `tests/harness.py`, `tests/test_boot.py`, `tests/test_latency.py`, `tests/test_trace.py`
- Modify: `run-arm.sh` (full rewrite below), `.gitignore`

**Interfaces:**
- Produces:
  - `run-arm.sh` environment overrides: `ESP_DIR`, `MCP_SOCK`, `EXTRA_ELFS`, `OMIT_ELFS`, `EXTRA_FILES_DIR`, `OVMF_VARS`, `SKIP_BUILD`, `BUILD_ONLY`, `FRESHOS_ACCEL`.
  - `harness.Boot(omit=(), extra_files={})` with `.start()`, `.stop()`, `.wait_for_log(pattern, timeout=30.0) -> re.Match`, `.find_logs(pattern) -> list[re.Match]`, `.send_keys(text)`, `.mcp() -> McpClient`.
  - `McpClient.view(name) -> Any`.
  - `harness.FreshOSTestCase`, with class attributes `boot_options` and `ready_pattern`, and methods `services() -> dict[str, dict]` and `wait_until(cond, timeout=10.0, message="")`.
  - `harness.TEST_ELFS: tuple[str, ...]`, which later tasks extend.

- [ ] **Step 1: Pin Python.** Create `.python-version`:

```
3.14
```

- [ ] **Step 2: Rewrite `run-arm.sh`** so the harness can reuse it. Full content:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Build FreshOS for aarch64, stage an ESP, and boot it in QEMU.
#
# Optional environment overrides (tests/harness.py uses these):
#   ESP_DIR          where to stage the ESP                    (default: ./esp-arm)
#   MCP_SOCK         the MCP bridge's Unix socket              (default: ./mcp.sock)
#   EXTRA_ELFS       extra userbin packages, without "freshos-", to build and stage
#   OMIT_ELFS        userbin packages to leave off the ESP, e.g. "init"
#   EXTRA_FILES_DIR  files copied as-is into \EFI\FreshOS\ (e.g. malformed ELFs)
#   OVMF_VARS        writable UEFI variable store              (default: ./edk2-arm-vars.fd)
#   SKIP_BUILD=1     stage what is already built
#   BUILD_ONLY=1     build, then exit without staging or booting
#   FRESHOS_ACCEL    hvf (default) or tcg

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OVMF_CODE="/opt/homebrew/share/qemu/edk2-aarch64-code.fd"
OVMF_VARS_SRC="/opt/homebrew/share/qemu/edk2-arm-vars.fd"
OVMF_VARS="${OVMF_VARS:-$SCRIPT_DIR/edk2-arm-vars.fd}"
ESP_ROOT="${ESP_DIR:-$SCRIPT_DIR/esp-arm}"
MCP_SOCK="${MCP_SOCK:-$SCRIPT_DIR/mcp.sock}"
ACCEL="${FRESHOS_ACCEL:-hvf}"

PROFILE="debug"
CARGO_FLAGS=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
    shift
fi

# Every userbin, by package name without the "freshos-" prefix.
USERBINS="init pong pulse fault ${EXTRA_ELFS:-}"

TARGET_DIR="$SCRIPT_DIR/target/aarch64-unknown-uefi/$PROFILE"
USER_TARGET_DIR="$SCRIPT_DIR/target/aarch64-unknown-none/$PROFILE"
BOOT_DIR="$ESP_ROOT/EFI/BOOT"
FRESHOS_DIR="$ESP_ROOT/EFI/FreshOS"

# ESP file name for a userbin: upper case, no hyphens, at most 8 characters (FAT 8.3).
elf_name() {
    local n="${1//-/}"
    n="$(printf '%s' "$n" | tr '[:lower:]' '[:upper:]')"
    printf '%s.ELF' "${n:0:8}"
}

if [[ -z "${SKIP_BUILD:-}" ]]; then
    echo ":: Building FreshOS kernel for aarch64 ($PROFILE)..."
    rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi $CARGO_FLAGS
    for bin in $USERBINS; do
        echo ":: Building $bin for aarch64 ($PROFILE)..."
        rustup run nightly cargo build --package "freshos-$bin" --target aarch64-unknown-none $CARGO_FLAGS
    done
fi
if [[ -n "${BUILD_ONLY:-}" ]]; then
    exit 0
fi

echo ":: Preparing UEFI boot image in $ESP_ROOT..."
rm -rf "$FRESHOS_DIR"
mkdir -p "$BOOT_DIR" "$FRESHOS_DIR"
cp "$TARGET_DIR/freshos-kernel.efi" "$BOOT_DIR/BOOTAA64.EFI"
for bin in $USERBINS; do
    if [[ " ${OMIT_ELFS:-} " == *" $bin "* ]]; then
        echo ":: Omitting $bin"
        continue
    fi
    cp "$USER_TARGET_DIR/freshos-$bin" "$FRESHOS_DIR/$(elf_name "$bin")"
done
if [[ -n "${EXTRA_FILES_DIR:-}" ]] && compgen -G "$EXTRA_FILES_DIR/*" > /dev/null; then
    cp "$EXTRA_FILES_DIR"/* "$FRESHOS_DIR/"
fi

if [ ! -f "$OVMF_VARS" ]; then
    echo ":: Copying UEFI vars..."
    cp "$OVMF_VARS_SRC" "$OVMF_VARS"
fi

CPU="host"
if [[ "$ACCEL" == "tcg" ]]; then
    CPU="max"
fi

echo ":: Launching QEMU aarch64 ($ACCEL, serial on stdio, MCP on $MCP_SOCK)..."
exec qemu-system-aarch64 \
    -machine virt,accel="$ACCEL",highmem=off,gic-version=3 \
    -cpu "$CPU" \
    -m 512M \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$OVMF_VARS" \
    -device ramfb \
    -global virtio-mmio.force-legacy=false \
    -display cocoa \
    -device qemu-xhci \
    -device usb-kbd \
    -serial mon:stdio \
    -serial unix:"$MCP_SOCK",server=on,wait=off \
    -drive format=raw,file=fat:rw:"$ESP_ROOT" \
    "$@"
```

`gic-version=3` is now explicit. HVF already defaults to it, and TCG needs it, because the `qemu_virt` board layer drives a GICv3.

- [ ] **Step 3: Check the script still boots by hand.** Run `./run-arm.sh -display none` for about 20 s, then Ctrl-A X. Expected: the usual boot log, ending with `[init] services launched` and `[mcp] listening on the second UART`.

- [ ] **Step 4: Write `tests/harness.py`:**

```python
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
TEST_ELFS: tuple[str, ...] = ()

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
```

- [ ] **Step 5: Write `test.sh`** and make it executable (`chmod +x test.sh`):

```bash
#!/usr/bin/env bash
# Build FreshOS and run the automated QEMU tests in tests/.
# Extra arguments go to unittest, e.g. ./test.sh -k latency
set -euo pipefail
cd "$(dirname "$0")"
exec python3 -m unittest discover -s tests -t tests "$@"
```

- [ ] **Step 6: Write the first tests.** These describe today's system.

`tests/test_boot.py`:

```python
from harness import FreshOSTestCase

# Services that stay running once booted. pulse and fault come and go by design.
LONG_RUNNING = ("kbd", "comp", "shell", "dash", "ping", "pong", "mcp")


class BootTest(FreshOSTestCase):
    def test_every_long_running_service_is_running(self) -> None:
        def all_running() -> bool:
            services = self.services()
            return all(services.get(name, {}).get("running") for name in LONG_RUNNING)

        self.wait_until(all_running, timeout=20, message=f"all of {LONG_RUNNING} running")

    def test_tasks_are_named_by_the_kernel(self) -> None:
        names = {task["name"] for task in self.boot.mcp().view("tasks")}
        self.assertLessEqual(set(LONG_RUNNING), names)
```

`tests/test_latency.py`:

```python
from harness import FreshOSTestCase


class LatencyBaseline(FreshOSTestCase):
    def test_ping_pong_round_trip_is_measured(self) -> None:
        def latest_us() -> int:
            return self.boot.mcp().view("metrics")["ipc_round_trip"]["latest_us"]

        self.wait_until(lambda: latest_us() > 0, timeout=20, message="a measured round trip")
        print(f"\nBASELINE ping/pong round trip: {latest_us()} us")
```

`tests/test_trace.py`:

```python
from harness import FreshOSTestCase


class TraceTest(FreshOSTestCase):
    def test_pings_carry_the_real_sender(self) -> None:
        def pings() -> list[dict]:
            return [m for m in self.boot.mcp().view("message_trace") if m["type"] == "PING"]

        self.wait_until(lambda: bool(pings()), timeout=20, message="a PING in the trace")
        for message in pings():
            self.assertEqual(message["from"]["name"], "ping")
            if message["to"] is not None:
                self.assertEqual(message["to"]["name"], "pong")
```

- [ ] **Step 7: Run the tests.** Run `./test.sh -v`. Expected: 4 tests, `OK`, and a `BASELINE ping/pong round trip: N us` line, where N is roughly 14000. **Write N down;** the commit message and Task 6 need it.

- [ ] **Step 8: Ignore test droppings.** Add `/mcp.sock` to `.gitignore` if it's not already there. The harness writes only to temporary directories.

- [ ] **Step 9: Commit.**

```bash
git add .python-version test.sh tests run-arm.sh .gitignore
git commit -m "Add an automated QEMU test harness and record the IPC baseline" \
  -m "Tests boot FreshOS through run-arm.sh (now configurable through environment overrides) into a private temporary ESP, capture the serial log, and query the MCP bridge. First tests: long-running services are up and named, and the ping/pong trace carries real identities. Baseline ping/pong round trip: N us (replace N)."
```

---

### Task 2: The shared ABI crate

This creates one definition of everything that crosses the kernel/user boundary, and makes the kernel stamp every message's sender itself.

**Files:**
- Create: `lib/abi/Cargo.toml`, `lib/abi/src/lib.rs`
- Modify: `Cargo.toml` (workspace members), `kernel/Cargo.toml`, `kernel/src/ipc.rs:18-67` (message type and tags), `kernel/src/ipc.rs` `send` (stamping)

**Interfaces:**
- Produces, in `freshos_abi`:
  - `sys::{SEND, RECV, TRY_RECV, YIELD, EXIT, TIME_NS, LOG, CHANNEL_CREATE, SPAWN}: u64`;
  - `Error`, `#[repr(i64)]`, with `from_code(i64) -> Error`;
  - `Message`, with `empty()`, `new(tag)`, `with_data(slot, value)`, `with_name(&str)` and `name() -> ([u8; 16], usize)`;
  - `tag::*`;
  - `Handle(u32)`;
  - `Rights`, with `SEND`, `RECV`, `NONE`, `contains`, `union` and `without`;
  - `Grant { handle, rights }` and `SpawnRequest`;
  - `ExitReason { Clean = 1, Fault = 2, Panic = 3 }`, with `from_u64` and `as_str`;
  - the constants `USER_BASE`, `USER_SIZE`, `USER_STACK_SIZE`, `MAX_HANDLES`, `NAME_LEN`, `MAX_LOG` and `MAX_BINARY_NAME`.
- Changes: `ipc::Message` is re-exported from `freshos_abi`, and `ipc::send` now overwrites `sender` with the current task.

- [ ] **Step 1: Create `lib/abi/Cargo.toml`:**

```toml
[package]
name = "freshos-abi"
version = "0.1.0"
edition = "2024"

[dependencies]
```

- [ ] **Step 2: Create `lib/abi/src/lib.rs`:**

```rust
//! The FreshOS kernel/user ABI: every number and type that crosses the
//! boundary, defined once and used by the kernel and every userbin
//! (decision 0006). Nothing here may be redeclared by hand elsewhere.
#![no_std]

/// Syscall numbers, passed in `x8` to `svc #0`.
pub mod sys {
    pub const SEND: u64 = 0;
    pub const RECV: u64 = 1;
    pub const TRY_RECV: u64 = 2;
    pub const YIELD: u64 = 3;
    pub const EXIT: u64 = 4;
    pub const TIME_NS: u64 = 5;
    pub const LOG: u64 = 6;
    pub const CHANNEL_CREATE: u64 = 7;
    pub const SPAWN: u64 = 8;
}

/// Syscall errors, returned in `x0` as negative numbers.
#[repr(i64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NoSuchSyscall = -1,
    NoSuchHandle = -2,
    NoRight = -3,
    BadPointer = -4,
    WouldBlock = -5,
    Timeout = -6,
    NotFound = -7,
    NotPermitted = -8,
    TableFull = -9,
    ReceiverTaken = -10,
    Full = -11,
    Invalid = -12,
    OutOfMemory = -13,
}

impl Error {
    pub const ALL: [Error; 13] = [
        Error::NoSuchSyscall,
        Error::NoSuchHandle,
        Error::NoRight,
        Error::BadPointer,
        Error::WouldBlock,
        Error::Timeout,
        Error::NotFound,
        Error::NotPermitted,
        Error::TableFull,
        Error::ReceiverTaken,
        Error::Full,
        Error::Invalid,
        Error::OutOfMemory,
    ];

    /// The error for a negative syscall result. Unknown codes map to `Invalid`.
    pub fn from_code(code: i64) -> Error {
        Error::ALL
            .into_iter()
            .find(|e| *e as i64 == code)
            .unwrap_or(Error::Invalid)
    }
}

/// Message tags.
pub mod tag {
    pub const PING: u32 = 1;
    pub const PONG: u32 = 2;
    pub const IRQ: u32 = 10;
    pub const MOUSE_RAW: u32 = 11;
    pub const MOUSE: u32 = 12;
    pub const KEY_DOWN: u32 = 20;
    pub const KEY_UP: u32 = 21;
    /// Kernel → init: payload[0] = task id, payload[1] = `ExitReason`.
    pub const TASK_EXITED: u32 = 100;
    /// Anyone with SEND on init's inbox → init: the service name, packed by `with_name`.
    pub const RESTART_REQUEST: u32 = 101;
}

/// A message: a tag and 32 bytes of inline payload. The kernel always
/// overwrites `sender` with the real sending task (0 = the kernel).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub tag: u32,
    pub sender: u16,
    pub len: u16,
    pub payload: [u64; 4],
}

impl Message {
    pub const fn empty() -> Self {
        Self { tag: 0, sender: 0, len: 0, payload: [0; 4] }
    }

    pub const fn new(tag: u32) -> Self {
        Self { tag, sender: 0, len: 0, payload: [0; 4] }
    }

    pub fn with_data(mut self, slot: usize, value: u64) -> Self {
        if slot < 4 {
            self.payload[slot] = value;
            let end = ((slot + 1) * 8) as u16;
            if end > self.len {
                self.len = end;
            }
        }
        self
    }

    /// Pack a name of up to `NAME_LEN` bytes into payload[0..2]; longer names are cut.
    pub fn with_name(mut self, name: &str) -> Self {
        let bytes = name.as_bytes();
        let n = bytes.len().min(NAME_LEN);
        let mut packed = [0u8; NAME_LEN];
        packed[..n].copy_from_slice(&bytes[..n]);
        self.payload[0] = u64::from_le_bytes(packed[0..8].try_into().unwrap_or([0; 8]));
        self.payload[1] = u64::from_le_bytes(packed[8..16].try_into().unwrap_or([0; 8]));
        self.len = n as u16;
        self
    }

    /// The name packed by `with_name`: the bytes and their length.
    pub fn name(&self) -> ([u8; NAME_LEN], usize) {
        let mut bytes = [0u8; NAME_LEN];
        bytes[0..8].copy_from_slice(&self.payload[0].to_le_bytes());
        bytes[8..16].copy_from_slice(&self.payload[1].to_le_bytes());
        (bytes, (self.len as usize).min(NAME_LEN))
    }
}

/// A slot in the calling task's handle table.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(pub u32);

/// What a handle allows.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const SEND: Rights = Rights(1);
    pub const RECV: Rights = Rights(2);

    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }
    pub const fn without(self, other: Rights) -> Rights {
        Rights(self.0 & !other.0)
    }
}

/// One handle for `spawn` to copy (SEND) or move (RECV) into the child.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub handle: Handle,
    pub rights: Rights,
}

/// `spawn`'s argument. The child's handles are the grants, in order, and its
/// `main` receives `arg`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SpawnRequest {
    pub name_ptr: u64,
    pub name_len: u64,
    pub binary_ptr: u64,
    pub binary_len: u64,
    pub grants_ptr: u64,
    pub grants_len: u64,
    pub arg: u64,
}

#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Clean = 1,
    Fault = 2,
    Panic = 3,
}

impl ExitReason {
    pub fn from_u64(value: u64) -> Option<ExitReason> {
        match value {
            1 => Some(ExitReason::Clean),
            2 => Some(ExitReason::Fault),
            3 => Some(ExitReason::Panic),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            ExitReason::Clean => "clean",
            ExitReason::Fault => "fault",
            ExitReason::Panic => "panic",
        }
    }
}

/// Every task's private window: 1 GiB at 16 GiB.
pub const USER_BASE: u64 = 0x4_0000_0000;
pub const USER_SIZE: u64 = 1 << 30;
pub const USER_STACK_SIZE: u64 = 64 * 1024;
pub const MAX_HANDLES: usize = 16;
pub const NAME_LEN: usize = 16;
pub const MAX_BINARY_NAME: usize = 32;
pub const MAX_LOG: usize = 256;
```

- [ ] **Step 3: Wire it in.**
  - Root `Cargo.toml`: add `"lib/abi"` to `members`.
  - `kernel/Cargo.toml` `[dependencies]`: add `freshos-abi = { path = "../lib/abi" }`.

- [ ] **Step 4: Make the kernel use the ABI's message.** In `kernel/src/ipc.rs`, replace everything from `pub const MSG_PING: u32 = 1;` through the end of `impl Message { … }` with:

```rust
pub use freshos_abi::Message;

pub const MSG_PING: u32 = freshos_abi::tag::PING;
pub const MSG_PONG: u32 = freshos_abi::tag::PONG;
pub const MSG_IRQ: u32 = freshos_abi::tag::IRQ;
pub const MSG_MOUSE_RAW: u32 = freshos_abi::tag::MOUSE_RAW;
pub const MSG_MOUSE: u32 = freshos_abi::tag::MOUSE;
pub const MSG_KEY_DOWN: u32 = freshos_abi::tag::KEY_DOWN;
pub const MSG_KEY_UP: u32 = freshos_abi::tag::KEY_UP;
```

  `Message::new` no longer reads the current task. Stamping moves into `send`, which also covers senders that build messages by hand. In `send`, replace `ch.buf[ch.head] = *msg;` with:

```rust
    // The kernel decides who sent a message, never the sender (spec §4).
    let mut stamped = *msg;
    stamped.sender = crate::arch::current_task() as u16;
    ch.buf[ch.head] = stamped;
```

- [ ] **Step 5: Build both boards.**

```bash
rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi 2>&1 | grep -E "^error|generated [0-9]+ warning"
rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi --no-default-features --features board-rpi4 2>&1 | grep -E "^error|generated [0-9]+ warning"
```

  Expected: no errors, and 33 warnings for each board. If `Message::empty()` is called in a `const` context in `ipc.rs` (`EMPTY_CHANNEL`), it still compiles, because it's a `const fn`.

- [ ] **Step 6: Run the tests.** Run `./test.sh`. Expected: `OK`, and `test_pings_carry_the_real_sender` still passes, now through kernel stamping.

- [ ] **Step 7: Commit.**

```bash
git add lib/abi Cargo.toml Cargo.lock kernel
git commit -m "Define the kernel/user ABI once, and stamp senders in the kernel" \
  -m "freshos-abi holds syscall numbers, errors, the message layout, handles, rights and spawn types, so no userbin ever copies a struct by hand again (decision 0006). ipc::send now overwrites every message's sender with the real task, instead of trusting Message::new, which also covers messages built by hand."
```

---

### Task 3: Boot images, and no built-in fallback for `init`

**Files:**
- Create: `kernel/src/boot_images.rs`
- Modify: `kernel/src/main.rs`:
  - the four `load_esp_file` calls (currently ~lines 217–243);
  - the `load_esp_file` function and `BootFile` (~lines 108–175);
  - the ELF loads (~lines 405–488);
  - the spawn block (~lines 508–517).
- Test: `tests/test_no_init.py`

**Interfaces:**
- Produces:
  - `boot_images::load_all()`, called before `exit_boot_services`;
  - `boot_images::find(name: &str) -> Option<&'static [u8]>`, case-insensitive on the ESP file name, e.g. `"PONG.ELF"`;
  - the boot log line `  Boot image NAME (N bytes)`;
  - the halt message `INIT.ELF missing from \EFI\FreshOS — nothing to run`.

- [ ] **Step 1: Write the failing test,** `tests/test_no_init.py`:

```python
import time

from harness import FreshOSTestCase


class NoInitTest(FreshOSTestCase):
    """Decision 0006: the kernel requires only init, and has no fallbacks."""

    boot_options = {"omit": ("init",)}
    ready_pattern = r"INIT\.ELF missing from"

    def test_nothing_else_runs(self) -> None:
        time.sleep(3)
        for tag in (r"\[init\]", r"\[kbd\]", r"\[comp\]", r"\[shell\]", r"\[mcp\]"):
            self.assertEqual(self.boot.find_logs(tag), [], f"{tag} ran without init")
```

- [ ] **Step 2: Run it and watch it fail.** Run `./test.sh -k NoInit`. Expected: a timeout waiting for `INIT.ELF missing from`, because today's kernel falls back to spawning the built-ins.

- [ ] **Step 3: Create `kernel/src/boot_images.rs`:**

```rust
/// Boot images: every `*.ELF` in `\EFI\FreshOS\`, read into memory while
/// UEFI's file access still exists. The kernel requires only INIT.ELF;
/// everything else is found here by name when init asks (decision 0006).
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use uefi::boot::{self, MemoryType};
use uefi::cstr16;
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};

use crate::serial::serial_println;

const MAX_IMAGES: usize = 16;
const NAME_LEN: usize = 16;

#[derive(Clone, Copy)]
struct Image {
    name: [u8; NAME_LEN],
    name_len: usize,
    ptr: *const u8,
    len: usize,
}

const EMPTY: Image = Image { name: [0; NAME_LEN], name_len: 0, ptr: core::ptr::null(), len: 0 };

struct Table(UnsafeCell<[Image; MAX_IMAGES]>);
// SAFETY: written only by `load_all` during single-threaded boot, read-only after.
unsafe impl Sync for Table {}

static IMAGES: Table = Table(UnsafeCell::new([EMPTY; MAX_IMAGES]));
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Read every `*.ELF` in `\EFI\FreshOS\`. Call before `exit_boot_services`.
pub fn load_all() {
    let Ok(mut fs) = boot::get_image_file_system(boot::image_handle()) else {
        serial_println!("  Boot images: no file system");
        return;
    };
    let Ok(mut root) = fs.open_volume() else {
        serial_println!("  Boot images: volume won't open");
        return;
    };
    let Some(mut dir) = root
        .open(cstr16!("\\EFI\\FreshOS"), FileMode::Read, FileAttribute::empty())
        .ok()
        .and_then(|handle| handle.into_directory())
    else {
        serial_println!("  Boot images: \\EFI\\FreshOS missing");
        return;
    };

    let mut info_buf = [0u8; 512];
    while let Ok(Some(info)) = dir.read_entry(&mut info_buf) {
        if !info.is_regular_file() {
            continue;
        }
        let mut name = [0u8; NAME_LEN];
        let mut name_len = 0;
        let mut ascii = true;
        for ch in info.file_name().iter() {
            let c = u16::from(*ch);
            if c >= 0x80 || name_len == NAME_LEN {
                ascii = false;
                break;
            }
            name[name_len] = c as u8;
            name_len += 1;
        }
        if !ascii || !name[..name_len].to_ascii_uppercase().ends_with(b".ELF") {
            continue;
        }
        let len = info.file_size() as usize;
        let Some(file) = dir
            .open(info.file_name(), FileMode::Read, FileAttribute::empty())
            .ok()
            .and_then(|handle| handle.into_regular_file())
        else {
            continue;
        };
        let Some(ptr) = read_whole(file, len) else { continue };

        let index = COUNT.load(Ordering::Relaxed);
        if index == MAX_IMAGES {
            serial_println!("  Boot images: more than {} ELFs, ignoring the rest", MAX_IMAGES);
            break;
        }
        unsafe { (*IMAGES.0.get())[index] = Image { name, name_len, ptr, len } };
        COUNT.store(index + 1, Ordering::Relaxed);
        serial_println!(
            "  Boot image {} ({} bytes)",
            core::str::from_utf8(&name[..name_len]).unwrap_or("?"),
            len
        );
    }
}

fn read_whole(mut file: uefi::proto::media::file::RegularFile, len: usize) -> Option<*const u8> {
    let pool = boot::allocate_pool(MemoryType::LOADER_DATA, len.max(1)).ok()?;
    let buf = unsafe { core::slice::from_raw_parts_mut(pool.as_ptr(), len) };
    match file.read(buf) {
        Ok(read) if read == len => Some(pool.as_ptr()),
        _ => None,
    }
}

/// The image whose ESP file name is `name` (case-insensitive), e.g. "PONG.ELF".
pub fn find(name: &str) -> Option<&'static [u8]> {
    let images = unsafe { &*IMAGES.0.get() };
    images[..COUNT.load(Ordering::Relaxed)]
        .iter()
        .find(|image| image.name[..image.name_len].eq_ignore_ascii_case(name.as_bytes()))
        .map(|image| unsafe { core::slice::from_raw_parts(image.ptr, image.len) })
}
```

  `FileInfo` is imported for the `read_entry` return type. If the compiler flags it as unused, remove it from the `use`.

- [ ] **Step 4: Use it in `main.rs`.**
  1. Add `mod boot_images;` next to `mod elf;`, under `#[cfg(target_arch = "aarch64")]`.
  2. Delete `struct BootFile`, `fn load_esp_file`, and the four `let init_file / pong_file / pulse_file / fault_file = load_esp_file(…)` blocks with their `serial_println!`s. Put `boot_images::load_all();` where the first of them was, before `boot::memory_map(…)`.
  3. In the ELF-loading section, replace each `init_file.as_ref()` / `pong_file.as_ref()` / `pulse_file.as_ref()` / `fault_file.as_ref()` pattern with `boot_images::find("INIT.ELF")`, `"PONG.ELF"`, `"PULSE.ELF"` and `"FAULT.ELF"`. Each `bytes` becomes the returned slice directly: `if let Some(bytes) = boot_images::find("PONG.ELF") { match elf::load_image(bytes) { … } }`.
  4. Replace the final spawn block:

```rust
    if let Some(image) = loaded_init {
        arch::context::spawn_with_arg(image.entry, init_abi::api_ptr() as u64);
    } else {
        serial_println!("INIT.ELF missing from \\EFI\\FreshOS — nothing to run");
        loop {
            arch::interrupt_disable();
            arch::halt();
        }
    }
```

  The six fallback `arch::context::spawn(arm_tasks::…)` calls are deleted.

- [ ] **Step 5: Build, then run the full suite.** Build as in Task 2, Step 5: no errors, and no new warnings. Remove imports that only the deleted code used, such as `CStr16`, `FileInfo` and `FileMode` in `main.rs`, if they're now reported as unused. Then run `./test.sh`. Expected: `OK`, including `NoInitTest`.

- [ ] **Step 6: Commit.**

```bash
git add kernel tests
git commit -m "Load every ELF on the ESP by name, and halt clearly without init" \
  -m "The kernel read four hardcoded files and, if INIT.ELF was missing, quietly spawned the built-in desktop instead. It now loads every *.ELF in \\EFI\\FreshOS\\ into a table of boot images, looked up by name, and without INIT.ELF it says so and halts: the kernel requires only init (decision 0006)."
```

---

### Task 4: Per-task address spaces, with `fault` at EL0

**Files:**
- Create: `kernel/src/arch/aarch64/addrspace.rs`
- Modify:
  - `.cargo/config.toml` (link address);
  - `kernel/src/arch/aarch64/mod.rs` (`pub mod addrspace;`);
  - `kernel/src/arch/aarch64/paging.rs` (`init`);
  - `kernel/src/elf.rs` (`load_into`);
  - `kernel/src/arch/aarch64/context.rs` (the Task struct, `spawn_el0`, TTBR0 switching, retire);
  - `kernel/src/init_abi.rs` (`spawn_fault_service`);
  - `kernel/src/service_abi.rs` (remove fault registration);
  - `kernel/src/main.rs` (remove the 2 MiB fault region);
  - `.gitignore` if needed.
- Test: `tests/test_isolation.py`

**Interfaces:**
- Consumes: `boot_images::find` (Task 3); `freshos_abi::{USER_BASE, USER_SIZE, USER_STACK_SIZE, Error}` (Task 2).
- Produces:
  - `addrspace::{AddressSpace, Perm, SpaceError, in_window, PAGE}`;
  - `AddressSpace::new(asid: u16) -> Result<Self, SpaceError>`;
  - `.ttbr0() -> u64`;
  - `.map_new_page(va: u64, perm: Perm) -> Result<u64, SpaceError>`;
  - `.translate(va: u64, write: bool) -> Option<u64>`;
  - `addrspace::copy_from_user(space, va, &mut [u8]) -> Result<(), Error>`, `copy_to_user(space, va, &[u8]) -> Result<(), Error>`, `read_user::<T: Copy>(space, va) -> Result<T, Error>` and `write_user::<T: Copy>(space, va, &T) -> Result<(), Error>`;
  - `elf::load_into(bytes, &mut AddressSpace) -> Result<u64, &'static str>`;
  - `context::spawn_el0(image: &[u8], arg0: u64, arg1: u64) -> Result<usize, SpawnError>`;
  - `context::SpawnError { NoSlot, OutOfMemory, BadImage(&'static str) }`;
  - `context::retire_current(reason: u64)`, which doesn't diverge;
  - the boot log line `    task N el0 asid=N entry=0x…`.

**Concepts for the implementer:**
- **The page table.** UEFI built one table (`T0SZ=28`: a 36-bit address space; the walk starts at level 1, with 64 entries of 1 GiB each). Each task's new level-1 table copies UEFI's entries for every slot except 16, so the kernel (identity-mapped RAM and devices, EL1-only) is identical in every space. Slot 16 points at level-2 and level-3 tables the task owns.
- **Page entries:**

| Bits | Meaning |
|---|---|
| 1:0 | `0b11`: a valid page |
| 4:2 | AttrIndx: memory type, copied from the firmware's RAM mapping |
| 7:6 | AP: `01` = EL1 and EL0 read-write; `11` = both read-only |
| 9:8 | SH: shareability, copied |
| 10 | AF: access flag |
| 11 | nG: not global, so the TLB entry is tagged with the ASID |
| 53 | PXN: EL1 may not execute (always set on user pages) |
| 54 | UXN: EL0 may not execute (set on data) |

- [ ] **Step 1: Write the failing test,** `tests/test_isolation.py`:

```python
from harness import FreshOSTestCase


class IsolationTest(FreshOSTestCase):
    def test_fault_runs_at_el0_in_its_own_space(self) -> None:
        self.boot.wait_for_log(r"task \d+ el0 asid=\d+ entry=0x4000", timeout=20)

    def test_fault_is_contained_and_restarted(self) -> None:
        self.boot.wait_for_log(r"service fault exited \(task \d+, reason=fault\)", timeout=20)
        self.wait_until(
            lambda: len(self.boot.find_logs(r"service fault exited")) >= 2,
            timeout=20,
            message="fault to crash twice (restarted in between)",
        )
        self.assertTrue(self.services()["pong"]["running"])
```

- [ ] **Step 2: Run it and watch it fail.** Run `./test.sh -k Isolation`. Expected: `test_fault_runs_at_el0_in_its_own_space` times out.

- [ ] **Step 3: Link userbins at 16 GiB.** Append to `.cargo/config.toml`:

```toml
# Userbins are linked at the start of every task's private 1 GiB window
# (0x4_0000_0000, slot 16 of the top-level page table). See the EL0 isolation spec.
[target.aarch64-unknown-none]
rustflags = ["-C", "link-arg=--image-base=0x400000000"]
```

  Verify with `/opt/homebrew/opt/llvm/bin/llvm-readelf -h -l target/aarch64-unknown-none/debug/freshos-pulse` after a build. Expected: `Type: EXEC`, entry `0x4000…`, and `LOAD` segments starting at `0x0000000400000000`. (This was verified while planning.) `init`, `pong` and `pulse` still load at EL1 through the old fixed-bias loader until they move; that has always worked because their code is PC-relative.

- [ ] **Step 4: Create `kernel/src/arch/aarch64/addrspace.rs`:**

```rust
/// Per-task address spaces (EL0 isolation spec, section 3).
///
/// Every task's top-level table copies the kernel's top-level entries, so the
/// kernel (RAM and devices, EL1-only) looks the same in every address space.
/// Slot 16, the 1 GiB window at 0x4_0000_0000, is private to the task: its
/// code, data and stack live there, mapped EL0-accessible and not-global, so
/// the TLB tags them with the task's ASID.
///
/// The kernel never dereferences a user virtual address. `copy_from_user` and
/// `copy_to_user` translate through the task's own table and copy through the
/// kernel's identity mapping of the frame, so PAN never has to be lifted.
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use freshos_abi::{Error, USER_BASE, USER_SIZE};

use crate::frame_alloc;

const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;
const AP_EL0_RW: u64 = 0b01 << 6;
const AP_EL0_RO: u64 = 0b11 << 6;
const AP_MASK: u64 = 0b11 << 6;
const AF: u64 = 1 << 10;
const NG: u64 = 1 << 11;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;
/// AttrIndx (bits 4:2) and shareability (bits 9:8): the memory type.
const MEMORY_TYPE_MASK: u64 = (0b111 << 2) | (0b11 << 8);

pub const PAGE: u64 = 4096;
pub const USER_L1_INDEX: usize = (USER_BASE >> 30) as usize;

static KERNEL_L1: AtomicU64 = AtomicU64::new(0);
static L1_ENTRIES: AtomicUsize = AtomicUsize::new(0);
static RAM_MEMORY_TYPE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perm {
    ReadOnly,
    ReadWrite,
    ReadExec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceError {
    OutOfMemory,
    OutsideWindow,
    AlreadyMapped,
}

/// Record the kernel's top-level table and the firmware's memory type for
/// RAM, sampled from the mapping of `ram_va`. Refuses geometries this module
/// doesn't handle, and a firmware map that already uses slot 16.
pub fn init(kernel_l1: u64, t0sz: u32, ram_va: u64) -> Result<(), &'static str> {
    // The walk must start at level 1 (T0SZ 25..=33), and the address space
    // must reach past 17 GiB for the user window (T0SZ <= 29).
    if !(25..=29).contains(&t0sz) {
        return Err("unsupported firmware page-table geometry");
    }
    if read_entry(kernel_l1, USER_L1_INDEX) & VALID != 0 {
        return Err("firmware map already uses the user window (slot 16)");
    }
    let leaf = kernel_leaf(kernel_l1, ram_va).ok_or("kernel RAM is not mapped")?;
    KERNEL_L1.store(kernel_l1, Ordering::Relaxed);
    L1_ENTRIES.store(1 << (34 - t0sz), Ordering::Relaxed);
    RAM_MEMORY_TYPE.store(leaf & MEMORY_TYPE_MASK, Ordering::Relaxed);
    Ok(())
}

/// Whether `[va, va + len)` lies entirely inside the user window.
pub fn in_window(va: u64, len: u64) -> bool {
    va >= USER_BASE && len <= USER_SIZE && va - USER_BASE <= USER_SIZE - len
}

pub struct AddressSpace {
    l1: u64,
    asid: u16,
    /// Every frame this space owns: page tables and data. Freed on drop.
    frames: Vec<u64>,
}

impl AddressSpace {
    pub fn new(asid: u16) -> Result<Self, SpaceError> {
        let mut space = AddressSpace { l1: 0, asid, frames: Vec::new() };
        space.l1 = space.alloc_zeroed()?;
        let kernel = KERNEL_L1.load(Ordering::Relaxed);
        for index in 0..L1_ENTRIES.load(Ordering::Relaxed) {
            if index != USER_L1_INDEX {
                write_entry(space.l1, index, read_entry(kernel, index));
            }
        }
        Ok(space)
    }

    /// The TTBR0_EL1 value for this space: table address plus ASID in bits 63:48.
    pub fn ttbr0(&self) -> u64 {
        self.l1 | ((self.asid as u64) << 48)
    }

    /// Map a fresh, zeroed page at `va` with `perm`. Returns its physical address.
    pub fn map_new_page(&mut self, va: u64, perm: Perm) -> Result<u64, SpaceError> {
        if va % PAGE != 0 || !in_window(va, PAGE) {
            return Err(SpaceError::OutsideWindow);
        }
        let l3 = self.l3_table_for(va)?;
        let index = ((va >> 12) & 0x1FF) as usize;
        if read_entry(l3, index) & VALID != 0 {
            return Err(SpaceError::AlreadyMapped);
        }
        let pa = self.alloc_zeroed()?;
        let access = match perm {
            Perm::ReadOnly => AP_EL0_RO | PXN | UXN,
            Perm::ReadWrite => AP_EL0_RW | PXN | UXN,
            Perm::ReadExec => AP_EL0_RO | PXN,
        };
        let memory_type = RAM_MEMORY_TYPE.load(Ordering::Relaxed);
        write_entry(l3, index, pa | VALID | TABLE_OR_PAGE | AF | NG | memory_type | access);
        Ok(pa)
    }

    /// Make every mapping made so far visible to the table walker. Call once
    /// after a batch of `map_new_page` and before the space is first used.
    pub fn publish(&self) {
        unsafe { core::arch::asm!("dsb ishst", options(nostack)) };
    }

    /// The physical address behind `va`, if EL0 may read it (or write it, when
    /// `write`). This is the check every user pointer passes through.
    pub fn translate(&self, va: u64, write: bool) -> Option<u64> {
        if !in_window(va, 1) {
            return None;
        }
        let l2 = read_entry(self.l1, USER_L1_INDEX);
        if l2 & VALID == 0 {
            return None;
        }
        let l3 = read_entry(l2 & ADDR_MASK, ((va >> 21) & 0x1FF) as usize);
        if l3 & VALID == 0 {
            return None;
        }
        let pte = read_entry(l3 & ADDR_MASK, ((va >> 12) & 0x1FF) as usize);
        if pte & VALID == 0 {
            return None;
        }
        let ap = pte & AP_MASK;
        let allowed = if write { ap == AP_EL0_RW } else { ap == AP_EL0_RW || ap == AP_EL0_RO };
        allowed.then(|| (pte & ADDR_MASK) | (va & (PAGE - 1)))
    }

    fn l3_table_for(&mut self, va: u64) -> Result<u64, SpaceError> {
        let l2 = self.child_table(self.l1, USER_L1_INDEX)?;
        self.child_table(l2, ((va >> 21) & 0x1FF) as usize)
    }

    fn child_table(&mut self, table: u64, index: usize) -> Result<u64, SpaceError> {
        let entry = read_entry(table, index);
        if entry & VALID != 0 {
            return Ok(entry & ADDR_MASK);
        }
        let child = self.alloc_zeroed()?;
        write_entry(table, index, child | VALID | TABLE_OR_PAGE);
        Ok(child)
    }

    fn alloc_zeroed(&mut self) -> Result<u64, SpaceError> {
        let frame = frame_alloc::allocate().ok_or(SpaceError::OutOfMemory)?;
        unsafe { core::ptr::write_bytes(frame as *mut u8, 0, PAGE as usize) };
        self.frames.push(frame);
        Ok(frame)
    }
}

impl Drop for AddressSpace {
    /// The caller must already have moved TTBR0 off this space (see
    /// `context::retire_current`): its tables are freed here.
    fn drop(&mut self) {
        let asid = (self.asid as u64) << 48;
        unsafe {
            core::arch::asm!("dsb ishst", "tlbi aside1is, {0}", "dsb ish", "isb", in(reg) asid, options(nostack));
        }
        for &frame in &self.frames {
            unsafe { frame_alloc::deallocate(frame) };
        }
    }
}

/// Check that every page of `[va, va + len)` is EL0-accessible (writable, if
/// `write`) before any byte is copied. Then no copy can fail halfway.
fn check_range(space: &AddressSpace, va: u64, len: u64, write: bool) -> Result<(), Error> {
    if !in_window(va, len) {
        return Err(Error::BadPointer);
    }
    if len == 0 {
        return Ok(());
    }
    let mut page = va & !(PAGE - 1);
    while page < va + len {
        space.translate(page, write).ok_or(Error::BadPointer)?;
        page += PAGE;
    }
    Ok(())
}

pub fn copy_from_user(space: &AddressSpace, va: u64, out: &mut [u8]) -> Result<(), Error> {
    check_range(space, va, out.len() as u64, false)?;
    let mut done = 0;
    while done < out.len() {
        let at = va + done as u64;
        let pa = space.translate(at, false).ok_or(Error::BadPointer)?;
        let chunk = ((PAGE - at % PAGE) as usize).min(out.len() - done);
        unsafe {
            core::ptr::copy_nonoverlapping(pa as *const u8, out[done..].as_mut_ptr(), chunk);
        }
        done += chunk;
    }
    Ok(())
}

pub fn copy_to_user(space: &AddressSpace, va: u64, data: &[u8]) -> Result<(), Error> {
    check_range(space, va, data.len() as u64, true)?;
    let mut done = 0;
    while done < data.len() {
        let at = va + done as u64;
        let pa = space.translate(at, true).ok_or(Error::BadPointer)?;
        let chunk = ((PAGE - at % PAGE) as usize).min(data.len() - done);
        unsafe {
            core::ptr::copy_nonoverlapping(data[done..].as_ptr(), pa as *mut u8, chunk);
        }
        done += chunk;
    }
    Ok(())
}

/// Read a `T` from user memory. The address must be aligned for `T`.
pub fn read_user<T: Copy>(space: &AddressSpace, va: u64) -> Result<T, Error> {
    if va % core::mem::align_of::<T>() as u64 != 0 {
        return Err(Error::BadPointer);
    }
    let mut value = core::mem::MaybeUninit::<T>::uninit();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, core::mem::size_of::<T>())
    };
    copy_from_user(space, va, bytes)?;
    Ok(unsafe { value.assume_init() })
}

/// Write a `T` to user memory. The address must be aligned for `T`.
pub fn write_user<T: Copy>(space: &AddressSpace, va: u64, value: &T) -> Result<(), Error> {
    if va % core::mem::align_of::<T>() as u64 != 0 {
        return Err(Error::BadPointer);
    }
    let bytes = unsafe {
        core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>())
    };
    copy_to_user(space, va, bytes)
}

/// Check that a `T` at `va` could be written, without writing it.
pub fn check_writable<T>(space: &AddressSpace, va: u64) -> Result<(), Error> {
    if va % core::mem::align_of::<T>() as u64 != 0 {
        return Err(Error::BadPointer);
    }
    check_range(space, va, core::mem::size_of::<T>() as u64, true)
}

fn kernel_leaf(l1: u64, va: u64) -> Option<u64> {
    let mut table = l1;
    for (level, shift) in [(1u32, 30u32), (2, 21), (3, 12)] {
        let entry = read_entry(table, ((va >> shift) & 0x1FF) as usize);
        if entry & VALID == 0 {
            return None;
        }
        if level == 3 || entry & TABLE_OR_PAGE == 0 {
            return Some(entry);
        }
        table = entry & ADDR_MASK;
    }
    None
}

fn read_entry(table: u64, index: usize) -> u64 {
    unsafe { core::ptr::read_volatile((table as *const u64).add(index)) }
}

fn write_entry(table: u64, index: usize, value: u64) {
    unsafe { core::ptr::write_volatile((table as *mut u64).add(index), value) }
}
```

  Add `pub mod addrspace;` to `kernel/src/arch/aarch64/mod.rs`.

- [ ] **Step 5: Turn PAN on, set `TCR.A1=0`, and initialise address spaces.** In `paging.rs` `init`, replace the block from `// Disable WXN (writable = execute-never)…` through the `serial_println!` with:

```rust
    // WXN off (writable pages stay executable where the firmware said so);
    // SPAN off, so PAN is set on every exception entry to EL1.
    let mut sctlr: u64;
    unsafe {
        core::arch::asm!("mrs {}, SCTLR_EL1", out(reg) sctlr, options(nomem, nostack));
    }
    sctlr &= !(1 << 19); // WXN=0
    sctlr &= !(1 << 23); // SPAN=0
    unsafe {
        core::arch::asm!("msr SCTLR_EL1, {}", in(reg) sctlr, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
        // PSTATE.PAN = 1 ("msr PAN, #1", encoded because the assembler may not
        // know PAN): the kernel faults if it ever touches EL0 memory directly.
        core::arch::asm!(".inst 0xd500419f", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }

    // TCR_EL1.A1 = 0: the ASID comes from TTBR0, where each task's lives.
    let tcr = tcr & !(1 << 22);
    unsafe {
        core::arch::asm!("msr TCR_EL1, {}", in(reg) tcr, options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }

    let ram_va = core::ptr::addr_of!(TTBR0_ROOT) as u64;
    if let Err(reason) = super::addrspace::init(ttbr0 & ADDR_MASK, t0sz, ram_va) {
        panic!("paging: {reason}");
    }

    serial_println!(
        "  Paging: T0SZ={}, WXN off, PAN on, user window {:#x}",
        t0sz,
        freshos_abi::USER_BASE
    );
```

  PAN is safe to turn on now because, after this task, nothing in the kernel's map is EL0-accessible. `fault` stops using `grant_user_access` in Step 8, and `spawn_user` is deleted in Step 7.

- [ ] **Step 6: Add `elf::load_into`.** Append to `kernel/src/elf.rs`:

```rust
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Load an ELF into `space` at its linked addresses, page by page, with
/// permissions from its flags. Refuses W+X segments, segments outside the
/// user window or overlapping the stack, and overlapping segments.
/// Returns the entry point.
pub fn load_into(
    bytes: &[u8],
    space: &mut crate::arch::addrspace::AddressSpace,
) -> Result<u64, &'static str> {
    use crate::arch::addrspace::{Perm, SpaceError, PAGE, in_window};
    use freshos_abi::{USER_BASE, USER_SIZE, USER_STACK_SIZE};

    let layout = parse_layout(bytes)?;
    // Everything the image uses must end below the stack's guard page.
    let image_limit = USER_BASE + USER_SIZE - USER_STACK_SIZE - PAGE;

    for idx in 0..layout.phnum {
        let ph = layout.phoff + idx * layout.phentsize;
        if read_u32(bytes, ph).ok_or("truncated program header")? != PT_LOAD {
            continue;
        }
        let flags = read_u32(bytes, ph + 4).ok_or("missing p_flags")?;
        let offset = read_u64(bytes, ph + 8).ok_or("missing p_offset")? as usize;
        let vaddr = read_u64(bytes, ph + 16).ok_or("missing p_vaddr")?;
        let filesz = read_u64(bytes, ph + 32).ok_or("missing p_filesz")?;
        let memsz = read_u64(bytes, ph + 40).ok_or("missing p_memsz")?;

        if flags & PF_W != 0 && flags & PF_X != 0 {
            return Err("segment is writable and executable");
        }
        let end = vaddr.checked_add(memsz).ok_or("segment address overflow")?;
        if !in_window(vaddr, memsz) {
            return Err("segment outside the user window");
        }
        if end > image_limit {
            return Err("segment overlaps the stack");
        }
        let perm = if flags & PF_X != 0 {
            Perm::ReadExec
        } else if flags & PF_W != 0 {
            Perm::ReadWrite
        } else {
            Perm::ReadOnly
        };

        let file_end = vaddr + filesz;
        let mut page = align_down(vaddr, PAGE);
        while page < end {
            let pa = space.map_new_page(page, perm).map_err(|e| match e {
                SpaceError::AlreadyMapped => "segments overlap a page",
                SpaceError::OutsideWindow => "segment outside the user window",
                SpaceError::OutOfMemory => "out of memory",
            })?;
            // Copy the part of the file image that falls in this page; the
            // rest of the page (including .bss) stays zero.
            let copy_start = page.max(vaddr);
            let copy_end = (page + PAGE).min(file_end);
            if copy_start < copy_end {
                let src = offset + (copy_start - vaddr) as usize;
                let n = (copy_end - copy_start) as usize;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bytes[src..src + n].as_ptr(),
                        (pa + (copy_start - page)) as *mut u8,
                        n,
                    );
                }
            }
            page += PAGE;
        }
    }

    if !in_window(layout.entry, 4) {
        return Err("entry point outside the user window");
    }
    Ok(layout.entry)
}
```

  `parse_layout` already rejects segments that fall outside the file, and filesz > memsz, so `bytes[src..src + n]` is in bounds.

- [ ] **Step 7: Tasks with address spaces, in `context.rs`.**
  1. Add `use freshos_abi::{USER_BASE, USER_SIZE, USER_STACK_SIZE};` and `use super::addrspace::{AddressSpace, Perm, PAGE};`.
  2. Replace `struct Task` and `EMPTY_TASK`. `Task` is no longer `Copy`, because it owns its address space:

```rust
struct Task {
    sp: u64, // saved frame (kernel stack, after save_all_regs)
    kernel_stack_bottom: u64,
    kernel_stack_pages: usize,
    ttbr0: u64, // TTBR0_EL1 to load when this task runs
    space: Option<AddressSpace>,
    state: State,
}

const EMPTY_TASK: Task = Task {
    sp: 0,
    kernel_stack_bottom: 0,
    kernel_stack_pages: 0,
    ttbr0: 0,
    space: None,
    state: State::Free,
};
```

  3. `PendingFree` and `queue_pending_free` now track only the kernel stack. Delete the `user_stack_*` fields, and change `queue_pending_free(task: Task)` to `queue_pending_free(bottom: u64, pages: usize)`. Keep `reap_pending_frees` as it is, minus the user-stack branch.
  4. Add a kernel TTBR0 and the switch helper:

```rust
/// TTBR0 for tasks without their own space: the firmware's table, ASID 0.
static KERNEL_TTBR0: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Point TTBR0 at `ttbr0` if it isn't already. Kernel mappings are global
/// and identical in every table, so the kernel keeps running across the
/// switch; ASIDs mean no TLB flush is needed.
fn activate(ttbr0: u64) {
    let current: u64;
    unsafe { core::arch::asm!("mrs {}, TTBR0_EL1", out(reg) current, options(nomem, nostack)) };
    if current != ttbr0 {
        unsafe {
            core::arch::asm!("msr TTBR0_EL1, {}", "isb", in(reg) ttbr0, options(nostack));
        }
    }
}
```

  In `init(ttbr0)`, store it: `KERNEL_TTBR0.store(ttbr0, Ordering::SeqCst);`. Build task 0 as `Task { ttbr0, state: State::Running, ..EMPTY_TASK }`, and reset the array with `for task in t.iter_mut() { *task = EMPTY_TASK; }`.

  5. In `scheduler_tick_arm`, after `CURRENT.store(next, …)`, replace the comment "All tasks share UEFI's (patched) page tables…" with `activate(t[next].ttbr0);`.
  6. `spawn_with_arg`: set `ttbr0: KERNEL_TTBR0.load(Ordering::SeqCst)`, and build the task with `..EMPTY_TASK` for the removed fields.
  7. Replace `allocate_slot` (which panics) with:

```rust
fn free_slot(t: &[Task; MAX_TASKS]) -> Option<usize> {
    (1..MAX_TASKS).find(|&id| t[id].state == State::Free)
}
```

  In `spawn_with_arg`, use `let id = free_slot(t).expect("too many tasks");`. Built-ins are spawned at boot, so running out there is a boot bug.

  8. Delete `spawn_user` and `spawn_user_pregranted` entirely.
  9. Add the EL0 spawn:

```rust
#[derive(Debug)]
pub enum SpawnError {
    NoSlot,
    OutOfMemory,
    BadImage(&'static str),
}

/// Start an EL0 task from an ELF image, in its own address space. `arg0` and
/// `arg1` arrive in x0 and x1 at the entry point (freshos-rt passes them to
/// `main` as the handle count and the service's argument).
pub fn spawn_el0(image: &[u8], arg0: u64, arg1: u64) -> Result<usize, SpawnError> {
    reap_pending_frees(0);
    let t = unsafe { &mut *tasks() };
    let id = free_slot(t).ok_or(SpawnError::NoSlot)?;

    // The ASID is the task slot: unique among live tasks, and flushed when
    // the space is dropped, before the slot can be reused.
    let mut space = AddressSpace::new(id as u16).map_err(|_| SpawnError::OutOfMemory)?;
    let entry = crate::elf::load_into(image, &mut space).map_err(SpawnError::BadImage)?;

    // Stack at the top of the window. The page below it is never mapped:
    // that's the guard page.
    let stack_top = USER_BASE + USER_SIZE;
    let mut va = stack_top - USER_STACK_SIZE;
    while va < stack_top {
        space.map_new_page(va, Perm::ReadWrite).map_err(|_| SpawnError::OutOfMemory)?;
        va += PAGE;
    }
    space.publish();

    let kernel_stack_bottom = frame_alloc::allocate_contiguous(KERNEL_STACK_SIZE / 4096)
        .ok_or(SpawnError::OutOfMemory)?;
    let kernel_stack_top = kernel_stack_bottom + KERNEL_STACK_SIZE as u64;

    // Seed a frame for restore_all_regs + eret into EL0.
    let frame_base = kernel_stack_top - 272;
    unsafe {
        core::ptr::write_bytes(frame_base as *mut u8, 0, 272);
        let slots = frame_base as *mut u64;
        *slots.add(0) = arg0; // x0
        *slots.add(1) = arg1; // x1
        *slots.add(31) = stack_top; // SP_EL0
        *slots.add(32) = entry; // ELR_EL1
        *slots.add(33) = 0; // SPSR_EL1: EL0t, interrupts unmasked
    }

    let ttbr0 = space.ttbr0();
    t[id] = Task {
        sp: frame_base,
        kernel_stack_bottom,
        kernel_stack_pages: KERNEL_STACK_SIZE / 4096,
        ttbr0,
        space: Some(space),
        state: State::Ready,
    };
    COUNT.store(task_count(), Ordering::SeqCst);
    serial_println!("    task {} el0 asid={} entry={:#x}", id, id, entry);
    Ok(id)
}
```

  10. Split termination into a non-diverging retire and the old diverging wrapper:

```rust
/// Remove the current task: record its exit, free its address space, and
/// queue its kernel stack. The caller is still running on that stack and
/// must switch away (a syscall returns through the scheduler; a fault waits
/// for the next tick).
pub fn retire_current(reason: u64) {
    let cur = CURRENT.load(Ordering::SeqCst);
    if cur == 0 {
        return;
    }
    let t = unsafe { &mut *tasks() };
    let mut task = core::mem::replace(&mut t[cur], EMPTY_TASK);
    // Never keep running on a table that is about to be freed.
    activate(KERNEL_TTBR0.load(Ordering::SeqCst));
    drop(task.space.take());
    queue_pending_free(task.kernel_stack_bottom, task.kernel_stack_pages);
    crate::init_abi::task_exited(cur, reason);
    COUNT.store(task_count(), Ordering::SeqCst);
}

pub fn terminate_current_with_reason(reason: u64) -> ! {
    retire_current(reason);
    loop {
        super::interrupt_enable();
        super::halt();
    }
}
```

- [ ] **Step 8: Start `fault` in its own space.**
  1. In `init_abi.rs`, replace `spawn_fault_service`:

```rust
fn spawn_fault_service() -> usize {
    let Some(image) = crate::boot_images::find("FAULT.ELF") else {
        serial_println!("[init] FAULT.ELF missing");
        return 0;
    };
    match arch::context::spawn_el0(image, 0, 0) {
        Ok(id) => id,
        Err(err) => {
            serial_println!("[init] cannot start fault: {:?}", err);
            0
        }
    }
}
```

  `init_spawn_service` must treat a returned 0 as failure. Before `remember_service_task`, add `if task_id == 0 { return -1; }`, adapting to its local variable names.

  2. In `service_abi.rs`, delete `EXTERNAL_FAULT_ENTRY`, `EXTERNAL_FAULT_USER_STACK`, `register_external_fault`, `external_fault_entry` and `external_fault_user_stack`.
  3. In `main.rs`, delete the whole `if let Some(bytes) = boot_images::find("FAULT.ELF") { … }` block, and the `EL0_FAULT_REGION_*` constants.
  4. Delete `arm_tasks::supervised_fault_el1`. It's no longer referenced; its only caller was the fallback in `spawn_fault_service`.

- [ ] **Step 9: Build and run all tests.** Build both boards: no errors, and no warnings beyond 33. Some lines may disappear, for example `spawn_user` being unused. Run `./test.sh`. Expected: `OK`, including both `IsolationTest` tests. If a boot hangs right after `Paging:`, check that `TCR_EL1` was read before the store (`tcr` is read earlier in `init`).

- [ ] **Step 10: Commit.**

```bash
git add .cargo/config.toml kernel tests
git commit -m "Give EL0 tasks their own address spaces, starting with fault" \
  -m "Each EL0 task now gets a private top-level page table that shares the kernel's EL1-only mappings and owns a 1 GiB window at 16 GiB, where its ELF is loaded page by page at its link address with W^X, under a 64 KiB stack with an unmapped guard page. TTBR0 switches with the task, and ASIDs (the task slot) avoid TLB flushes. PAN is back on and TCR.A1 is cleared. fault is the first service to run this way; its special 2 MiB region and global permission patching are gone, as are spawn_user and spawn_user_pregranted. Userbins are now linked at 0x4_0000_0000."
```

---

### Task 5: The runtime crate and the validated syscall layer

This moves `fault` and `pulse` onto `freshos-rt`, and adds `probe-bad`.

**Files:**
- Create: `lib/rt/Cargo.toml`, `lib/rt/src/lib.rs`, `lib/rt/src/syscall.rs`, `kernel/src/syscalls.rs`, `userbins/probe-bad/{Cargo.toml,src/main.rs}`
- Modify:
  - `Cargo.toml` (members);
  - `kernel/src/arch/aarch64/exception.s` (`svc_dispatch`);
  - `kernel/src/arch/aarch64/syscall.rs` (replace `syscall_dispatch_arm` and the old handlers);
  - `kernel/src/arch/aarch64/context.rs` (`switch_away`, `current_space`);
  - `kernel/src/init_abi.rs` (`pulse` at EL0; optional `probe-bad-*` entries);
  - `kernel/src/service_abi.rs` (remove `pulse`);
  - `userbins/fault`, `userbins/pulse` (rewrite on rt);
  - `kernel/src/arm_tasks.rs` (delete `supervised_pulse_el1`);
  - `kernel/src/main.rs` (remove the `PULSE.ELF` load and `mod syscalls;`);
  - `tests/harness.py` (`TEST_ELFS`);
  - `tests/test_isolation.py`.

**Interfaces:**
- Consumes: `addrspace::{copy_from_user, read_user, …}` (Task 4); `freshos_abi` (Task 2).
- Produces:
  - `freshos_rt::{entry!, log!, Startup, send, recv, recv_until, try_recv, yield_now, exit, time_ns, log, channel_create, spawn, LineBuf}`. The IPC wrappers are callable now but return `NoSuchSyscall` until Task 6 or 7.
  - `Startup::handle(i) -> Handle`, `handle_count()` and `arg()`.
  - `syscalls::{dispatch, Outcome}`.
  - `context::switch_away(frame: u64) -> u64` and `context::current_space() -> Option<&'static AddressSpace>`.
  - The kernel log format `[<task name>] <text>`.

- [ ] **Step 1: Write the failing tests.** Add to `tests/harness.py`: `TEST_ELFS: tuple[str, ...] = ("probe-bad",)`. Add to `tests/test_isolation.py`:

```python
    def test_forbidden_memory_access_is_a_contained_fault(self) -> None:
        probes = ("probe-bad-kernel", "probe-bad-unmapped", "probe-bad-code", "probe-bad-stack")

        def all_faulted() -> bool:
            services = self.services()
            return all(services.get(p, {}).get("last_exit") == "fault" for p in probes)

        self.wait_until(all_faulted, timeout=20, message=f"{probes} to fault")
        self.assertEqual(self.boot.find_logs(r"SURVIVED"), [])
        self.assertTrue(self.services()["pong"]["running"])

    def test_logs_are_prefixed_with_the_kernel_registered_name(self) -> None:
        self.boot.wait_for_log(r"^\[pulse\] start$", timeout=20)
```

- [ ] **Step 2: Run them and watch them fail.** Run `./test.sh -k Isolation`. Expected: both new tests fail. There's no `probe-bad` package yet, so the build itself fails.

- [ ] **Step 3: Create `lib/rt`.** First `lib/rt/Cargo.toml`:

```toml
[package]
name = "freshos-rt"
version = "0.1.0"
edition = "2024"

[dependencies]
freshos-abi = { path = "../abi" }
```

`lib/rt/src/syscall.rs`:

```rust
//! The only `svc` in any userbin. The kernel preserves every register except
//! x0 across a syscall (exception.s saves and restores the full frame).
use freshos_abi::Error;

#[inline(always)]
pub(crate) fn svc(nr: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") a0 as i64 => ret,
            in("x1") a1,
            in("x2") a2,
            options(nostack),
        );
    }
    ret
}

pub(crate) fn check(ret: i64) -> Result<u64, Error> {
    if ret < 0 {
        Err(Error::from_code(ret))
    } else {
        Ok(ret as u64)
    }
}
```

`lib/rt/src/lib.rs`:

```rust
//! What a FreshOS userbin links against: the entry point, a panic handler,
//! safe syscall wrappers and `log!`. Userbins have no allocator.
#![no_std]

mod syscall;

pub use freshos_abi::{self as abi, Error, ExitReason, Grant, Handle, Message, Rights, tag};
use freshos_abi::{SpawnRequest, sys};
use syscall::{check, svc};

/// What a service is started with: its granted handles (slots 0..count, in
/// the order init's table lists them) and its table argument.
#[derive(Clone, Copy)]
pub struct Startup {
    handles: u32,
    arg: u64,
}

impl Startup {
    #[doc(hidden)]
    pub fn __new(handles: u64, arg: u64) -> Self {
        Startup { handles: handles as u32, arg }
    }

    /// The `index`-th granted handle. Panics if it wasn't granted.
    pub fn handle(&self, index: u32) -> Handle {
        assert!(index < self.handles, "handle {index} was not granted");
        Handle(index)
    }

    pub fn handle_count(&self) -> u32 {
        self.handles
    }

    pub fn arg(&self) -> u64 {
        self.arg
    }
}

/// Declare the userbin's `main(Startup) -> !`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn _start(handle_count: u64, arg: u64) -> ! {
            let main: fn($crate::Startup) -> ! = $main;
            main($crate::Startup::__new(handle_count, arg))
        }
    };
}

pub fn send(handle: Handle, message: &Message) -> Result<(), Error> {
    check(svc(sys::SEND, handle.0 as u64, message as *const Message as u64, 0)).map(|_| ())
}

/// Wait for a message.
pub fn recv(handle: Handle) -> Result<Message, Error> {
    recv_until(handle, 0)
}

/// Wait for a message until `deadline_ns` (from `time_ns`); `Timeout` if it passes.
/// A deadline of 0 waits forever.
pub fn recv_until(handle: Handle, deadline_ns: u64) -> Result<Message, Error> {
    let mut message = Message::empty();
    check(svc(sys::RECV, handle.0 as u64, &mut message as *mut Message as u64, deadline_ns))?;
    Ok(message)
}

/// Take a waiting message, or `WouldBlock`.
pub fn try_recv(handle: Handle) -> Result<Message, Error> {
    let mut message = Message::empty();
    check(svc(sys::TRY_RECV, handle.0 as u64, &mut message as *mut Message as u64, 0))?;
    Ok(message)
}

pub fn yield_now() {
    svc(sys::YIELD, 0, 0, 0);
}

pub fn exit() -> ! {
    exit_with(ExitReason::Clean)
}

fn exit_with(reason: ExitReason) -> ! {
    svc(sys::EXIT, reason as u64, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}

pub fn time_ns() -> u64 {
    svc(sys::TIME_NS, 0, 0, 0) as u64
}

/// Write one log line. The kernel prefixes it with this task's name.
pub fn log(text: &str) {
    svc(sys::LOG, text.as_ptr() as u64, text.len() as u64, 0);
}

/// init only: create a channel; the handle carries SEND and RECV.
pub fn channel_create() -> Result<Handle, Error> {
    check(svc(sys::CHANNEL_CREATE, 0, 0, 0)).map(|h| Handle(h as u32))
}

/// init only: start `binary` (an ESP file name, or "builtin:<name>") as the
/// service `name`, with `grants` as its handles 0.., and `arg` for its `main`.
pub fn spawn(name: &str, binary: &str, grants: &[Grant], arg: u64) -> Result<u32, Error> {
    let request = SpawnRequest {
        name_ptr: name.as_ptr() as u64,
        name_len: name.len() as u64,
        binary_ptr: binary.as_ptr() as u64,
        binary_len: binary.len() as u64,
        grants_ptr: grants.as_ptr() as u64,
        grants_len: grants.len() as u64,
        arg,
    };
    check(svc(sys::SPAWN, &request as *const SpawnRequest as u64, 0, 0)).map(|id| id as u32)
}

/// A fixed buffer for `log!`; longer text is cut at `MAX_LOG` bytes.
pub struct LineBuf {
    bytes: [u8; freshos_abi::MAX_LOG],
    len: usize,
}

impl LineBuf {
    pub const fn new() -> Self {
        LineBuf { bytes: [0; freshos_abi::MAX_LOG], len: 0 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("?")
    }
}

impl Default for LineBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for ch in s.chars() {
            let mut utf8 = [0u8; 4];
            let encoded = ch.encode_utf8(&mut utf8).as_bytes();
            if self.len + encoded.len() > self.bytes.len() {
                break;
            }
            self.bytes[self.len..self.len + encoded.len()].copy_from_slice(encoded);
            self.len += encoded.len();
        }
        Ok(())
    }
}

/// `log!("x = {}", x)`: format into a stack buffer and log it.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        let mut line = $crate::LineBuf::new();
        let _ = core::fmt::Write::write_fmt(&mut line, format_args!($($arg)*));
        $crate::log(line.as_str());
    }};
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log!("panic: {}", info.message());
    exit_with(ExitReason::Panic)
}
```

  Add `"lib/rt"` to the root `Cargo.toml` members.

- [ ] **Step 4: A frame-based syscall entry.** In `exception.s`, replace everything from `svc_dispatch:` through its `eret` with:

```asm
svc_dispatch:
    // Hand Rust the whole saved frame: it reads x8 and x0-x5 from it, writes
    // the result into the saved x0, and returns the frame to resume, which
    // may belong to another task (blocking, yielding, exiting, hand-off).
    mov     x0, sp
    bl      syscall_entry_arm
    mov     sp, x0

    restore_all_regs
    eret
```

  Update the ABI comment above `lower_sync_entry` to say "x0 = return value (written into the saved frame)".

- [ ] **Step 5: Replace the dispatcher in `arch/aarch64/syscall.rs`.** Delete the `SYS_*` constants, `syscall_dispatch_arm`, `sys_send` and `sys_recv`. Keep `FbInfo`, `FB_INFO_PTR`, `set_fb_info`, `SurfaceInfo`, `SURFACES_PTR`, `SURFACE_COUNT` and `add_surface`, which the in-kernel built-ins still read. Also delete `fb_address` and `fb_size` if they're still unused. Update the header comment. Add:

```rust
/// Syscall entry from exception.s. `frame` is the task's saved register frame
/// (save_all_regs layout: x0..x30 at 8*n, SP_EL0 at 248, ELR at 256, SPSR at 264).
/// Returns the frame to restore.
#[unsafe(no_mangle)]
extern "C" fn syscall_entry_arm(frame: u64) -> u64 {
    use crate::syscalls::Outcome;
    let regs = frame as *mut u64;
    let (nr, args) = unsafe {
        (*regs.add(8), [*regs.add(0), *regs.add(1), *regs.add(2), *regs.add(3), *regs.add(4), *regs.add(5)])
    };
    match crate::syscalls::dispatch(nr, args) {
        Outcome::Return(value) => {
            unsafe { *regs = value as u64 };
            frame
        }
        Outcome::Yield => {
            unsafe { *regs = 0 };
            super::context::switch_away(frame)
        }
        Outcome::Exited | Outcome::Blocked => super::context::switch_away(frame),
        Outcome::HandOff(task) => {
            unsafe { *regs = 0 };
            super::context::hand_off(frame, task)
        }
    }
}
```

`Outcome::Blocked` and `HandOff` are produced from Task 6 onwards. Add `hand_off` now, as shown in Step 6.

- [ ] **Step 6: Scheduler entry points, in `context.rs`.** Extract the scheduling half of `scheduler_tick_arm` into `switch_away`, and add `hand_off` and `current_space`:

```rust
#[unsafe(no_mangle)]
extern "C" fn scheduler_tick_arm(stack_ptr: u64) -> u64 {
    reap_pending_frees(stack_ptr);
    let intid = gic::acknowledge();
    timer::handle_irq();
    gic::end_of_interrupt(intid);
    switch_away(stack_ptr)
}

/// Save the current task's frame and pick the next ready task, round-robin.
/// Returns the frame to restore. Runs with IRQs masked (exception context).
pub fn switch_away(frame: u64) -> u64 {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    if t[cur].state != State::Free {
        t[cur].sp = frame;
    }
    if t[cur].state == State::Running {
        t[cur].state = State::Ready;
    }
    let next = (1..=MAX_TASKS)
        .map(|step| (cur + step) % MAX_TASKS)
        .find(|&candidate| t[candidate].state == State::Ready)
        .unwrap_or(0);
    run(t, next)
}

/// Run `target` next (direct hand-off). The current task stays ready.
pub fn hand_off(frame: u64, target: usize) -> u64 {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    if t[cur].state != State::Free {
        t[cur].sp = frame;
    }
    if t[cur].state == State::Running {
        t[cur].state = State::Ready;
    }
    run(t, target)
}

fn run(t: &mut [Task; MAX_TASKS], next: usize) -> u64 {
    t[next].state = State::Running;
    CURRENT.store(next, Ordering::SeqCst);
    activate(t[next].ttbr0);
    t[next].sp
}

/// The current task's address space (None for kernel tasks).
pub fn current_space() -> Option<&'static AddressSpace> {
    let t = unsafe { &*tasks() };
    t[CURRENT.load(Ordering::SeqCst)].space.as_ref()
}
```

  `.unwrap_or(0)` keeps the old behaviour: with nothing ready, task 0 (idle) runs. Task 0 is always `Ready` or `Running`, and is included, because `(cur + step) % MAX_TASKS` reaches 0.

- [ ] **Step 7: Create `kernel/src/syscalls.rs`.** Add `#[cfg(target_arch = "aarch64")] mod syscalls;` to `main.rs`.

```rust
/// System calls from EL0 (EL0 isolation spec, section 4).
///
/// Every pointer is checked against the caller's own address space before
/// use, and errors are returned, not fatal. What terminates a task is its own
/// hardware fault, handled in exceptions.rs.
use core::fmt::Write;

use freshos_abi::{Error, ExitReason, MAX_LOG, sys};

use crate::arch::addrspace::{AddressSpace, copy_from_user};
use crate::arch::context;

pub enum Outcome {
    /// Resume the caller with this value in x0.
    Return(i64),
    /// The caller gave up the CPU; it resumes with 0.
    Yield,
    /// The caller exited; never resume it.
    Exited,
    /// The caller is waiting; its x0 is written when it's woken.
    Blocked,
    /// A send woke this task: run it next. The caller resumes with 0.
    HandOff(usize),
}

fn err(e: Error) -> Outcome {
    Outcome::Return(e as i64)
}

fn space() -> Result<&'static AddressSpace, Error> {
    context::current_space().ok_or(Error::NotPermitted)
}

pub fn dispatch(nr: u64, a: [u64; 6]) -> Outcome {
    match nr {
        sys::YIELD => Outcome::Yield,
        sys::EXIT => exit(a[0]),
        sys::TIME_NS => Outcome::Return(crate::arch::time_ns() as i64),
        sys::LOG => log(a[0], a[1]),
        _ => err(Error::NoSuchSyscall),
    }
}

fn exit(reason: u64) -> Outcome {
    // A task may report Clean or Panic. Fault is the kernel's to give.
    let reason = match ExitReason::from_u64(reason) {
        Some(ExitReason::Panic) => ExitReason::Panic,
        _ => ExitReason::Clean,
    };
    context::retire_current(reason as u64);
    Outcome::Exited
}

/// Print one line as "[name] text". The text is capped at MAX_LOG bytes, and
/// invalid UTF-8 and control characters (newlines included) become '?', so a
/// log can never forge another task's line.
fn log(ptr: u64, len: u64) -> Outcome {
    let space = match space() {
        Ok(space) => space,
        Err(e) => return err(e),
    };
    let n = len.min(MAX_LOG as u64) as usize;
    let mut buf = [0u8; MAX_LOG];
    if let Err(e) = copy_from_user(space, ptr, &mut buf[..n]) {
        return err(e);
    }
    let mut out = crate::serial::Serial;
    let _ = write!(out, "[{}] ", crate::task_names::name(context::current_task()));
    for chunk in buf[..n].utf8_chunks() {
        for ch in chunk.valid().chars() {
            let _ = out.write_char(if ch.is_control() { '?' } else { ch });
        }
        if !chunk.invalid().is_empty() {
            let _ = out.write_char('?');
        }
    }
    if len > MAX_LOG as u64 {
        let _ = out.write_str("…");
    }
    let _ = out.write_str("\n");
    Outcome::Return(0)
}
```

  `write_char` for `Serial` comes from `core::fmt::Write`'s default method.

- [ ] **Step 8: Rewrite `fault` and `pulse` on rt.** For both, `userbins/<name>/Cargo.toml` `[dependencies]` becomes `freshos-rt = { path = "../../lib/rt" }`.

`userbins/fault/src/main.rs`:

```rust
#![no_std]
#![no_main]

use freshos_rt::{Startup, entry, log, yield_now};

entry!(main);

/// Deliberately crashes after two beats, to prove that faults are contained
/// and supervised services are restarted.
fn main(_: Startup) -> ! {
    log!("start");
    for beat in 1..=2 {
        for _ in 0..300 {
            yield_now();
        }
        log!("beat {beat}");
    }
    log!("crash test");
    unsafe { core::arch::asm!("brk #0", options(noreturn)) }
}
```

`userbins/pulse/src/main.rs`:

```rust
#![no_std]
#![no_main]

use freshos_rt::{Startup, entry, exit, log, yield_now};

entry!(main);

/// Beats three times and exits cleanly, to exercise supervised restarts.
fn main(_: Startup) -> ! {
    log!("start");
    for beat in 1..=3 {
        for _ in 0..500 {
            yield_now();
        }
        log!("beat {beat}");
    }
    log!("exit");
    exit()
}
```

- [ ] **Step 9: Create `probe-bad`,** with `Cargo.toml` like `fault`'s and name `freshos-probe-bad`. Add `"userbins/probe-bad"` to the workspace members. `userbins/probe-bad/src/main.rs`:

```rust
#![no_std]
#![no_main]

use freshos_rt::{Startup, entry, exit, log};

entry!(main);

/// Test-only. Attempts one forbidden memory access, chosen by its table
/// argument, and should never survive it:
///   0 = write to its own code, 1 = overflow its stack, else = read that address.
fn main(start: Startup) -> ! {
    match start.arg() {
        0 => {
            log!("writing to own code");
            let code = main as *const () as *mut u32;
            unsafe { code.write_volatile(0) };
        }
        1 => {
            log!("overflowing the stack");
            let depth = recurse(0);
            log!("depth {depth}");
        }
        address => {
            log!("reading {address:#x}");
            let value = unsafe { (address as *const u64).read_volatile() };
            log!("read {value:#x}");
        }
    }
    log!("SURVIVED");
    exit()
}

#[inline(never)]
fn recurse(depth: u64) -> u64 {
    let frame = [depth; 64];
    core::hint::black_box(&frame);
    recurse(depth + 1) + frame[0]
}
```

- [ ] **Step 10: Start `pulse` and the probes from `init_abi`.** This is transitional; Task 7 replaces all of it.
  1. Change `ServiceDefinition.spawn` to `fn() -> Option<usize>`. Every existing spawn function returns `Some(id)`. `init_spawn_service` returns `-1` for `None`. Remove the Task 4 `0`-means-failure special case, and make `spawn_fault_service` return `None` on failure.
  2. Add a helper and the spawn functions:

```rust
fn spawn_image(binary: &str, arg: u64) -> Option<usize> {
    let image = crate::boot_images::find(binary)?; // optional: silently absent
    match arch::context::spawn_el0(image, 0, arg) {
        Ok(id) => Some(id),
        Err(err) => {
            serial_println!("[init] cannot start {}: {:?}", binary, err);
            None
        }
    }
}

fn spawn_pulse_service() -> Option<usize> { spawn_image("PULSE.ELF", 0) }
fn spawn_probe_bad_kernel() -> Option<usize> { spawn_image("PROBEBAD.ELF", 0x4000_0000) }
fn spawn_probe_bad_unmapped() -> Option<usize> { spawn_image("PROBEBAD.ELF", 0x4_2000_0000) }
fn spawn_probe_bad_code() -> Option<usize> { spawn_image("PROBEBAD.ELF", 0) }
fn spawn_probe_bad_stack() -> Option<usize> { spawn_image("PROBEBAD.ELF", 1) }
```

  `0x4000_0000` is the start of RAM on QEMU `virt` (the kernel heap). The kernel's identity map of RAM, which also covers every other task's frames, is EL1-only, so this one probe stands for both "kernel memory" and "another service's frames".

  3. Add `SERVICE_PROBE_BAD_KERNEL = 10` through `SERVICE_PROBE_BAD_STACK = 13`, bump `SERVICE_COUNT` to 13, and add four `ServiceDefinition`s with names `"probe-bad-kernel"`, `"probe-bad-unmapped"`, `"probe-bad-code"` and `"probe-bad-stack"`, flags `SERVICE_FLAG_AUTOSTART`, and restart period 0.
  4. In `service_abi.rs`, delete the pulse registration (`EXTERNAL_PULSE_ENTRY`, `register_external_pulse`, `external_pulse_entry`). In `main.rs`, delete the `PULSE.ELF` load block. Delete `arm_tasks::supervised_pulse_el1`.

- [ ] **Step 11: Build everything and run the tests.** Don't add `probe-bad` to `USERBINS` in `run-arm.sh`. It's test-only, and comes in through `EXTRA_ELFS`. Then:

```bash
EXTRA_ELFS="probe-bad" BUILD_ONLY=1 ./run-arm.sh
./test.sh
```

  Expected: `OK`. The isolation tests show all four probes with `last_exit == "fault"`. The pulse logs read `[pulse] start` and `[pulse] beat 1`, prefixed by the kernel. If `test_every_long_running_service_is_running` now sees extra probe services, that's expected: probes aren't in `LONG_RUNNING`.

- [ ] **Step 12: Commit.**

```bash
git add lib/rt kernel userbins Cargo.toml Cargo.lock tests
git commit -m "Route EL0 syscalls through one validated path, and move pulse to EL0" \
  -m "Syscalls now enter with the whole saved frame and may resume a different task, which lets yield and exit switch immediately instead of idling until the next tick. Every user pointer is translated through the caller's own page table and copied through the kernel's mapping of the frame, so the kernel never touches user addresses and PAN stays on. Logs are capped, sanitised and prefixed by the kernel with the task's name, so they can't impersonate anyone. freshos-rt gives userbins an entry point, a panic handler that reports 'panic', and safe wrappers; fault and pulse use it, pulse now at EL0. probe-bad (test-only) shows that reading kernel memory, reading an unmapped address, writing to code and overflowing the stack each end in a contained fault."
```

---

### Task 6: Handles, IPC over syscalls, direct hand-off, and `ping`/`pong` at EL0

**Files:**
- Create: `kernel/src/handles.rs`, `userbins/ping/`, `userbins/probe-chan/`, `tests/test_channels.py`
- Modify:
  - `kernel/src/ipc.rs` (queued timestamps, receivers, user-wait delivery, delivery metric);
  - `kernel/src/metrics.rs` (`ipc_rtt_us` → `ipc_delivery_ns`);
  - `kernel/src/arm_tasks.rs` (metric labels; delete `ipc_probe_*`);
  - `kernel/src/mcp.rs` (the metric);
  - `kernel/src/arch/aarch64/context.rs` (handles, waits, deadlines, `complete_user_recv`);
  - `kernel/src/syscalls.rs` (SEND/RECV/TRY_RECV);
  - `kernel/src/init_abi.rs` (EL0 ping/pong/probe-chan with grants);
  - `kernel/src/main.rs` (channels 4 and 5; remove the PONG load and `service_abi`);
  - delete `kernel/src/service_abi.rs`;
  - `userbins/pong` (rewrite);
  - `run-arm.sh` (`USERBINS += ping`);
  - `tests/harness.py`, `tests/test_latency.py`, `tests/test_trace.py`.

**Interfaces:**
- Consumes: `Outcome::{Blocked, HandOff}` and `hand_off` (Task 5); `read_user`, `write_user` and `check_writable` (Task 4).
- Produces:
  - `handles::{HandleTable, Slot}`, with `HandleTable::EMPTY`, `lookup(Handle, Rights) -> Result<u32, Error>`, `insert(Slot) -> Result<Handle, Error>`, `get_mut(Handle) -> Option<&mut Slot>` and `slots() -> impl Iterator<Item = (u32, Slot)>`.
  - `context::spawn_el0(image, handles: HandleTable, arg0, arg1)` (the signature gains `handles`), plus `context::current_handles() -> Option<&'static HandleTable>`, `set_user_wait(channel, buf, deadline)`, `has_user_wait(task) -> bool` and `complete_user_recv(task, &Message)`.
  - `ipc::send(channel, &msg) -> Result<Option<usize>, Error>`, where `Some(task)` means a user waiter was completed and should run next.
  - `ipc::try_dequeue(channel) -> Result<Option<Message>, Error>`, `ipc::register_waiter(channel, task)`, `ipc::cancel_waiter(channel, task)` and `ipc::set_receiver(channel, task)`.
  - `metrics::record_ipc_delivery_ns(ns)`; `Snapshot.ipc_delivery_ns`.
  - The MCP metric `"ipc_delivery": {"latest_ns", "max_ns"}`.
  - Log lines `[ping] rtt_median_ns=N samples=100` and `[probe-chan] [test] <case> result=<refused|ok> code=<n>`.

**Concepts for the implementer.**
- **Direct hand-off.** A user task blocked in `recv` sits in the kernel with its frame saved. It isn't sleeping in `wfi`: it has been switched away from.
- When a message arrives for it, `ipc::send` delivers the message **straight into that task's buffer**, through its own address space, and writes `0` into its saved `x0`. The task is then complete and `Ready`.
- If the sender was itself a syscall, it returns `HandOff(receiver)`, and the receiver runs next. That's the request/response path that gets the round trip down to microseconds.
- Kernel built-ins that block in `ipc::recv` keep the old sleep-until-tick behaviour. They're leaving the kernel, and `send` still wakes them.

- [ ] **Step 1: Write the failing tests.**
  1. `tests/harness.py`: `TEST_ELFS = ("probe-bad", "probe-chan")`.
  2. `tests/test_latency.py`: replace the class with:

```python
from harness import FreshOSTestCase

TARGET_NS = 100_000  # spec: median ping/pong round trip under 100 us on QEMU + HVF


class LatencyTest(FreshOSTestCase):
    def test_median_round_trip_is_under_target(self) -> None:
        match = self.boot.wait_for_log(r"\[ping\] rtt_median_ns=(\d+) samples=100", timeout=30)
        median = int(match.group(1))
        print(f"\nping/pong median round trip: {median} ns (target {TARGET_NS})")
        self.assertLess(median, TARGET_NS)

    def test_kernel_measures_every_delivery(self) -> None:
        def latest() -> int:
            return self.boot.mcp().view("metrics")["ipc_delivery"]["latest_ns"]

        self.wait_until(lambda: latest() > 0, timeout=20, message="a measured delivery")
```

  3. `tests/test_trace.py`: tighten the `to` check. Every PING must now name `pong`: delete the `if message["to"] is not None:` guard.
  4. `tests/test_channels.py`:

```python
import unittest

from harness import FreshOSTestCase

# probe-chan logs "[test] <case> result=<refused|ok> code=<n>". Expected codes
# come from freshos_abi::Error.
EXPECTED = {
    "ungranted-send": -2,          # NoSuchHandle
    "recv-on-send-only": -3,       # NoRight
    "send-from-kernel-memory": -4,  # BadPointer
    "send-from-null": -4,
    "send-unaligned": -4,
    "send-straddles-window-end": -4,
    "recv-into-code": -4,
    "log-from-kernel-memory": -4,
    "queue-full": -11,             # Full, on the 17th send
}


class ChannelTest(FreshOSTestCase):
    ready_pattern = r"\[probe-chan\] \[test\] done"

    def result(self, case: str) -> int:
        match = self.boot.wait_for_log(rf"\[test\] {case} result=\w+ code=(-?\d+)", timeout=5)
        return int(match.group(1))

    def test_every_refusal_returns_the_right_error(self) -> None:
        for case, code in EXPECTED.items():
            with self.subTest(case=case):
                self.assertEqual(self.result(case), code)

    @unittest.expectedFailure  # Task 7 adds SPAWN; remove this marker then.
    def test_spawn_is_refused_outside_init(self) -> None:
        self.assertEqual(self.result("spawn-not-init"), -8)  # NotPermitted

    def test_refused_sends_queued_nothing(self) -> None:
        # The refused sends above all targeted the sink. If any had queued a
        # message, the queue would fill before 16 legitimate sends succeeded.
        match = self.boot.wait_for_log(r"\[test\] queue-full result=refused code=-11 after=(\d+)")
        self.assertEqual(int(match.group(1)), 16)

    def test_hostile_log_text_is_contained(self) -> None:
        long_line = self.boot.wait_for_log(r"^\[probe-chan\] (L+)…$").group(1)
        self.assertEqual(len(long_line), 256)
        self.boot.wait_for_log(r"^\[probe-chan\] bad\?utf8$")
        self.boot.wait_for_log(r"^\[probe-chan\] fake\?\[init\] spoof$")
        self.assertEqual(self.boot.find_logs(r"^\[init\] spoof"), [])
```

  Until Task 7, `spawn-not-init` returns `-1` (`NoSuchSyscall`). That's why its test carries `@unittest.expectedFailure`, which Task 7 removes.

- [ ] **Step 2: Run them and watch them fail.** Run `./test.sh -k "Latency or Channel or Trace"`. Expected: failures, because nothing exists yet.

- [ ] **Step 3: Create `kernel/src/handles.rs`,** and declare it in `main.rs` under `cfg(aarch64)`:

```rust
/// Per-task handle tables (EL0 isolation spec, section 4). A handle is a slot
/// index; the slot names a channel and what the task may do with it. Tasks
/// never see raw channel numbers.
use freshos_abi::{Error, Handle, MAX_HANDLES, Rights};

#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub channel: u32,
    pub rights: Rights,
}

#[derive(Clone, Copy)]
pub struct HandleTable {
    slots: [Option<Slot>; MAX_HANDLES],
}

impl HandleTable {
    pub const EMPTY: HandleTable = HandleTable { slots: [None; MAX_HANDLES] };

    /// The channel behind `handle`, if it carries `need`.
    pub fn lookup(&self, handle: Handle, need: Rights) -> Result<u32, Error> {
        let slot = self
            .slots
            .get(handle.0 as usize)
            .copied()
            .flatten()
            .ok_or(Error::NoSuchHandle)?;
        if slot.rights.contains(need) {
            Ok(slot.channel)
        } else {
            Err(Error::NoRight)
        }
    }

    pub fn insert(&mut self, slot: Slot) -> Result<Handle, Error> {
        let index = self.slots.iter().position(Option::is_none).ok_or(Error::TableFull)?;
        self.slots[index] = Some(slot);
        Ok(Handle(index as u32))
    }

    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut Slot> {
        self.slots.get_mut(handle.0 as usize).and_then(Option::as_mut)
    }

    /// Every occupied slot, as (slot index, slot).
    pub fn slots(&self) -> impl Iterator<Item = (u32, Slot)> + '_ {
        self.slots.iter().enumerate().filter_map(|(i, s)| s.map(|s| (i as u32, s)))
    }
}
```

- [ ] **Step 4: Change IPC.** In `kernel/src/ipc.rs`:
  1. Queue timestamps alongside messages. Add

```rust
#[derive(Clone, Copy)]
struct Queued {
    msg: Message,
    sent_ns: u64,
}
const EMPTY_QUEUED: Queued = Queued { msg: Message::empty(), sent_ns: 0 };
```

  Change `Channel.buf` to `[Queued; CHANNEL_CAP]`, with `EMPTY_CHANNEL.buf: [EMPTY_QUEUED; CHANNEL_CAP]`. Add a field `receiver: u16` (`0xFFFF` = none) to `Channel` and `EMPTY_CHANNEL`.

  2. One dequeue path, which also records the delivery metric:

```rust
/// Take the oldest message, recording how long it waited to be delivered.
fn dequeue(ch: &mut Channel) -> Option<Message> {
    if ch.count == 0 {
        return None;
    }
    let queued = ch.buf[ch.tail];
    ch.tail = (ch.tail + 1) % CHANNEL_CAP;
    ch.count -= 1;
    let now = crate::arch::time_ns();
    crate::metrics::record_ipc_delivery_ns(now.saturating_sub(queued.sent_ns));
    Some(queued.msg)
}
```

  Use it in `recv` (replacing the inline three-line dequeue) and in `try_recv`.

  3. In `send`: store `Queued { msg: stamped, sent_ns: now_ns }`. Change the receiver attribution to prefer the recorded receiver:

```rust
    let receiver = match ch.waiter {
        Some(w) => w,
        None if ch.receiver != 0xFFFF => ch.receiver as usize,
        None if ch.consumer != 0xFFFF => ch.consumer as usize,
        None => 0xFFFF,
    };
```

  Replace the wake block and its `Ok(())` with:

```rust
    // Wake the waiting receiver. A user task waiting in recv gets the message
    // delivered straight into its buffer and is ready to run; the caller may
    // hand the CPU to it.
    if let Some(task_id) = ch.waiter.take() {
        crate::metrics::note_task_unblocked(task_id, now_ns);
        if crate::arch::context::has_user_wait(task_id) {
            if let Some(message) = dequeue(ch) {
                crate::arch::context::complete_user_recv(task_id, &message);
                return Ok(Some(task_id));
            }
        }
        crate::arch::unblock_task(task_id);
    }
    Ok(None)
```

  Change the signature to `pub fn send(channel_id: u32, msg: &Message) -> Result<Option<usize>, Error>`. Callers that ignore the result (`let _ = ipc::send(…)`) are unaffected.

  4. Add:

```rust
/// Take a message without blocking (for syscalls).
pub fn try_dequeue(channel_id: u32) -> Result<Option<Message>, Error> {
    let ch = channel_mut(channel_id)?;
    ch.consumer = crate::arch::current_task() as u16;
    Ok(dequeue(ch))
}

/// Record `task` as waiting on `channel_id` (it must hold the channel's RECV).
pub fn register_waiter(channel_id: u32, task: usize) -> Result<(), Error> {
    channel_mut(channel_id)?.waiter = Some(task);
    Ok(())
}

/// Forget `task`'s wait on `channel_id` (deadline passed, or the task exited).
pub fn cancel_waiter(channel_id: u32, task: usize) {
    if let Ok(ch) = channel_mut(channel_id) {
        if ch.waiter == Some(task) {
            ch.waiter = None;
        }
    }
}

/// The task holding RECV on `channel_id`, for attribution.
pub fn set_receiver(channel_id: u32, task: usize) {
    if let Ok(ch) = channel_mut(channel_id) {
        ch.receiver = task as u16;
    }
}

fn channel_mut(channel_id: u32) -> Result<&'static mut Channel, Error> {
    let id = channel_id as usize;
    if id >= MAX_CHANNELS {
        return Err(Error::InvalidChannel);
    }
    let ch = unsafe { &mut (*channels())[id] };
    if ch.active { Ok(ch) } else { Err(Error::InvalidChannel) }
}
```

- [ ] **Step 5: Rename the metric.** In `metrics.rs`:
  - rename `IPC_RTT_US_LATEST`/`MAX` to `IPC_DELIVERY_NS_LATEST`/`MAX`;
  - rename `record_ipc_rtt_us` to `record_ipc_delivery_ns`;
  - rename the `Snapshot` field `ipc_rtt_us` to `ipc_delivery_ns`, with its `snapshot()` entry.

  Then update the call sites:
  - `arm_tasks.rs:694`: `draw_metric_value(fb, lx + 80, y, metrics.ipc_delivery_ns.latest / 1000, PANEL_BG);`
  - the two `[metrics]` lines (1237–1244 and 1931–1938): replace `ipc_rtt={}us` with `ipc={}ns`, and the argument with `metrics.ipc_delivery_ns.latest`;
  - `arm_tasks.rs:1996`: `draw_metric_pair_line(&mut surf, 14, y, "IPC deliv", MetricSample { latest: metrics.ipc_delivery_ns.latest / 1000, max: metrics.ipc_delivery_ns.max / 1000 }, DASH_BG);`, importing `crate::metrics::MetricSample`.

  In `mcp.rs`, replace `"ipc_round_trip": sample(snapshot.ipc_rtt_us),` with `"ipc_delivery": { "latest_ns": snapshot.ipc_delivery_ns.latest, "max_ns": snapshot.ipc_delivery_ns.max },`, and update the `metrics` view description to say "IPC delivery time in nanoseconds".

- [ ] **Step 6: Waits and handles in `context.rs`.**

```rust
#[derive(Clone, Copy)]
struct UserWait {
    channel: u32,
    buf: u64,
    deadline_ns: u64, // 0 = none
}
```

  Add `handles: HandleTable` and `wait: Option<UserWait>` to `Task`, and `handles: HandleTable::EMPTY, wait: None` to `EMPTY_TASK`. `spawn_el0` gains a `handles: HandleTable` parameter, stored in the task.

```rust
pub fn current_handles() -> Option<&'static HandleTable> {
    let t = unsafe { &*tasks() };
    let task = &t[CURRENT.load(Ordering::SeqCst)];
    task.space.as_ref().map(|_| &task.handles)
}

/// Block the current task in recv: the frame is saved by switch_away.
pub fn set_user_wait(channel: u32, buf: u64, deadline_ns: u64) {
    let t = unsafe { &mut *tasks() };
    let cur = CURRENT.load(Ordering::SeqCst);
    t[cur].wait = Some(UserWait { channel, buf, deadline_ns });
    t[cur].state = State::Blocked;
}

pub fn has_user_wait(task: usize) -> bool {
    let t = unsafe { &*tasks() };
    task < MAX_TASKS && t[task].wait.is_some()
}

/// Finish `task`'s recv: copy `message` into its buffer (validated when it
/// blocked) and set its result, then make it ready.
pub fn complete_user_recv(task: usize, message: &Message) {
    let t = unsafe { &mut *tasks() };
    let Some(wait) = t[task].wait.take() else { return };
    let result = match t[task].space.as_ref() {
        Some(space) => match super::addrspace::write_user(space, wait.buf, message) {
            Ok(()) => 0i64,
            Err(e) => e as i64,
        },
        None => Error::NotPermitted as i64,
    };
    unsafe { *(t[task].sp as *mut u64) = result as u64 };
    t[task].state = State::Ready;
}

/// Wake every task whose recv deadline has passed, with Timeout.
fn expire_waits(now_ns: u64) {
    let t = unsafe { &mut *tasks() };
    for id in 1..MAX_TASKS {
        let Some(wait) = t[id].wait else { continue };
        if wait.deadline_ns != 0 && now_ns >= wait.deadline_ns {
            crate::ipc::cancel_waiter(wait.channel, id);
            t[id].wait = None;
            unsafe { *(t[id].sp as *mut u64) = Error::Timeout as i64 as u64 };
            t[id].state = State::Ready;
        }
    }
}
```

  Imports: `use freshos_abi::Error;`, `use crate::handles::HandleTable;`, `use crate::ipc::Message;`.
  - In `scheduler_tick_arm`, call `expire_waits(super::time_ns());` before `switch_away`.
  - In `retire_current`, before dropping the space, add `if let Some(wait) = task.wait { crate::ipc::cancel_waiter(wait.channel, cur); }`.

- [ ] **Step 7: SEND, RECV and TRY_RECV in `syscalls.rs`.** Add to `dispatch`:

```rust
        sys::SEND => send(a[0], a[1]),
        sys::RECV => recv(a[0], a[1], a[2], true),
        sys::TRY_RECV => recv(a[0], a[1], 0, false),
```

  and:

```rust
fn channel(handle: u64, need: Rights) -> Result<u32, Error> {
    let handles = context::current_handles().ok_or(Error::NotPermitted)?;
    let handle = u32::try_from(handle).map_err(|_| Error::NoSuchHandle)?;
    handles.lookup(Handle(handle), need)
}

fn send(handle: u64, ptr: u64) -> Outcome {
    let run = || -> Result<Outcome, Error> {
        let channel = channel(handle, Rights::SEND)?;
        let message: Message = read_user(space()?, ptr)?;
        match crate::ipc::send(channel, &message) {
            Ok(Some(woken)) => Ok(Outcome::HandOff(woken)),
            Ok(None) => Ok(Outcome::Return(0)),
            Err(crate::ipc::Error::Full) => Err(Error::Full),
            Err(_) => Err(Error::NoSuchHandle),
        }
    };
    run().unwrap_or_else(err)
}

fn recv(handle: u64, buf: u64, deadline_ns: u64, blocking: bool) -> Outcome {
    let run = || -> Result<Outcome, Error> {
        let channel = channel(handle, Rights::RECV)?;
        let space = space()?;
        // Validate the destination first, so a bad buffer is refused even
        // when a message is waiting.
        check_writable::<Message>(space, buf)?;
        match crate::ipc::try_dequeue(channel).map_err(|_| Error::NoSuchHandle)? {
            Some(message) => {
                write_user(space, buf, &message)?;
                Ok(Outcome::Return(0))
            }
            None if !blocking => Err(Error::WouldBlock),
            None if deadline_ns != 0 && crate::arch::time_ns() >= deadline_ns => Err(Error::Timeout),
            None => {
                crate::ipc::register_waiter(channel, context::current_task())
                    .map_err(|_| Error::NoSuchHandle)?;
                context::set_user_wait(channel, buf, deadline_ns);
                Ok(Outcome::Blocked)
            }
        }
    };
    run().unwrap_or_else(err)
}
```

  Imports: `freshos_abi::{Handle, Message, Rights}` and `crate::arch::addrspace::{check_writable, read_user, write_user}`.

- [ ] **Step 8: `ping`, `pong` and `probe-chan` at EL0.**
  - `userbins/pong/src/main.rs` (Cargo deps: rt only):

```rust
#![no_std]
#![no_main]

use freshos_rt::{Message, Startup, entry, recv, send, tag};

entry!(main);

/// Echoes every PING back as a PONG. Handles: 0 = RECV ping, 1 = SEND pong.
fn main(start: Startup) -> ! {
    let pings = start.handle(0);
    let pongs = start.handle(1);
    loop {
        if let Ok(ping) = recv(pings) {
            let _ = send(pongs, &Message::new(tag::PONG).with_data(0, ping.payload[0]));
        }
    }
}
```

  - `userbins/ping/` is a new package, `freshos-ping`: add it to the workspace members, and change `USERBINS` in `run-arm.sh` to `"init ping pong pulse fault ${EXTRA_ELFS:-}"`.

```rust
#![no_std]
#![no_main]

use freshos_rt::{Error, Message, Startup, entry, log, recv, recv_until, send, tag, time_ns};

entry!(main);

const SAMPLES: usize = 100;

/// Measures ping/pong round trips in batches of 100 and logs each batch's
/// median, then waits a second. Handles: 0 = SEND ping, 1 = RECV pong.
fn main(start: Startup) -> ! {
    let pings = start.handle(0);
    let pongs = start.handle(1);
    loop {
        let mut rtts = [0u64; SAMPLES];
        for rtt in rtts.iter_mut() {
            let sent = time_ns();
            if send(pings, &Message::new(tag::PING).with_data(0, sent)).is_err() {
                continue;
            }
            if let Ok(reply) = recv(pongs) {
                *rtt = time_ns().saturating_sub(reply.payload[0]);
            }
        }
        rtts.sort_unstable();
        log!("rtt_median_ns={} samples={}", rtts[SAMPLES / 2], SAMPLES);
        // Nothing arrives on pongs between batches, so this is a one-second sleep.
        match recv_until(pongs, time_ns() + 1_000_000_000) {
            Ok(_) | Err(Error::Timeout) => {}
            Err(e) => log!("wait failed: {e:?}"),
        }
    }
}
```

  `sort_unstable` on a slice is in `core`, so this needs no allocator.

  - `userbins/probe-chan/` is a new package, `freshos-probe-chan`, test-only. Handles: 0 = SEND on the sink channel, 1 = RECV on its own probe channel.

```rust
#![no_std]
#![no_main]

use freshos_rt::{Error, Handle, Message, Startup, abi, entry, exit, log, send, spawn, try_recv};

entry!(main);

fn report(case: &str, result: Result<(), Error>) {
    match result {
        Ok(()) => log!("[test] {case} result=ok code=0"),
        Err(e) => log!("[test] {case} result=refused code={}", e as i64),
    }
}

/// Raw syscall, for deliberately bad arguments the safe wrappers can't express.
fn raw(nr: u64, a0: u64, a1: u64) -> Result<(), Error> {
    let ret: i64;
    unsafe {
        core::arch::asm!("svc #0", in("x8") nr, inlateout("x0") a0 as i64 => ret, in("x1") a1, in("x2") 0u64, options(nostack));
    }
    if ret < 0 { Err(Error::from_code(ret)) } else { Ok(()) }
}

/// Test-only: every way of using channels and pointers wrongly, each reported.
fn main(start: Startup) -> ! {
    let sink = start.handle(0);
    let own = start.handle(1);
    let message = Message::new(1);
    let msg_ptr = &message as *const Message as u64;

    report("ungranted-send", send(Handle(5), &message));
    report("recv-on-send-only", try_recv(sink).map(|_| ()));
    report("send-from-kernel-memory", raw(abi::sys::SEND, sink.0 as u64, 0x4000_0000));
    report("send-from-null", raw(abi::sys::SEND, sink.0 as u64, 0));
    report("send-unaligned", raw(abi::sys::SEND, sink.0 as u64, msg_ptr + 1));
    report(
        "send-straddles-window-end",
        raw(abi::sys::SEND, sink.0 as u64, abi::USER_BASE + abi::USER_SIZE - 8),
    );
    report("recv-into-code", raw(abi::sys::TRY_RECV, own.0 as u64, main as *const () as u64));
    report("log-from-kernel-memory", raw(abi::sys::LOG, 0x4000_0000, 16));

    // Nobody receives on the sink: 16 sends queue, the 17th is refused. If any
    // refused send above had queued a message, fewer than 16 would succeed.
    let mut sent = 0;
    let mut result = Ok(());
    for _ in 0..17 {
        result = send(sink, &message);
        if result.is_err() {
            break;
        }
        sent += 1;
    }
    match result {
        Ok(()) => log!("[test] queue-full result=ok code=0 after={sent}"),
        Err(e) => log!("[test] queue-full result=refused code={} after={sent}", e as i64),
    }

    report("spawn-not-init", spawn("x", "PONG.ELF", &[], 0).map(|_| ()));

    let long = [b'L'; 300];
    freshos_rt::log(core::str::from_utf8(&long).unwrap_or(""));
    let bad_utf8: &[u8] = b"bad\xffutf8";
    let _ = raw(abi::sys::LOG, bad_utf8.as_ptr() as u64, bad_utf8.len() as u64);
    freshos_rt::log("fake\n[init] spoof");

    log!("[test] done");
    exit()
}
```

- [ ] **Step 9: Spawn them with grants, and retire the built-in probes.**
  1. In `main.rs`, after the four existing `ipc::create()` calls, add:

```rust
    let _ = ipc::create().expect("ch4: probe sink");
    let _ = ipc::create().expect("ch5: probe-chan own");
```

  Delete the `PONG.ELF` load block.
  2. In `init_abi.rs`, give `spawn_image` a `handles: HandleTable` parameter (the existing callers pass `HandleTable::EMPTY`) and pass it to `spawn_el0`. Add:

```rust
fn grants(slots: &[(u32, Rights)]) -> HandleTable {
    let mut table = HandleTable::EMPTY;
    for &(channel, rights) in slots {
        let _ = table.insert(Slot { channel, rights });
    }
    table
}

fn spawn_ping_service() -> Option<usize> {
    let id = spawn_image("PING.ELF", grants(&[(2, Rights::SEND), (3, Rights::RECV)]), 2, 0)?;
    crate::ipc::set_receiver(3, id);
    Some(id)
}

fn spawn_pong_service() -> Option<usize> {
    let id = spawn_image("PONG.ELF", grants(&[(2, Rights::RECV), (3, Rights::SEND)]), 2, 0)?;
    crate::ipc::set_receiver(2, id);
    Some(id)
}

fn spawn_probe_chan() -> Option<usize> {
    let id = spawn_image("PROBECHA.ELF", grants(&[(4, Rights::SEND), (5, Rights::RECV)]), 2, 0)?;
    crate::ipc::set_receiver(5, id);
    Some(id)
}
```

  So `spawn_image(binary, handles, handle_count, arg)` passes `handle_count` as `arg0`, and the existing callers pass `0`. Add a `probe-chan` service entry (id 14, `SERVICE_COUNT` 14, autostart). Delete `service_abi.rs`, its `mod`, and `probe_api_ptr` uses. Delete `arm_tasks::ipc_probe_ping_el1` and `ipc_probe_pong_el1`, and the `CH_IPC_PROBE_*` constants in `arm_tasks.rs`.

- [ ] **Step 10: Build and run everything.** Build both boards: no new warnings. Then:

```bash
EXTRA_ELFS="probe-bad probe-chan" BUILD_ONLY=1 ./run-arm.sh && ./test.sh
```

  Expected: `OK`. The latency test prints the median round trip in nanoseconds. If the median is over 100 µs:
  1. Confirm hand-off is happening: `pong` should run immediately after `ping`'s send, and TTBR0 switches should be the only cost.
  2. Record the number and the investigation in the commit, and raise it with Steve.

  Don't raise the target.

- [ ] **Step 11: Commit.**

```bash
git add kernel lib userbins run-arm.sh Cargo.toml Cargo.lock tests
git rm kernel/src/service_abi.rs
git commit -m "Send messages across address spaces with granted handles and direct hand-off" \
  -m "EL0 tasks now reach channels only through their handle table, so a send or receive on anything not granted is refused. A recv with nothing waiting switches away immediately; a send to a waiting receiver delivers straight into its buffer, through its own page table, and runs it next. That took the ping/pong round trip from ~14.5 ms (Task 1 baseline) to N ns (replace N). The kernel now times every delivery (ipc_delivery), replacing the ping-reported round trip. ping and pong run at EL0 with their built-in copies gone, as does the old function-pointer service ABI. probe-chan (test-only) shows that bad handles, missing rights, bad pointers, a full queue and hostile log text are all refused or contained."
```

---

### Task 7: `init` at EL0, the task registry, and supervision over IPC

**Files:**
- Create: `kernel/src/registry.rs`; `tests/test_supervision.py`, `tests/test_missing_service.py`
- Delete: `kernel/src/init_abi.rs`, `kernel/src/task_names.rs`
- Modify:
  - `kernel/src/ipc.rs` (well-known channels, RECV homes, kernel sends);
  - `kernel/src/syscalls.rs` (CHANNEL_CREATE, SPAWN, log names from the registry);
  - `kernel/src/arch/aarch64/context.rs` (retire: registry, RECV return, TASK_EXITED, init death);
  - `kernel/src/arch/aarch64/exceptions.rs` and `syscalls.rs` (use `ExitReason`);
  - `kernel/src/arm_tasks.rs` (`builtin()`, shell services and restart, flow names);
  - `kernel/src/mcp.rs` (registry-backed views, `frames_free`);
  - `kernel/src/main.rs` (channels, spawn only `init`, remove the old ELF loads);
  - `userbins/init` (rewrite);
  - `tests/harness.py`, `tests/test_boot.py`, `tests/test_channels.py`;
  - the spec (the inbox change).

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `registry::{Name, ServiceRecord, on_spawn, on_exit, name, services, find}`;
  - the well-known channels `ipc::{KBD_EVENTS=0, SHELL_KEYS=1, INIT_INBOX=2}`;
  - `ipc::send_as_kernel(channel, &msg)` and `ipc::receiver_exited(channel, task) -> Option<(usize, u32)>` (the home task and slot);
  - `ipc::set_receiver_with_home(channel, task, home_task, home_slot)`;
  - `context::handles_mut(task) -> &'static mut HandleTable`;
  - `arm_tasks::builtin(name: &[u8]) -> Option<fn() -> !>`;
  - the init log lines `[init] starting services`, `[init] services launched`, `[init] cannot start NAME: ERROR` and `[init] restarting NAME`.

**Design notes, which amend the spec. Record them in the spec in Step 10:**
- **One inbox for `init`.** A task waits on one channel at a time, so task-exit notices (from the kernel, sender 0) and restart requests share `init`'s inbox, channel 2, told apart by tag. The spec's separate control channel is dropped.
- **RECV moves on grant and comes home on exit.** `spawn` moves a granted RECV out of `init`'s slot into the child. When the child exits, the right returns to that slot, with any queued messages intact: the manifesto's "channels buffer during the restart". Asking to grant a RECV that has already moved gives `ReceiverTaken`.
- **Built-ins during the transition.** `init`'s table starts the in-kernel built-ins with the binary name `builtin:<name>`, so the kernel still starts only `init` (decision 0006) and `init` owns all startup policy. Each later spec that moves a built-in out deletes its `builtin:` entry.

- [ ] **Step 1: Write the failing tests.**
  - `tests/test_boot.py`: add `"init"` to `LONG_RUNNING`.
  - `tests/test_channels.py`: remove the `@unittest.expectedFailure` marker from `test_spawn_is_refused_outside_init`.
  - `tests/test_isolation.py`: `init` now logs exits itself. In `test_fault_is_contained_and_restarted`, change both patterns to `r"\[init\] fault exited \(fault\)"`.
  - `tests/test_supervision.py`:

```python
import struct
import time

from harness import Boot, FreshOSTestCase


class SupervisionTest(FreshOSTestCase):
    def test_restarts_are_counted_by_the_kernel(self) -> None:
        self.wait_until(
            lambda: self.services().get("pulse", {}).get("restarts", 0) >= 2,
            timeout=30,
            message="pulse restarted twice",
        )
        self.assertEqual(self.services()["pulse"]["last_exit"], "clean")

    def test_second_receiver_is_refused_by_the_kernel(self) -> None:
        self.boot.wait_for_log(r"\[init\] cannot start probe-dup-recv: ReceiverTaken")
        self.assertTrue(self.services()["pong"]["running"])

    def test_shell_restart_goes_through_init(self) -> None:
        self.boot.wait_for_log(r"\[probe-chan\] \[test\] done", timeout=30)
        self.wait_until(
            lambda: not self.services()["probe-chan"]["running"],
            timeout=10,
            message="probe-chan to finish",
        )
        self.boot.send_keys("restart probe-chan\r")
        self.wait_until(
            lambda: len(self.boot.find_logs(r"\[probe-chan\] \[test\] done")) >= 2,
            timeout=20,
            message="probe-chan to run again",
        )

    def test_crash_loop_does_not_leak_frames(self) -> None:
        def frames_between_fault_runs() -> int:
            # Sample while fault is down, so no instance's frames are counted.
            self.wait_until(lambda: not self.services()["fault"]["running"], timeout=10)
            time.sleep(0.05)
            return self.boot.mcp().view("system")["frames_free"]

        def restarts() -> int:
            return self.services()["fault"]["restarts"]

        self.wait_until(lambda: restarts() >= 2, timeout=30, message="fault restarting")
        before, first = frames_between_fault_runs(), restarts()
        self.wait_until(lambda: restarts() >= first + 5, timeout=60, message="five more restarts")
        after = frames_between_fault_runs()
        self.assertLessEqual(abs(before - after), 2, f"frames {before} -> {after}")


def elf_with_wx_segment() -> bytes:
    """A minimal aarch64 ELF whose only segment is writable and executable."""
    header = struct.pack(
        "<4sBBBBB7xHHIQQQIHHHHHH",
        b"\x7fELF", 2, 1, 1, 0, 0,          # 64-bit, little-endian, version 1
        2, 0xB7, 1,                         # ET_EXEC, EM_AARCH64, version
        0x4_0000_0078, 64, 0,               # entry, phoff, shoff
        0, 64, 56, 1, 64, 0, 0,             # flags, ehsize, phentsize, phnum, shentsize, shnum, shstrndx
    )
    segment = struct.pack(
        "<IIQQQQQQ",
        1, 7,                               # PT_LOAD, flags R|W|X
        0, 0x4_0000_0000, 0x4_0000_0000,    # offset, vaddr, paddr
        0x80, 0x80, 0x1000,                 # filesz, memsz, align
    )
    return (header + segment).ljust(0x80, b"\0")


class MalformedElfTest(FreshOSTestCase):
    boot_options = {"extra_files": {"BADELF.ELF": elf_with_wx_segment()}}

    def test_malformed_elf_is_refused(self) -> None:
        self.boot.wait_for_log(r"refused BADELF\.ELF: segment is writable and executable")
        self.boot.wait_for_log(r"\[init\] cannot start probe-badelf: Invalid")
        self.assertTrue(self.services()["pong"]["running"])
```

  - `tests/test_missing_service.py`:

```python
from harness import FreshOSTestCase


class MissingPongTest(FreshOSTestCase):
    boot_options = {"omit": ("pong",)}

    def test_everything_else_runs_and_pong_is_reported(self) -> None:
        self.boot.wait_for_log(r"\[init\] cannot start pong: NotFound")
        services = self.services()
        self.assertFalse(services.get("pong", {}).get("running", False))
        for name in ("kbd", "comp", "shell", "dash", "ping", "mcp"):
            self.assertTrue(services[name]["running"], name)
```

- [ ] **Step 2: Run them and watch them fail.** Run `./test.sh -k "Supervision or Missing or Malformed or Channel"`. Expected: failures.

- [ ] **Step 3: Create `kernel/src/registry.rs`,** and declare it in `main.rs` in place of `task_names`:

```rust
/// The task registry: the kernel's first-hand record of every task and every
/// service by name (EL0 isolation spec, section 5). The kernel keeps the
/// facts; init keeps the policy. MCP, the flow view and the shell read this,
/// so none of them has to trust init's account.
use core::cell::UnsafeCell;

use freshos_abi::{ExitReason, NAME_LEN};

use crate::arch::IrqGuard;
use crate::arch::context::MAX_TASKS;

const MAX_SERVICES: usize = 32;

#[derive(Clone, Copy)]
pub struct Name {
    bytes: [u8; NAME_LEN],
    len: u8,
}

impl Name {
    pub const EMPTY: Name = Name { bytes: [0; NAME_LEN], len: 0 };

    pub fn new(name: &[u8]) -> Name {
        let len = name.len().min(NAME_LEN);
        let mut bytes = [0; NAME_LEN];
        bytes[..len].copy_from_slice(&name[..len]);
        Name { bytes, len: len as u8 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("?")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Clone, Copy)]
pub struct ServiceRecord {
    pub name: Name,
    pub task: Option<u16>,
    pub starts: u64,
    pub exits: u64,
    pub last_exit: Option<ExitReason>,
}

struct Registry {
    task_names: [Name; MAX_TASKS],
    services: [Option<ServiceRecord>; MAX_SERVICES],
}

struct Cell(UnsafeCell<Registry>);
// SAFETY: every access holds an IrqGuard (one CPU, preemptible tasks).
unsafe impl Sync for Cell {}

static REGISTRY: Cell = Cell(UnsafeCell::new(Registry {
    task_names: [Name::EMPTY; MAX_TASKS],
    services: [None; MAX_SERVICES],
}));

fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let _irq = IrqGuard::mask();
    f(unsafe { &mut *REGISTRY.0.get() })
}

/// A task started as the service `name`.
pub fn on_spawn(task: usize, name: &[u8]) {
    let name = Name::new(name);
    with(|r| {
        if task < MAX_TASKS {
            r.task_names[task] = name;
        }
        let slot = r
            .services
            .iter()
            .position(|s| s.is_some_and(|s| s.name.as_str() == name.as_str()))
            .or_else(|| r.services.iter().position(Option::is_none));
        if let Some(index) = slot {
            let record = r.services[index].get_or_insert(ServiceRecord {
                name,
                task: None,
                starts: 0,
                exits: 0,
                last_exit: None,
            });
            record.task = Some(task as u16);
            record.starts += 1;
        }
    });
}

/// A task ended.
pub fn on_exit(task: usize, reason: ExitReason) {
    with(|r| {
        for record in r.services.iter_mut().flatten() {
            if record.task == Some(task as u16) {
                record.task = None;
                record.exits += 1;
                record.last_exit = Some(reason);
            }
        }
        if task < MAX_TASKS {
            r.task_names[task] = Name::EMPTY;
        }
    });
}

/// The name a task was started as (empty for tasks never named).
pub fn name(task: usize) -> Name {
    with(|r| r.task_names.get(task).copied().unwrap_or(Name::EMPTY))
}

/// A snapshot of every service record.
pub fn services() -> [Option<ServiceRecord>; MAX_SERVICES] {
    with(|r| r.services)
}

pub fn find(name: &str) -> Option<ServiceRecord> {
    services().into_iter().flatten().find(|s| s.name.as_str() == name)
}
```

  `ExitReason` needs `Clone, Copy`; it derives them in Task 2. Make `arch::context::MAX_TASKS` visible, since it's already `pub`.

- [ ] **Step 4: IPC for `init`.** In `ipc.rs`:
  1. Add the well-known channels:

```rust
/// Channels the kernel creates at boot, in this order. Everything else is
/// created by init.
pub const KBD_EVENTS: u32 = 0;
pub const SHELL_KEYS: u32 = 1;
pub const INIT_INBOX: u32 = 2;
```

  2. Add a field `recv_home: Option<(u16, u32)>` to `Channel` (`None` in `EMPTY_CHANNEL`).
  3. Split `send` into a public stamping wrapper and an inner function:

```rust
pub fn send(channel_id: u32, msg: &Message) -> Result<Option<usize>, Error> {
    send_from(crate::arch::current_task() as u16, channel_id, msg)
}

/// Send as the kernel itself (sender 0), e.g. TASK_EXITED.
pub fn send_as_kernel(channel_id: u32, msg: &Message) -> Result<Option<usize>, Error> {
    send_from(0, channel_id, msg)
}
```

  `send_from(sender, …)` holds the old body, with `stamped.sender = sender;` and `from_task: sender` in the trace.

  4. Add:

```rust
/// `task` now holds RECV on `channel_id`, moved from `home_task`'s slot `home_slot`.
pub fn set_receiver_with_home(channel_id: u32, task: usize, home_task: usize, home_slot: u32) {
    if let Ok(ch) = channel_mut(channel_id) {
        ch.receiver = task as u16;
        ch.recv_home = Some((home_task as u16, home_slot));
    }
}

/// The receiver of `channel_id`, if any.
pub fn receiver(channel_id: u32) -> Option<usize> {
    channel_mut(channel_id).ok().and_then(|ch| (ch.receiver != 0xFFFF).then_some(ch.receiver as usize))
}

/// `task`, the receiver of `channel_id`, exited. Queued messages stay; the
/// right goes back to where it came from. Returns that home (task, slot).
pub fn receiver_exited(channel_id: u32, task: usize) -> Option<(usize, u32)> {
    let ch = channel_mut(channel_id).ok()?;
    if ch.receiver != task as u16 {
        return None;
    }
    if ch.waiter == Some(task) {
        ch.waiter = None;
    }
    let (home_task, home_slot) = ch.recv_home.take()?;
    ch.receiver = home_task;
    Some((home_task as usize, home_slot))
}
```

- [ ] **Step 5: Retirement reports to `init`.** In `context.rs`:
  1. Add `static INIT_TASK: AtomicUsize = AtomicUsize::new(0);` and `pub fn set_init_task(id: usize)`.
  2. Add:

```rust
pub fn handles_mut(task: usize) -> &'static mut HandleTable {
    unsafe { &mut (*tasks())[task].handles }
}
```

  3. Add the imports `use freshos_abi::{ExitReason, Handle, Rights};`, then rewrite `retire_current(reason: ExitReason)`, which now takes the enum. Update its callers: `syscalls::exit` passes the enum; `exceptions.rs` passes `ExitReason::Fault`; `terminate_current_with_reason` becomes `terminate_current(reason: ExitReason) -> !`.

```rust
pub fn retire_current(reason: ExitReason) {
    let cur = CURRENT.load(Ordering::SeqCst);
    if cur == 0 {
        return;
    }
    if cur == INIT_TASK.load(Ordering::SeqCst) {
        serial_println!("init exited ({}): nothing supervises the system — halting", reason.as_str());
        loop {
            super::interrupt_disable();
            super::halt();
        }
    }
    let t = unsafe { &mut *tasks() };
    let mut task = core::mem::replace(&mut t[cur], EMPTY_TASK);
    activate(KERNEL_TTBR0.load(Ordering::SeqCst));
    if let Some(wait) = task.wait {
        crate::ipc::cancel_waiter(wait.channel, cur);
    }
    // Receive rights go home; messages queued meanwhile wait for the next receiver.
    for (_, slot) in task.handles.slots() {
        if slot.rights.contains(Rights::RECV) {
            if let Some((home, home_slot)) = crate::ipc::receiver_exited(slot.channel, cur) {
                if let Some(home_slot) = t[home].handles.get_mut(Handle(home_slot)) {
                    home_slot.rights = home_slot.rights.union(Rights::RECV);
                }
            }
        }
    }
    drop(task.space.take());
    queue_pending_free(task.kernel_stack_bottom, task.kernel_stack_pages);
    crate::registry::on_exit(cur, reason);
    let _ = crate::ipc::send_as_kernel(
        crate::ipc::INIT_INBOX,
        &Message::new(freshos_abi::tag::TASK_EXITED)
            .with_data(0, cur as u64)
            .with_data(1, reason as u64),
    );
    COUNT.store(task_count(), Ordering::SeqCst);
}
```

  Also delete the `crate::init_abi::task_exited` call. In `syscalls.rs`, change the log prefix to `crate::registry::name(context::current_task()).as_str()`.

- [ ] **Step 6: CHANNEL_CREATE and SPAWN.** In `syscalls.rs`:

```rust
        sys::CHANNEL_CREATE => channel_create(),
        sys::SPAWN => spawn(a[0]),
```

```rust
fn caller_is_init() -> Result<usize, Error> {
    let cur = context::current_task();
    if cur == context::init_task() { Ok(cur) } else { Err(Error::NotPermitted) }
}

fn channel_create() -> Outcome {
    let run = || -> Result<Outcome, Error> {
        let init = caller_is_init()?;
        let channel = crate::ipc::create().map_err(|_| Error::TableFull)?;
        let handle = context::handles_mut(init)
            .insert(Slot { channel, rights: Rights::SEND.union(Rights::RECV) })?;
        crate::ipc::set_receiver(channel, init);
        Ok(Outcome::Return(handle.0 as i64))
    };
    run().unwrap_or_else(err)
}

fn spawn(request_ptr: u64) -> Outcome {
    let run = || -> Result<Outcome, Error> {
        let init = caller_is_init()?;
        let space = space()?;
        let request: SpawnRequest = read_user(space, request_ptr)?;
        if request.name_len == 0
            || request.name_len > NAME_LEN as u64
            || request.binary_len == 0
            || request.binary_len > MAX_BINARY_NAME as u64
            || request.grants_len > MAX_HANDLES as u64
        {
            return Err(Error::Invalid);
        }
        let mut name = [0u8; NAME_LEN];
        let name = &mut name[..request.name_len as usize];
        copy_from_user(space, request.name_ptr, name)?;
        let mut binary = [0u8; MAX_BINARY_NAME];
        let binary = &mut binary[..request.binary_len as usize];
        copy_from_user(space, request.binary_ptr, binary)?;
        let binary = core::str::from_utf8(binary).map_err(|_| Error::Invalid)?;

        // Built-ins still in the kernel: started by init's policy, run at EL1.
        if let Some(builtin) = binary.strip_prefix("builtin:") {
            let entry = crate::arm_tasks::builtin(builtin.as_bytes()).ok_or(Error::NotFound)?;
            let id = context::spawn(entry);
            crate::registry::on_spawn(id, name);
            return Ok(Outcome::Return(id as i64));
        }

        let image = crate::boot_images::find(binary).ok_or(Error::NotFound)?;
        let count = request.grants_len as usize;
        let mut grants = [Grant { handle: Handle(0), rights: Rights::NONE }; MAX_HANDLES];
        for (i, grant) in grants[..count].iter_mut().enumerate() {
            *grant = read_user(space, request.grants_ptr + (i * core::mem::size_of::<Grant>()) as u64)?;
        }

        // Resolve every grant against init's table before anything changes.
        let init_handles = context::handles_mut(init);
        let mut child = HandleTable::EMPTY;
        for (i, grant) in grants[..count].iter().enumerate() {
            let slot = *init_handles.get_mut(grant.handle).ok_or(Error::NoSuchHandle)?;
            if grant.rights.contains(Rights::RECV) {
                let duplicate = grants[..i].iter().any(|g| {
                    g.rights.contains(Rights::RECV)
                        && init_handles.get_mut(g.handle).map(|s| s.channel) == Some(slot.channel)
                });
                if duplicate || !slot.rights.contains(Rights::RECV) {
                    let taken = crate::ipc::receiver(slot.channel).is_some_and(|r| r != init);
                    return Err(if duplicate || taken { Error::ReceiverTaken } else { Error::NoRight });
                }
            }
            if grant.rights.contains(Rights::SEND) && !slot.rights.contains(Rights::SEND) {
                return Err(Error::NoRight);
            }
            child.insert(Slot { channel: slot.channel, rights: grant.rights })?;
        }

        let id = context::spawn_el0(image, child, count as u64, request.arg).map_err(|e| {
            if let context::SpawnError::BadImage(reason) = e {
                crate::serial::serial_println!("refused {}: {}", binary, reason);
            }
            match e {
                context::SpawnError::NoSlot => Error::TableFull,
                context::SpawnError::OutOfMemory => Error::OutOfMemory,
                context::SpawnError::BadImage(_) => Error::Invalid,
            }
        })?;

        // Only now that the child exists do receive rights move.
        for grant in grants[..count].iter().filter(|g| g.rights.contains(Rights::RECV)) {
            let slot = context::handles_mut(init).get_mut(grant.handle).ok_or(Error::NoSuchHandle)?;
            slot.rights = slot.rights.without(Rights::RECV);
            crate::ipc::set_receiver_with_home(slot.channel, id, init, grant.handle.0);
        }
        crate::registry::on_spawn(id, name);
        Ok(Outcome::Return(id as i64))
    };
    run().unwrap_or_else(err)
}
```

  Imports: `freshos_abi::{Grant, MAX_BINARY_NAME, MAX_HANDLES, NAME_LEN, SpawnRequest}` and `crate::handles::{HandleTable, Slot}`. Add `pub fn init_task() -> usize` to `context.rs`.

- [ ] **Step 7: Kernel-side changes.**
  1. **`arm_tasks.rs`:**
     - Add:

```rust
/// The in-kernel built-ins init may start as "builtin:<name>" until each moves
/// out to its own binary (decision 0006).
pub fn builtin(name: &[u8]) -> Option<fn() -> !> {
    match name {
        b"kbd" => Some(keyboard_el1),
        b"comp" => Some(compositor_el1),
        b"shell" => Some(shell_el1),
        b"dash" => Some(dashboard_el1),
        b"mcp" => Some(crate::mcp::bridge_el1),
        _ => None,
    }
}
```

     - Replace `CH_KBD_EVENTS` and `CH_SHELL_KEYS` with `ipc::KBD_EVENTS` and `ipc::SHELL_KEYS`.
     - In `flow_name_buf`, change `buf: &mut [u8; 8]` to `&mut [u8; 16]`, update its callers' buffers, and use `crate::registry::name(id)`, copying its bytes into `buf`.
     - Shell: replace the `services`/`ps` body with a loop over `crate::registry::services().into_iter().flatten()`, printing `name`, `running`/`stopped`, `task`, `restarts = starts - 1`, `exits` and `last_exit.map(ExitReason::as_str).unwrap_or("-")`. Replace the `restart` body with:

```rust
            let request = ipc::Message::new(freshos_abi::tag::RESTART_REQUEST).with_name(service_name);
            let text = match ipc::send(ipc::INIT_INBOX, &request) {
                Ok(_) => "asked init to restart it",
                Err(_) => "init's inbox is full",
            };
            shell_print_line(surf, left, start_y, max_y, line_h, cy, text, SUBTLE, damage);
```

     - Delete the helpers `shell_service_state_label`, `shell_service_exit_label` and `shell_service_line_color`, which were `init_abi`-typed. Use `GREEN` for running rows, `ORANGE` for rows whose last exit was a fault, and `SUBTLE` otherwise.
  2. **`mcp.rs`:**
     - `system_view` gains `"frames_free": crate::frame_alloc::free_count()`.
     - `services_view` becomes:

```rust
fn services_view() -> Value {
    let services: Vec<Value> = crate::registry::services()
        .into_iter()
        .flatten()
        .map(|s| {
            json!({
                "name": s.name.as_str(),
                "task": s.task,
                "running": s.task.is_some(),
                "restarts": s.starts.saturating_sub(1),
                "exits": s.exits,
                "last_exit": s.last_exit.map(|r| r.as_str()),
            })
        })
        .collect();
    json!(services)
}
```

     - `tasks_view` and `task_json` use `crate::registry::name(id)` (`.is_empty()` / `.as_str()`).
  3. **`main.rs`:**
     - Remove `mod init_abi;` and `mod task_names;`.
     - Replace the six `ipc::create()` calls with:

```rust
    let _ = ipc::create().expect("ch0: kbd events");
    let _ = ipc::create().expect("ch1: shell keys");
    let _ = ipc::create().expect("ch2: init inbox");
```

     - Delete the `INIT.ELF` EL1 load (`loaded_init`), and replace the spawn block with:

```rust
    let Some(init_image) = boot_images::find("INIT.ELF") else {
        serial_println!("INIT.ELF missing from \\EFI\\FreshOS — nothing to run");
        loop {
            arch::interrupt_disable();
            arch::halt();
        }
    };
    let mut init_handles = handles::HandleTable::EMPTY;
    let _ = init_handles.insert(handles::Slot { channel: ipc::INIT_INBOX, rights: freshos_abi::Rights::RECV });
    match arch::context::spawn_el0(init_image, init_handles, 1, 0) {
        Ok(id) => {
            arch::context::set_init_task(id);
            ipc::set_receiver(ipc::INIT_INBOX, id);
            registry::on_spawn(id, b"init");
        }
        Err(err) => {
            serial_println!("INIT.ELF won't start: {:?} — nothing to run", err);
            loop {
                arch::interrupt_disable();
                arch::halt();
            }
        }
    }
```

  4. Delete `kernel/src/init_abi.rs` and `kernel/src/task_names.rs`. Fix any remaining references the compiler reports.

- [ ] **Step 8: Rewrite `init`.** `userbins/init/Cargo.toml` deps: `freshos-rt` only. `userbins/init/src/main.rs`:

```rust
#![no_std]
#![no_main]

//! init: the one place system policy lives. It creates the channels, starts
//! every service with exactly the handles its table grants, and restarts
//! supervised services when the kernel reports they've exited.

use freshos_rt::{
    Error, ExitReason, Grant, Handle, Message, Rights, Startup, channel_create, entry, log,
    recv_until, spawn, tag, time_ns,
};

entry!(main);

// Channels init creates, by index into `channels`.
const PING: usize = 0;
const PONG: usize = 1;
const SINK: usize = 2;
const PROBE: usize = 3;
const CHANNELS: usize = 4;

struct Service {
    name: &'static str,
    binary: &'static str,
    grants: &'static [(usize, Rights)],
    arg: u64,
    restart_after_ms: Option<u64>,
    optional: bool,
}

const fn service(name: &'static str, binary: &'static str) -> Service {
    Service { name, binary, grants: &[], arg: 0, restart_after_ms: None, optional: false }
}

const SEND: Rights = Rights::SEND;
const RECV: Rights = Rights::RECV;

const TABLE: &[Service] = &[
    // In-kernel built-ins, until each moves out (decision 0006).
    service("mcp", "builtin:mcp"),
    service("kbd", "builtin:kbd"),
    service("comp", "builtin:comp"),
    service("shell", "builtin:shell"),
    service("dash", "builtin:dash"),
    // EL0 services.
    Service { grants: &[(PING, RECV), (PONG, SEND)], ..service("pong", "PONG.ELF") },
    Service { grants: &[(PING, SEND), (PONG, RECV)], ..service("ping", "PING.ELF") },
    Service { restart_after_ms: Some(250), ..service("pulse", "PULSE.ELF") },
    Service { restart_after_ms: Some(200), ..service("fault", "FAULT.ELF") },
    // Test-only, present only when a test stages their binaries.
    Service { arg: 0x4000_0000, optional: true, ..service("probe-bad-kernel", "PROBEBAD.ELF") },
    Service { arg: 0x4_2000_0000, optional: true, ..service("probe-bad-unmapped", "PROBEBAD.ELF") },
    Service { arg: 0, optional: true, ..service("probe-bad-code", "PROBEBAD.ELF") },
    Service { arg: 1, optional: true, ..service("probe-bad-stack", "PROBEBAD.ELF") },
    Service { grants: &[(SINK, SEND), (PROBE, RECV)], optional: true, ..service("probe-chan", "PROBECHA.ELF") },
    Service { grants: &[(PING, RECV)], optional: true, ..service("probe-dup-recv", "PROBECHA.ELF") },
    Service { optional: true, ..service("probe-badelf", "BADELF.ELF") },
];

#[derive(Clone, Copy)]
struct Runtime {
    task: Option<u32>,
    restart_at: Option<u64>,
}

fn main(start: Startup) -> ! {
    let inbox = start.handle(0);
    let mut channels = [Handle(0); CHANNELS];
    for channel in channels.iter_mut() {
        match channel_create() {
            Ok(handle) => *channel = handle,
            Err(e) => log!("cannot create a channel: {e:?}"),
        }
    }

    let mut state = [Runtime { task: None, restart_at: None }; TABLE.len()];
    log!("starting services");
    for index in 0..TABLE.len() {
        state[index].task = start_service(index, &channels);
    }
    log!("services launched");

    loop {
        let deadline = state.iter().filter_map(|s| s.restart_at).min().unwrap_or(0);
        match recv_until(inbox, deadline) {
            Ok(message) => handle_message(&message, &mut state, &channels),
            Err(Error::Timeout) => {}
            Err(e) => log!("inbox: {e:?}"),
        }
        let now = time_ns();
        for index in 0..TABLE.len() {
            if state[index].restart_at.is_some_and(|at| now >= at) {
                state[index].restart_at = None;
                log!("restarting {}", TABLE[index].name);
                state[index].task = start_service(index, &channels);
            }
        }
    }
}

fn handle_message(message: &Message, state: &mut [Runtime], channels: &[Handle; CHANNELS]) {
    match message.tag {
        // Only the kernel (sender 0) reports exits.
        tag::TASK_EXITED if message.sender == 0 => {
            let task = message.payload[0] as u32;
            let Some(index) = state.iter().position(|s| s.task == Some(task)) else { return };
            state[index].task = None;
            let reason = ExitReason::from_u64(message.payload[1]).map(|r| r.as_str()).unwrap_or("?");
            log!("{} exited ({})", TABLE[index].name, reason);
            if let Some(ms) = TABLE[index].restart_after_ms {
                state[index].restart_at = Some(time_ns() + ms * 1_000_000);
            }
        }
        tag::RESTART_REQUEST => {
            let (bytes, len) = message.name();
            let name = core::str::from_utf8(&bytes[..len]).unwrap_or("");
            match TABLE.iter().position(|s| s.name == name) {
                None => log!("restart request for unknown service {name}"),
                Some(index) if state[index].task.is_some() => log!("{name} is already running"),
                Some(index) => {
                    log!("restarting {name} (requested)");
                    state[index].task = start_service(index, channels);
                }
            }
        }
        _ => {}
    }
}

fn start_service(index: usize, channels: &[Handle; CHANNELS]) -> Option<u32> {
    let service = &TABLE[index];
    let mut grants = [Grant { handle: Handle(0), rights: Rights::NONE }; 16];
    for (grant, &(channel, rights)) in grants.iter_mut().zip(service.grants) {
        *grant = Grant { handle: channels[channel], rights };
    }
    match spawn(service.name, service.binary, &grants[..service.grants.len()], service.arg) {
        Ok(task) => Some(task),
        Err(Error::NotFound) if service.optional => None,
        Err(e) => {
            log!("cannot start {}: {:?}", service.name, e);
            None
        }
    }
}
```

  `mcp` starts first, so tests can connect as early as possible.

- [ ] **Step 9: Build and run everything.** Build both boards: fix any fallout, with no new warnings. Update `tests/harness.py`: `TEST_ELFS` is unchanged (`probe-bad`, `probe-chan`). Then:

```bash
EXTRA_ELFS="probe-bad probe-chan" BUILD_ONLY=1 ./run-arm.sh && ./test.sh -v
```

  Expected: every test passes, including `SupervisionTest`, `MalformedElfTest`, `MissingPongTest`, `NoInitTest` and all channel subtests.

- [ ] **Step 10: Record the design notes in the spec.** In `docs/plans/2026-09-26-el0-isolation-design.md` section 5:
  - replace the two-channel boot step with the single init inbox;
  - add the RECV move/return rule and the `builtin:` transition to "What changes elsewhere";
  - note that user copies go through physical addresses, so PAN is never lifted.

  These are corrections, so delete the old wording rather than appending to it.

- [ ] **Step 11: Commit.**

```bash
git add -A kernel userbins lib tests docs/plans/2026-09-26-el0-isolation-design.md Cargo.lock
git commit -m "Run init at EL0 and supervise services over IPC" \
  -m "init is now an ordinary EL0 binary holding the whole service table: it creates the channels, starts every service (and, until they move out, the in-kernel built-ins) with exactly the handles its table grants, and restarts supervised services when the kernel reports an exit on its inbox. The kernel starts only init, and keeps a registry of every task and service that MCP, the flow view and the shell read, so none of them trusts init's account. Receive rights move on grant and return to init when the receiver exits, with queued messages intact, and a second receiver is refused. The function-pointer init ABI and task_names are gone. Tests cover restarts, the refused second receiver, a shell-requested restart, a crash loop that leaks no frames, a malformed ELF, and a missing service."
```

---

### Task 8: Remove the old paths, document, and verify

**Files:**
- Modify: `kernel/src/elf.rs`, `kernel/src/arch/aarch64/paging.rs`, `kernel/src/arch/aarch64/context.rs`, `AGENTS.md`, `docs/Where-We-Are.md`, `docs/plans/2026-09-26-el0-isolation-design.md` (status)

- [ ] **Step 1: Delete the dead code** now that every userbin runs at EL0. Build after each deletion:
  - `elf::load_image`, `elf::load_image_into`, `LoadedImage` and `copy_segments`, plus the fields only they used (`min_vaddr`, `image_span`, `max_align` if unused);
  - `paging::grant_user_access`, `make_executable`, `patch_leaf_entry`, `clear_xn_leaf_entry`, `user_ttbr0` and `switch_ttbr0`;
  - `context::spawn_with_arg`, if nothing calls it (built-ins use `spawn`).

  Expected: no errors. Build warnings at or below the Task 1 baseline of 33.

- [ ] **Step 2: Check the warning baselines.**

```bash
rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi 2>&1 | grep -E "generated [0-9]+ warning"
touch kernel/src/main.rs && rustup run nightly cargo clippy --package freshos-kernel --message-format=short 2>&1 | grep -cE "^kernel/"
```

  Expected: build ≤ 33 and clippy ≤ 64. Fix any warning in code this plan added.

- [ ] **Step 3: Update `AGENTS.md`:**
  - **Commands:** `./test.sh` (and `./test.sh -k name`), and the `run-arm.sh` environment overrides.
  - **"How it runs today":** EL0 tasks each have their own address space, and the built-ins are EL1 until moved.
  - **Architecture:**
    - the user window and stack;
    - the ABI and runtime crates;
    - the syscall table;
    - handles, with one receiving process per channel;
    - the registry and `init`'s table;
    - how to add a userbin: package under `userbins/`, add to `USERBINS` in `run-arm.sh` (or `TEST_ELFS` in `tests/harness.py`), add a table entry in `userbins/init/src/main.rs`, and use `freshos-rt` only.
  - Remove every mention of `init_abi`, `service_abi`, `task_names`, the function-pointer ABI and `grant_user_access`.

- [ ] **Step 4: Update the status block in `docs/Where-We-Are.md`:** EL0 isolation done on QEMU, with the measured round trip and the test count. Next: move the keyboard driver, dashboard, MCP bridge, shell and compositor out, each with its own spec. Set the spec's frontmatter `status: implemented`.

- [ ] **Step 5: Run the full suite one last time.** Run `./test.sh -v`. Expected: every test passes.

- [ ] **Step 6: Commit.**

```bash
git add -A kernel AGENTS.md docs
git commit -m "Remove the pre-isolation loading paths and document the EL0 model" \
  -m "With every userbin at EL0, the fixed-bias ELF loader, global permission patching and EL1 spawn-with-argument path have no callers. AGENTS.md now describes the address spaces, syscalls, handles, registry and init table, and how to add a userbin; Where-We-Are records the result and the next step, moving the in-kernel built-ins out one spec at a time."
```

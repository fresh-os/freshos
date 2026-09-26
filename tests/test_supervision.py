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
        def state() -> tuple[bool, int, bool, int]:
            services = self.services()
            fault, pulse = services.get("fault", {}), services.get("pulse", {})
            return (
                fault.get("running", True),
                fault.get("restarts", 0),
                pulse.get("running", False),
                pulse.get("restarts", 0),
            )

        def frames_between_fault_runs() -> int:
            # Sample while fault is down, so no instance's frames are counted,
            # and while pulse is up, so its are always counted. Each is down
            # for only a few hundred ms, so keep only a sample taken with the
            # same state, and the same restart counts, on both sides of it.
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                before = state()
                if before[0] or not before[2]:
                    time.sleep(0.02)
                    continue
                frames = self.boot.mcp().view("system")["frames_free"]
                if state() == before:
                    return frames
            self.fail("no sample with fault down and pulse up")

        def restarts() -> int:
            # mcp starts first, so fault may not be registered yet.
            return self.services().get("fault", {}).get("restarts", 0)

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
